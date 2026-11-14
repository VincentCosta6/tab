//! The person's PATH, for an app started from Finder, the Dock or a desktop
//! launcher.
//!
//! Such an app gets the system's minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`
//! on macOS), not the one their shell sets up, so `claude`, `gh`, `npm` and
//! the rest aren't found, and the agents tab starts can't find them either.
//! [`import`] asks the login shell for its PATH once at startup and puts it,
//! plus the usual install folders, in front of what the app was given.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MARK: &str = "__TAB_PATH__";

/// How long the shell gets to start. A slow shell setup still lets the app
/// open; the usual install folders are added either way.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Sets this process's PATH to the login shell's, then the one it started
/// with, then the usual install folders. Call it first thing in `main`,
/// before any thread starts, since it changes the environment.
pub fn import() {
    if cfg!(windows) {
        return;
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    let shell = login_shell_path();
    let home = crate::sandbox::home();
    let merged = merge(shell.as_deref(), &current, &extra_dirs(home.as_deref()));
    if merged != current {
        // SAFETY: called from `main` before any other thread exists.
        unsafe { std::env::set_var("PATH", &merged) };
    }
}

/// The PATH an interactive login shell sets up, or `None` if it couldn't be
/// read in time.
fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/sh".into()
            }
        });
    let script = format!("printf '\\n{MARK}%s{MARK}\\n' \"$PATH\"");
    let mut child = std::process::Command::new(&shell)
        // Interactive too, since many people set PATH in `.zshrc`/`.bashrc`.
        .args(["-l", "-i", "-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // Keep oh-my-zsh and friends from prompting to update.
        .env("DISABLE_AUTO_UPDATE", "true")
        .env("TAB_RESOLVING_PATH", "1")
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = Vec::new();
    use std::io::Read;
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    parse(&String::from_utf8_lossy(&out)).map(Into::into)
}

/// The PATH between the markers, ignoring whatever the shell's setup printed.
fn parse(out: &str) -> Option<String> {
    let start = out.find(MARK)? + MARK.len();
    let len = out[start..].find(MARK)?;
    let path = out[start..start + len].trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Folders installers commonly use, for when the shell couldn't be asked or
/// doesn't list them.
fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut d: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        for rel in [".local/bin", ".claude/local", ".opencode/bin", ".cargo/bin"] {
            d.push(h.join(rel));
        }
    }
    for p in [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        d.push(PathBuf::from(p));
    }
    d.retain(|p| p.is_dir());
    d
}

/// The shell's folders, then the current ones, then `extra`, each once.
fn merge(
    shell: Option<&std::ffi::OsStr>,
    current: &std::ffi::OsStr,
    extra: &[PathBuf],
) -> OsString {
    let mut seen = std::collections::HashSet::new();
    let dirs: Vec<PathBuf> = shell
        .into_iter()
        .flat_map(std::env::split_paths)
        .chain(std::env::split_paths(current))
        .chain(extra.iter().cloned())
        .filter(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()))
        .collect();
    std::env::join_paths(dirs).unwrap_or_else(|_| current.to_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignores_what_shell_setup_prints() {
        let out = format!("Welcome!\nnvm loaded\n{MARK}/a/bin:/usr/bin{MARK}\nbye\n");
        assert_eq!(parse(&out).as_deref(), Some("/a/bin:/usr/bin"));
        assert_eq!(parse("no markers here"), None);
        assert_eq!(parse(&format!("{MARK}{MARK}")), None);
    }

    #[cfg(unix)]
    #[test]
    fn merge_puts_the_shell_first_and_keeps_each_folder_once() {
        let merged = merge(
            Some("/Users/v/.local/bin:/usr/bin".as_ref()),
            "/usr/bin:/bin".as_ref(),
            &[PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/bin")],
        );
        assert_eq!(
            merged,
            "/Users/v/.local/bin:/usr/bin:/bin:/opt/homebrew/bin"
        );
    }

    #[cfg(unix)]
    #[test]
    fn merge_without_a_shell_adds_the_install_folders() {
        let merged = merge(None, "/usr/bin:/bin".as_ref(), &[PathBuf::from("/x/bin")]);
        assert_eq!(merged, "/usr/bin:/bin:/x/bin");
    }

    #[test]
    fn extra_dirs_lists_only_folders_that_exist() {
        let home = std::env::temp_dir().join(format!("tab-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let d = extra_dirs(Some(&home));
        assert!(d.contains(&home.join(".local/bin")));
        assert!(!d.contains(&home.join(".claude/local")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_login_shells_path() {
        let path = login_shell_path().expect("the shell answers");
        assert!(std::env::split_paths(&path).any(|p| p.join("sh").is_file()));
    }
}
