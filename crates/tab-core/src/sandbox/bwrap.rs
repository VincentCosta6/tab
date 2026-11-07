//! Linux: bubblewrap arguments.
//!
//! The process sees the real filesystem, bound into a new mount namespace.
//! Protected folders are bound again read-only, writable exceptions bound
//! again read-write on top, and hidden ones covered with an empty tmpfs (or
//! `/dev/null` for a file). bubblewrap applies mounts in order, so later
//! arguments win, the same precedence as the Seatbelt profile.

use std::ffi::OsString;
use std::path::Path;

use super::{Network, Policy};

pub(super) fn args(policy: &Policy, cwd: &Path) -> Vec<OsString> {
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
    if policy.network == Network::None {
        a.push("--unshare-net".into());
    }
    a.push("--chdir".into());
    a.push(cwd.into());
    a
}
