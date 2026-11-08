//! macOS: a Seatbelt profile for `sandbox-exec -p`.
//!
//! The profile allows everything by default and then takes away: the
//! contents of confined folders (the home folder) except what's needed,
//! writes to protected folders, reads of hidden ones, and network when the
//! policy limits it. In Seatbelt the last matching rule wins, so the order below is
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

/// A regex matching every path that starts with `prefix`.
fn prefix_regex(prefix: &str) -> String {
    let mut out = String::from("(regex #\"^");
    for c in prefix.chars() {
        if "\\.^$|?*+()[]{}\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str("\")");
    out
}

/// `subpath` matches a folder and everything in it, `literal` one file. A
/// path that doesn't exist yet gets both. A path ending in `*` matches
/// everything that starts with what comes before it.
fn filters(paths: &[std::path::PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        if p.as_os_str() == "/" {
            out.push_str(" (subpath \"/\")");
            continue;
        }
        if let Some(prefix) = p.to_str().and_then(|s| s.strip_suffix('*')) {
            out.push(' ');
            out.push_str(&prefix_regex(prefix));
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

/// Keeps a sandboxed process from reading other processes' arguments and
/// environment through `sysctl` (what `ps -E` uses).
pub(super) const PROCARGS_RULE: &str = "(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))";

pub(super) fn profile(policy: &Policy, proxy_port: Option<u16>, hide_procargs: bool) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");
    if !policy.confine.is_empty() {
        // `file-read-data` covers reading files and listing folders but not
        // `stat`, so paths through a confined folder still resolve.
        p.push_str(&format!(
            "(deny file-read-data file-write*{})\n",
            filters(&policy.confine)
        ));
        let readable: Vec<_> = policy
            .readable
            .iter()
            .chain(&policy.open)
            .chain(&policy.writable)
            .cloned()
            .collect();
        if !readable.is_empty() {
            p.push_str(&format!("(allow file-read*{})\n", filters(&readable)));
        }
        if !policy.open.is_empty() {
            p.push_str(&format!("(allow file-write*{})\n", filters(&policy.open)));
        }
    }
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
    if !policy.reveal_rw.is_empty() {
        p.push_str(&format!(
            "(allow file-read* file-write*{})\n",
            filters(&policy.reveal_rw)
        ));
    }
    // Programs tab runs on the agent's behalf instead (the real `gh`):
    // neither started nor read, so a copy can't be started either.
    for b in &policy.block_exec {
        p.push_str(&format!(
            "(deny process-exec file-read* (literal {}))\n",
            quote(b)
        ));
    }
    // Other processes' environments and arguments (`ps -E`): tab's own
    // holds tokens the scrubbed child doesn't get.
    if hide_procargs {
        p.push_str(PROCARGS_RULE);
        p.push('\n');
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
