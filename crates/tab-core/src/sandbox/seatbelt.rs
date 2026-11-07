//! macOS: a Seatbelt profile for `sandbox-exec -p`.
//!
//! The profile allows everything by default and then takes away: writes to
//! protected folders, reads of hidden ones, and network when the policy
//! limits it. In Seatbelt the last matching rule wins, so the order below is
//! the order of precedence.

use std::path::Path;

use super::{Network, Policy};

fn quote(p: &Path) -> String {
    let s = p.display().to_string();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `subpath` matches a folder and everything in it, `literal` one file. A
/// path that doesn't exist yet gets both.
fn filters(paths: &[std::path::PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        if p.as_os_str() == "/" {
            out.push_str(" (subpath \"/\")");
            continue;
        }
        let q = quote(p);
        if p.is_file() {
            out.push_str(&format!(" (literal {q})"));
        } else if p.is_dir() {
            out.push_str(&format!(" (subpath {q})"));
        } else {
            out.push_str(&format!(" (literal {q}) (subpath {q})"));
        }
    }
    out
}

pub(super) fn profile(policy: &Policy, proxy_port: Option<u16>) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");
    if !policy.protect.is_empty() {
        p.push_str(&format!("(deny file-write*{})\n", filters(&policy.protect)));
    }
    if !policy.writable.is_empty() {
        p.push_str(&format!(
            "(allow file-write*{})\n",
            filters(&policy.writable)
        ));
    }
    // Writing to the terminal and /dev/null always works.
    p.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/fd/\"))\n");
    if !policy.hide.is_empty() {
        p.push_str(&format!(
            "(deny file-read* file-write*{})\n",
            filters(&policy.hide)
        ));
    }
    if !policy.reveal.is_empty() {
        p.push_str(&format!("(allow file-read*{})\n", filters(&policy.reveal)));
    }
    match (&policy.network, proxy_port) {
        (Network::Any, _) => {}
        (Network::None, _) | (Network::Domains(_), None) => {
            p.push_str("(deny network-outbound (remote ip))\n");
        }
        (Network::Domains(_), Some(port)) => {
            p.push_str("(deny network-outbound (remote ip))\n");
            p.push_str(&format!(
                "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
            ));
        }
    }
    p
}
