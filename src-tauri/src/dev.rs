//! The dev build's own identity, so it can run beside an installed tab.
//!
//! `scripts/dev.mjs` (`pnpm dev:tab`) builds the app with
//! `src-tauri/tauri.dev.conf.json` merged in, which gives it its own
//! identifier. Tauri derives the data and config folders from the
//! identifier, so the dev build gets its own `tab.db`, `hub.db` and
//! `config.json`. The ports are the one thing shared by the whole machine,
//! so the dev build starts from its own range too.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tab_core::agents::AgentHub;

/// The identifier in `tauri.dev.conf.json`.
pub const DEV_IDENTIFIER: &str = "dev.tab.dev";

/// The MCP port tab uses unless config.json says otherwise.
const MCP_PORT: u16 = 47_821;
/// Where the hub listens for other devices (the next free port up from here).
const HUB_PORT: u16 = 47_850;
/// The dev build's ranges: clear of the installed app's, which counts up
/// from its own when a port is taken.
const DEV_MCP_PORT: u16 = 47_921;
const DEV_HUB_PORT: u16 = 47_950;

/// Ports for the build with this identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub mcp: u16,
    pub hub: u16,
}

impl Ports {
    /// `configured` is the MCP port from config.json. A dev build moves the
    /// default one (a fresh config, or one copied from the installed app)
    /// into its own range and keeps one the person picked.
    pub fn for_build(identifier: &str, configured: u16) -> Self {
        if identifier == DEV_IDENTIFIER {
            Ports {
                mcp: if configured == MCP_PORT {
                    DEV_MCP_PORT
                } else {
                    configured
                },
                hub: DEV_HUB_PORT,
            }
        } else {
            Ports {
                mcp: configured,
                hub: HUB_PORT,
            }
        }
    }
}

/// The identifier of the app the release pipeline builds.
const INSTALLED_IDENTIFIER: &str = "dev.tab.app";

/// The installed app's data and config folders, when this is the dev build,
/// so its sandbox keeps agents out of them too. Both sit next to this
/// build's own, under the identifier.
pub fn installed_app_dirs(
    identifier: &str,
    data_dir: &Path,
    config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if identifier != DEV_IDENTIFIER {
        return Vec::new();
    }
    [Some(data_dir), config_dir]
        .into_iter()
        .flatten()
        .filter_map(|d| d.parent())
        .map(|base| base.join(INSTALLED_IDENTIFIER))
        .collect()
}

/// What the dev runner reads to decide whether a restart would cut off an
/// agent. Written only when it sets `TAB_DEV_STATUS`.
#[derive(Debug, serde::Serialize)]
struct Status {
    pid: u32,
    working: usize,
}

/// Keeps the file named by `TAB_DEV_STATUS` up to date with how many agent
/// turns are running. Does nothing when it isn't set.
pub fn report_status(hub: Arc<AgentHub>) {
    let Some(path) = std::env::var_os("TAB_DEV_STATUS").map(PathBuf::from) else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let mut last = None;
        loop {
            let working = hub.running_count();
            if last != Some(working) {
                match write_status(&path, working) {
                    Ok(()) => last = Some(working),
                    Err(e) => tracing::warn!(error = %e, "couldn't write the dev status file"),
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
}

fn write_status(path: &std::path::Path, working: usize) -> std::io::Result<()> {
    let body = serde_json::to_vec(&Status {
        pid: std::process::id(),
        working,
    })?;
    // Write then rename, so the runner never reads half a file.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_app_keeps_its_ports() {
        assert_eq!(
            Ports::for_build("dev.tab.app", MCP_PORT),
            Ports {
                mcp: MCP_PORT,
                hub: HUB_PORT
            }
        );
        assert_eq!(Ports::for_build("dev.tab.app", 50_000).mcp, 50_000);
    }

    #[test]
    fn the_dev_build_moves_off_the_default_ports() {
        assert_eq!(
            Ports::for_build(DEV_IDENTIFIER, MCP_PORT),
            Ports {
                mcp: DEV_MCP_PORT,
                hub: DEV_HUB_PORT
            }
        );
        // A port the person chose stays.
        assert_eq!(Ports::for_build(DEV_IDENTIFIER, 50_000).mcp, 50_000);
    }

    #[test]
    fn only_the_dev_build_guards_the_installed_app() {
        let data = Path::new("/home/v/.local/share/dev.tab.dev");
        let config = Path::new("/home/v/.config/dev.tab.dev");
        assert_eq!(
            installed_app_dirs(DEV_IDENTIFIER, data, Some(config)),
            vec![
                PathBuf::from("/home/v/.local/share/dev.tab.app"),
                PathBuf::from("/home/v/.config/dev.tab.app"),
            ]
        );
        assert!(installed_app_dirs(INSTALLED_IDENTIFIER, data, Some(config)).is_empty());
    }

    #[test]
    fn the_status_file_is_whole_json() {
        let path = std::env::temp_dir().join(format!("tab-dev-status-{}.json", std::process::id()));
        write_status(&path, 2).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(v["working"], 2);
        assert_eq!(v["pid"], std::process::id());
    }
}
