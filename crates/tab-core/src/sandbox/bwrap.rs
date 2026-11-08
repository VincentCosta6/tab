//! Linux: bubblewrap arguments.
//!
//! The process sees the real filesystem, bound into a new mount namespace.
//! Protected folders are bound again read-only, writable exceptions bound
//! again read-write on top, and hidden ones covered with an empty tmpfs (or
//! `/dev/null` for a file). bubblewrap applies mounts in order, so later
//! arguments win, the same precedence as the Seatbelt profile.
//!
//! Where the kernel allows it, the process also gets its own PID namespace
//! and `/proc`, so it can't read tab's environment or memory through
//! `/proc/<pid>`. A domain allowlist gets a network namespace of its own,
//! with tab's proxy reachable only through a Unix socket ([`super::bridge`]).

use std::ffi::OsString;
use std::path::Path;

use super::{Network, Policy};

/// What this computer's bubblewrap can do besides the basics.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Extras<'a> {
    /// A PID namespace and a fresh `/proc` work here.
    pub pid: bool,
    /// The folder holding the proxy's Unix socket, for a domain list.
    pub net_dir: Option<&'a Path>,
}

pub(super) fn args(policy: &Policy, cwd: &Path, x: Extras) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["--die-with-parent".into()];
    fn bind(a: &mut Vec<OsString>, flag: &str, p: &Path) {
        a.push(flag.into());
        a.push(p.into());
        a.push(p.into());
    }
    let all_read_only = policy.protect.iter().any(|p| p.as_os_str() == "/");
    bind(
        &mut a,
        if all_read_only { "--ro-bind" } else { "--bind" },
        Path::new("/"),
    );
    bind(&mut a, "--dev-bind", Path::new("/dev"));
    if x.pid {
        a.push("--unshare-pid".into());
        a.push("--proc".into());
        a.push("/proc".into());
    }
    for p in policy
        .protect
        .iter()
        .filter(|p| p.as_os_str() != "/" && p.exists())
    {
        bind(&mut a, "--ro-bind", p);
    }
    for p in policy.writable.iter().filter(|p| p.exists()) {
        if p == Path::new("/dev") {
            continue;
        }
        bind(&mut a, "--bind", p);
    }
    for p in policy.hide.iter().filter(|p| p.exists()) {
        if p.is_dir() {
            a.push("--tmpfs".into());
            a.push(p.into());
        } else {
            a.push("--ro-bind".into());
            a.push("/dev/null".into());
            a.push(p.into());
        }
    }
    for p in policy.reveal.iter().filter(|p| p.exists()) {
        bind(&mut a, "--ro-bind", p);
    }
    // A program covered with /dev/null can't be started.
    for p in policy.block_exec.iter().filter(|p| p.is_file()) {
        a.push("--ro-bind".into());
        a.push("/dev/null".into());
        a.push(p.into());
    }
    match (&policy.network, x.net_dir) {
        (Network::Any, _) => {}
        (Network::None, _) | (Network::Domains(_), None) => a.push("--unshare-net".into()),
        (Network::Domains(_), Some(dir)) => {
            a.push("--unshare-net".into());
            bind(&mut a, "--bind", dir);
        }
    }
    a.push("--chdir".into());
    a.push(cwd.into());
    a
}
