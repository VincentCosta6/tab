//! Outbound TCP connections that reqwest doesn't make (IMAP, the hub's
//! sync socket, the sandbox's egress proxy), through the company proxy when
//! there is one.
//!
//! reqwest already follows `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and
//! `NO_PROXY`; [`connect`] reads the same variables, so every connection tab
//! makes goes the same way. HTTP CONNECT, SOCKS4/4a and SOCKS5 (with a user
//! and password) are spoken by Zed's `proxy_handshake` (Apache-2.0); an
//! `https://` proxy gets TLS first.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use proxy_handshake::{ProxySpec, Target, Url};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// A connection, direct or tunnelled.
pub struct Stream(Box<dyn Io>);

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("net::Stream")
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

/// The proxy for `host`, from the environment: the scheme's variable
/// (`HTTPS_PROXY` for TLS, `HTTP_PROXY` for plain HTTP), then `ALL_PROXY`;
/// none for hosts `NO_PROXY` names, or for this computer.
pub fn proxy_for(host: &str, tls: bool) -> Option<ProxySpec> {
    proxy_from(host, tls, |k| std::env::var(k).ok())
}

fn proxy_from(host: &str, tls: bool, var: impl Fn(&str) -> Option<String>) -> Option<ProxySpec> {
    let get = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| var(n))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return None;
    }
    if let Some(no) = get(&["NO_PROXY", "no_proxy"])
        && proxy_handshake::no_proxy_matches(&no, host)
    {
        return None;
    }
    let scheme: &[&str] = if tls {
        &["HTTPS_PROXY", "https_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy"]
    };
    let raw = get(scheme).or_else(|| get(&["ALL_PROXY", "all_proxy"]))?;
    // `proxy:3128` means `http://proxy:3128`, as curl reads it.
    let raw = if raw.contains("://") {
        raw
    } else {
        format!("http://{raw}")
    };
    match Url::parse(&raw)
        .map_err(|e| e.to_string())
        .and_then(|u| ProxySpec::parse(&u).map_err(|e| e.to_string()))
    {
        Ok(spec) => Some(spec),
        Err(e) => {
            tracing::warn!(error = %e, "ignoring a proxy setting tab can't use");
            None
        }
    }
}

/// Connects to `host:port`; `tls` is whether the protocol inside is TLS,
/// which picks the proxy variable.
pub async fn connect(host: &str, port: u16, tls: bool) -> std::io::Result<Stream> {
    match proxy_for(host, tls) {
        None => Ok(Stream(Box::new(TcpStream::connect((host, port)).await?))),
        Some(spec) => through(&spec, host, port).await,
    }
}

async fn through(spec: &ProxySpec, host: &str, port: u16) -> std::io::Result<Stream> {
    let err = |e: &dyn std::fmt::Display| {
        std::io::Error::other(format!("proxy {}:{}: {e}", spec.host, spec.port))
    };
    let tcp = TcpStream::connect((spec.host.as_str(), spec.port)).await?;
    let target = if spec.remote_dns() {
        Target::Domain(host.to_string(), port)
    } else {
        let addr = tokio::net::lookup_host((host, port))
            .await?
            .next()
            .ok_or_else(|| err(&format!("{host} didn't resolve")))?;
        Target::Address(addr)
    };
    if spec.tls() {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| err(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name =
            rustls::pki_types::ServerName::try_from(spec.host.clone()).map_err(|e| err(&e))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await?;
        let s = proxy_handshake::tokio::establish(tls, spec, &target)
            .await
            .map_err(|e| err(&e))?;
        Ok(Stream(Box::new(s)))
    } else {
        let s = proxy_handshake::tokio::establish(tcp, spec, &target)
            .await
            .map_err(|e| err(&e))?;
        Ok(Stream(Box::new(s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn picks_the_proxy_like_curl() {
        let vars = [
            ("HTTPS_PROXY", "http://corp:3128"),
            ("ALL_PROXY", "socks5h://sock:1080"),
            ("NO_PROXY", ".internal,example.org"),
        ];
        let p = proxy_from("imap.gmail.com", true, env(&vars)).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("corp", 3128));
        let p = proxy_from("example.com", false, env(&vars)).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("sock", 1080));
        assert!(p.remote_dns());
        assert!(proxy_from("mail.internal", true, env(&vars)).is_none());
        assert!(proxy_from("example.org", true, env(&vars)).is_none());
        assert!(proxy_from("127.0.0.1", true, env(&vars)).is_none());
        assert!(proxy_from("x.com", true, env(&[])).is_none());
        let bare = proxy_from("x.com", true, env(&[("https_proxy", "corp:8080")])).unwrap();
        assert_eq!(bare.port, 8080);
    }

    /// A CONNECT proxy that checks the request and then echoes.
    #[tokio::test]
    async fn tunnels_through_an_http_proxy() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let proxy = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = s.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(
                head.starts_with("CONNECT imap.example.com:993 HTTP/1.1\r\n"),
                "{head}"
            );
            s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nhello")
                .await
                .unwrap();
            let n = s.read(&mut buf).await.unwrap();
            s.write_all(&buf[..n]).await.unwrap();
        });
        let spec =
            ProxySpec::parse(&Url::parse(&format!("http://127.0.0.1:{port}")).unwrap()).unwrap();
        let mut s = through(&spec, "imap.example.com", 993).await.unwrap();
        let mut got = [0u8; 5];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        s.write_all(b"ping").await.unwrap();
        let mut echo = [0u8; 4];
        s.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping");
        proxy.await.unwrap();
    }
}
