//! Linux: the way out of a sandbox that has no network of its own.
//!
//! A process with a domain allowlist runs under bubblewrap with
//! `--unshare-net`, so all it can reach is its own loopback. tab's proxy
//! ([`super::proxy`]) also listens on a Unix socket, bound into the
//! sandbox, and this bridge, started inside the sandbox in front of the real
//! program, listens on the sandbox's `127.0.0.1:<port>` and passes each
//! connection to that socket. The program's `HTTPS_PROXY` points at that
//! port, and nothing else leads out.
//!
//! The bridge is tab's own executable started as
//! `tab __tab_bridge <socket> <port> <program> [args...]`; hosts that
//! support it say so with [`set_helper`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The subcommand a host's `main` hands to [`main`].
pub const SUBCOMMAND: &str = "__tab_bridge";

static HELPER: OnceLock<PathBuf> = OnceLock::new();

/// Says this executable runs [`main`] when started with [`SUBCOMMAND`].
pub fn set_helper(exe: PathBuf) {
    let _ = HELPER.set(exe);
}

pub(super) fn helper() -> Option<&'static Path> {
    HELPER.get().map(PathBuf::as_path)
}

/// Runs the bridge, then the program, and returns the program's exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    let mut it = args.into_iter();
    let (Some(sock), Some(port), Some(exe)) = (it.next(), it.next(), it.next()) else {
        eprintln!("usage: {SUBCOMMAND} <socket> <port> <program> [args...]");
        return 2;
    };
    let Some(port) = port.to_str().and_then(|p| p.parse::<u16>().ok()) else {
        eprintln!("{SUBCOMMAND}: bad port");
        return 2;
    };
    let rest: Vec<OsString> = it.collect();
    run(PathBuf::from(sock), port, PathBuf::from(exe), rest)
}

#[cfg(unix)]
fn run(sock: PathBuf, port: u16, exe: PathBuf, args: Vec<OsString>) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{SUBCOMMAND}: {e}");
            return 1;
        }
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{SUBCOMMAND}: can't listen on 127.0.0.1:{port}: {e}");
                return 1;
            }
        };
        tokio::spawn(async move {
            while let Ok((mut inner, _)) = listener.accept().await {
                let sock = sock.clone();
                tokio::spawn(async move {
                    if let Ok(mut out) = tokio::net::UnixStream::connect(&sock).await {
                        let _ = tokio::io::copy_bidirectional(&mut inner, &mut out).await;
                    }
                });
            }
        });
        let status = tokio::process::Command::new(&exe)
            .args(&args)
            .status()
            .await;
        match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("{SUBCOMMAND}: couldn't start {}: {e}", exe.display());
                127
            }
        }
    })
}

#[cfg(not(unix))]
fn run(_sock: PathBuf, _port: u16, _exe: PathBuf, _args: Vec<OsString>) -> i32 {
    eprintln!("{SUBCOMMAND} runs only on Linux");
    1
}
