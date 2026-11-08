//! A small HTTP proxy that only reaches allowed hosts.
//!
//! A process with a domain allowlist gets `HTTPS_PROXY` and friends pointed
//! here. HTTPS goes through `CONNECT host:443` (the proxy never sees inside
//! the TLS), plain HTTP as absolute-form requests. Anything to a host off
//! the list gets a 403 that says why.
//!
//! On macOS the sandbox also blocks every other outbound connection, so the
//! list is enforced. On Linux the process gets a network namespace of its
//! own with nothing in it but loopback, and reaches this proxy through a
//! Unix socket bound into the sandbox ([`super::bridge`]), so the list is
//! enforced there too. In containers, a process that ignores the proxy
//! settings isn't stopped; tab says so in Settings.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A running proxy; it stops when dropped.
pub struct Handle {
    pub port: u16,
    /// Where it also listens, for a sandbox with no network of its own.
    pub socket: Option<std::path::PathBuf>,
    stop: CancellationToken,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(dir) = self.socket.as_ref().and_then(|s| s.parent()) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Whether `host` is on the list. `*.example.com` covers subdomains (not
/// `example.com` itself); `*` covers everything.
pub fn allowed(domains: &[String], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    domains.iter().any(|d| {
        let d = d.trim().to_ascii_lowercase();
        if d == "*" {
            true
        } else if let Some(suffix) = d.strip_prefix('*') {
            suffix.starts_with('.') && host.ends_with(suffix)
        } else {
            host == d
        }
    })
}

pub async fn start(domains: Vec<String>) -> anyhow::Result<Handle> {
    start_on(domains, false).await
}

/// Also listens on a Unix socket in a new folder only this user can open,
/// for a sandbox whose network namespace can't reach the host's loopback.
pub async fn start_on(domains: Vec<String>, unix: bool) -> anyhow::Result<Handle> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let stop = CancellationToken::new();
    let s = stop.clone();
    let domains = std::sync::Arc::new(domains);
    #[cfg(unix)]
    let socket = if unix {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::env::temp_dir().join(format!("tab-net-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let path = super::real(&dir).join("proxy.sock");
        let ul = tokio::net::UnixListener::bind(&path)?;
        let (s, domains) = (stop.clone(), domains.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = s.cancelled() => break,
                    r = ul.accept() => {
                        let Ok((sock, _)) = r else { continue };
                        let domains = domains.clone();
                        let s = s.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = s.cancelled() => {}
                                r = serve(sock, &domains) => {
                                    if let Err(e) = r {
                                        tracing::debug!(error = %e, "proxy connection ended");
                                    }
                                }
                            }
                        });
                    }
                }
            }
        });
        Some(path)
    } else {
        None
    };
    #[cfg(not(unix))]
    let socket = {
        let _ = unix;
        None
    };
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = s.cancelled() => break,
                r = listener.accept() => {
                    let Ok((sock, _)) = r else { continue };
                    let domains = domains.clone();
                    let s = s.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = s.cancelled() => {}
                            r = serve(sock, &domains) => {
                                if let Err(e) = r {
                                    tracing::debug!(error = %e, "proxy connection ended");
                                }
                            }
                        }
                    });
                }
            }
        }
    });
    Ok(Handle { port, socket, stop })
}

/// Points a process's HTTP clients at the proxy.
pub fn point_at(cmd: &mut tokio::process::Command, port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "npm_config_proxy",
        "npm_config_https_proxy",
        "CARGO_HTTP_PROXY",
    ] {
        cmd.env(k, &url);
    }
    for k in ["NO_PROXY", "no_proxy"] {
        cmd.env(k, "localhost,127.0.0.1,::1");
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut sock: S,
    domains: &[String],
) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let n = tokio::time::timeout(Duration::from_secs(30), sock.read(&mut chunk)).await??;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        anyhow::ensure!(buf.len() < 64 * 1024, "request head too large");
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let first = head.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or("HTTP/1.1"),
    );

    let refuse = |host: &str| {
        format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n\
             tab's sandbox blocked {host}: it isn't on this step's network list.\n"
        )
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443);
        if !allowed(domains, &host) {
            sock.write_all(refuse(&host).as_bytes()).await?;
            return Ok(());
        }
        let mut up = TcpStream::connect((host.as_str(), port)).await?;
        sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if buf.len() > head_end {
            up.write_all(&buf[head_end..]).await?;
        }
        tokio::io::copy_bidirectional(&mut sock, &mut up).await?;
        return Ok(());
    }

    // Plain HTTP: `GET http://host[:port]/path HTTP/1.1`.
    let Some(rest) = target.strip_prefix("http://") else {
        sock.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_host_port(authority, 80);
    if !allowed(domains, &host) {
        sock.write_all(refuse(&host).as_bytes()).await?;
        return Ok(());
    }
    let mut up = TcpStream::connect((host.as_str(), port)).await?;
    let rest_of_head = &head[first.len()..];
    up.write_all(format!("{method} {path} {version}{rest_of_head}").as_bytes())
        .await?;
    if buf.len() > head_end {
        up.write_all(&buf[head_end..]).await?;
    }
    tokio::io::copy_bidirectional(&mut sock, &mut up).await?;
    Ok(())
}

fn split_host_port(s: &str, default: u16) -> (String, u16) {
    // [v6]:port
    if let Some(rest) = s.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let host = rest[..end].to_string();
        let port = rest[end + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default);
        return (host, port);
    }
    match s.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), p.parse().unwrap_or(default))
        }
        _ => (s.to_string(), default),
    }
}
