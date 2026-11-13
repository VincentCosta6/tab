//! Where an app build keeps its files, and which ports it listens on.
//!
//! The folders follow Tauri's rules for an identifier, so the GPUI app
//! opens the same `tab.db`, `hub.db` and `config.json` as the Tauri app
//! with the same identifier. Builds with different identifiers keep
//! separate data and ports and can run side by side.

use std::path::PathBuf;

/// The identifier the release pipeline builds.
pub const INSTALLED: &str = "dev.tab.app";
/// The Tauri dev build (`pnpm dev:tab`).
pub const TAURI_DEV: &str = "dev.tab.dev";
/// The GPUI app while it's being built: its own data and ports, so it
/// never opens the installed app's database underneath it.
pub const GPUI_DEV: &str = "dev.tab.gpui";

#[derive(Debug, Clone)]
pub struct Paths {
    pub identifier: String,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
    pub ports: Ports,
}

impl Paths {
    /// The folders Tauri would give an app with this identifier.
    pub fn for_identifier(identifier: &str) -> anyhow::Result<Self> {
        let base = |d: Option<PathBuf>, what: &str| {
            d.map(|d| d.join(identifier))
                .ok_or_else(|| anyhow::anyhow!("no {what} folder on this system"))
        };
        let data_dir = base(dirs::data_dir(), "data")?;
        let config_dir = base(dirs::config_dir(), "config")?;
        let log_dir = if cfg!(target_os = "macos") {
            dirs::home_dir()
                .map(|h| h.join("Library/Logs").join(identifier))
                .ok_or_else(|| anyhow::anyhow!("no home folder"))?
        } else {
            base(dirs::data_local_dir(), "local data")?.join("logs")
        };
        Ok(Paths {
            identifier: identifier.into(),
            data_dir,
            config_dir,
            log_dir,
            ports: Ports::for_identifier(identifier),
        })
    }

    /// Other builds' data and config folders on this computer, so the
    /// sandbox keeps this build's agents out of them.
    pub fn other_app_dirs(&self) -> Vec<PathBuf> {
        [INSTALLED, TAURI_DEV, GPUI_DEV]
            .into_iter()
            .filter(|id| *id != self.identifier)
            .flat_map(|id| {
                [self.data_dir.parent(), self.config_dir.parent()]
                    .into_iter()
                    .flatten()
                    .map(move |base| base.join(id))
            })
            .collect()
    }
}

/// The MCP port tab uses unless config.json says otherwise.
const MCP_PORT: u16 = 47_821;
/// Where the hub listens for other devices (the next free port up).
const HUB_PORT: u16 = 47_850;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    /// This build's default MCP port.
    pub mcp: u16,
    pub hub: u16,
}

impl Ports {
    /// Dev builds start from their own ranges, clear of the installed
    /// app's, which counts up from its own when a port is taken.
    pub fn for_identifier(identifier: &str) -> Self {
        let offset = match identifier {
            TAURI_DEV => 100,
            GPUI_DEV => 200,
            _ => 0,
        };
        Ports {
            mcp: MCP_PORT + offset,
            hub: HUB_PORT + offset,
        }
    }

    /// The MCP port to use: this build's own default in place of the
    /// installed app's (a fresh config, or one copied from it), and one the
    /// person picked as it is.
    pub fn mcp_for(&self, configured: u16) -> u16 {
        if configured == MCP_PORT {
            self.mcp
        } else {
            configured
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_keep_apart() {
        let app = Ports::for_identifier(INSTALLED);
        let tauri = Ports::for_identifier(TAURI_DEV);
        let gpui = Ports::for_identifier(GPUI_DEV);
        assert_eq!(
            app,
            Ports {
                mcp: 47_821,
                hub: 47_850
            }
        );
        // Tauri's dev ports, as src-tauri/src/dev.rs has them.
        assert_eq!(
            tauri,
            Ports {
                mcp: 47_921,
                hub: 47_950
            }
        );
        assert_ne!(gpui.mcp, app.mcp);
        assert_ne!(gpui.hub, tauri.hub);
    }

    #[test]
    fn a_picked_port_stays() {
        let gpui = Ports::for_identifier(GPUI_DEV);
        assert_eq!(gpui.mcp_for(47_821), gpui.mcp);
        assert_eq!(gpui.mcp_for(50_000), 50_000);
    }

    #[test]
    fn folders_end_in_the_identifier() {
        let p = Paths::for_identifier(GPUI_DEV).unwrap();
        assert!(p.data_dir.ends_with(GPUI_DEV));
        assert!(p.config_dir.ends_with(GPUI_DEV));
        let others = p.other_app_dirs();
        assert!(others.iter().any(|d| d.ends_with(INSTALLED)));
        assert!(!others.iter().any(|d| d.ends_with(GPUI_DEV)));
    }
}
