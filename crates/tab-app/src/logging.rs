//! Logs to stderr and to `tab.log` in the app's log folder.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// Sets up `tracing`. Call once, first thing.
pub fn init() {
    use tracing_subscriber::prelude::*;
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tab_core=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(|| LogFile),
        )
        .try_init();
}

/// Writes log lines to `tab.log` once it's open (and nowhere before).
struct LogFile;

impl std::io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match LOG_FILE.get() {
            Some(f) => f.lock().unwrap().write(buf),
            None => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match LOG_FILE.get() {
            Some(f) => f.lock().unwrap().flush(),
            None => Ok(()),
        }
    }
}

/// Keeps a log on disk, so what happened at a launch can be read
/// afterwards. The last launch's goes to `tab.old.log`.
pub fn open_file(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("tab.log");
    let _ = std::fs::rename(&path, dir.join("tab.old.log"));
    match std::fs::File::create(&path) {
        Ok(f) => {
            let _ = LOG_FILE.set(Mutex::new(f));
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "tab starting");
        }
        Err(e) => tracing::warn!(error = %e, "couldn't open the log file"),
    }
}
