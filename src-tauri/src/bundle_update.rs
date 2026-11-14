//! Installs a macOS update inside the existing `tab.app` instead of replacing
//! the bundle folder.
//!
//! The updater plugin swaps the whole `.app` folder: the new one takes the
//! path and the old one goes to a temp folder that is then deleted. The Dock,
//! LaunchServices and app blockers remember an app by that folder (as a file
//! reference, not just a path), so every update unpinned tab from the Dock and
//! showed up in app blockers as a new, temp-named app. Swapping only
//! `Contents` keeps the folder, so all of those keep pointing at tab. The code
//! signature covers `Contents` alone, so the result is the same signed app.
//!
//! Compiled on every platform so its tests run anywhere; only macOS uses it.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io;
use std::path::{Component, Path, PathBuf};

/// Why the update wasn't installed in place.
#[derive(Debug)]
pub enum Error {
    /// Nothing was changed, so another way of installing can still be tried.
    Untouched(io::Error),
    /// The old `Contents` was moved away and couldn't be put back. `kept` is
    /// where it is now.
    Broken { err: io::Error, kept: PathBuf },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Untouched(e) => write!(f, "{e}"),
            Error::Broken { err, kept } => {
                write!(f, "{err}; the previous version is at {}", kept.display())
            }
        }
    }
}

/// The `.app` folder holding `exe` (`X.app/Contents/MacOS/x`), if it's in one.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// Replaces `bundle/Contents` with the `Contents` of the app in `archive`, a
/// `.app.tar.gz` holding one `X.app/` folder, as the release builds make.
pub fn install(bundle: &Path, archive: &[u8]) -> Result<(), Error> {
    let staging = staging_dir(bundle).map_err(Error::Untouched)?;
    let new_app = staging.path().join("new");
    unpack(archive, &new_app).map_err(Error::Untouched)?;
    let new_contents = new_app.join("Contents");
    if !new_contents.join("Info.plist").is_file() {
        return Err(Error::Untouched(io::Error::new(
            io::ErrorKind::InvalidData,
            "the update has no Contents/Info.plist",
        )));
    }

    let contents = bundle.join("Contents");
    let old = staging.path().join("old");
    // Where the file system can (APFS), exchange the two in one step, so the
    // app is never missing its Contents. Otherwise move old out, new in.
    match swap(&new_contents, &contents) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(Error::Untouched(e));
        }
        Err(_) => {
            std::fs::rename(&contents, &old).map_err(Error::Untouched)?;
            if let Err(err) = std::fs::rename(&new_contents, &contents) {
                if std::fs::rename(&old, &contents).is_ok() {
                    return Err(Error::Untouched(err));
                }
                return Err(Error::Broken {
                    err,
                    kept: staging.keep().join("old"),
                });
            }
        }
    }

    // A new modification date tells LaunchServices to read the bundle again.
    let _ = std::fs::File::open(bundle).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    // `staging`, now holding the previous Contents, is deleted on drop.
    Ok(())
}

/// A temp folder on the bundle's volume, since the swap and renames only work
/// within one file system: the system temp folder when it's there, otherwise
/// next to the bundle.
fn staging_dir(bundle: &Path) -> io::Result<tempfile::TempDir> {
    let builder = || {
        let mut b = tempfile::Builder::new();
        b.prefix("tab-update-");
        b
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(bundle)?.dev();
        let tmp = std::env::temp_dir();
        if std::fs::metadata(&tmp).is_ok_and(|m| m.dev() == dev) {
            if let Ok(dir) = builder().tempdir_in(&tmp) {
                return Ok(dir);
            }
        }
    }
    let parent = bundle
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the app has no parent folder"))?;
    builder().tempdir_in(parent)
}

/// Unpacks the single top-level folder of a `.tar.gz` into `to`.
fn unpack(archive: &[u8], to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut parts = path.components();
        parts.next(); // the `X.app` folder itself
        let rel: PathBuf = parts.collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected path in the update: {}", path.display()),
            ));
        }
        // Links can't point out of the bundle (or later entries would be
        // written through them), and hard links aren't expected at all.
        match entry.header().entry_type() {
            tar::EntryType::Link => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected hard link in the update: {}", path.display()),
                ));
            }
            tar::EntryType::Symlink => {
                let target = entry.link_name()?.unwrap_or_default().into_owned();
                if !link_stays_inside(&rel, &target) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "a link in the update points outside it: {} -> {}",
                            path.display(),
                            target.display()
                        ),
                    ));
                }
            }
            _ => {}
        }
        let dest = to.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&dest)?;
    }
    Ok(())
}

/// Whether a link at `rel` (inside the bundle) to `target` stays inside
/// the bundle: relative, and never climbing above its top.
fn link_stays_inside(rel: &Path, target: &Path) -> bool {
    if target.as_os_str().is_empty() {
        return false;
    }
    let mut depth = rel.components().count().saturating_sub(1);
    for c in target.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Exchanges `a` and `b` atomically. Fails with `Unsupported` off macOS and on
/// file systems without the swap.
fn swap(a: &Path, b: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        // SAFETY: both are valid NUL-terminated strings that outlive the call.
        let r = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (a, b);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn links_must_stay_inside_the_bundle() {
        let at = Path::new("Contents/Frameworks/X.framework/Current");
        assert!(link_stays_inside(at, Path::new("Versions/A")));
        assert!(link_stays_inside(at, Path::new("../../MacOS/tab")));
        assert!(link_stays_inside(at, Path::new("../../../Info.plist")));
        assert!(!link_stays_inside(at, Path::new("../../../../outside")));
        assert!(!link_stays_inside(at, Path::new("/etc/passwd")));
        assert!(!link_stays_inside(at, Path::new("")));
    }

    fn archive(files: &[(&str, &str)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            // Written raw: the builder refuses `..`, which a test needs.
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            tar.append(&header, body.as_bytes()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn old_bundle(root: &Path) -> PathBuf {
        let bundle = root.join("tab.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(bundle.join("Contents/Info.plist"), "old").unwrap();
        fs::write(bundle.join("Contents/MacOS/tab"), "old").unwrap();
        fs::write(bundle.join("Contents/MacOS/gone"), "old").unwrap();
        bundle
    }

    #[test]
    fn replaces_contents_and_keeps_the_bundle_folder() {
        let root = tempfile::tempdir().unwrap();
        let bundle = old_bundle(root.path());
        // Lives outside Contents, so it survives only if the folder does.
        fs::write(bundle.join("marker"), "").unwrap();
        #[cfg(unix)]
        let ino = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&bundle).unwrap().ino()
        };

        let update = archive(&[
            ("tab.app/Contents/Info.plist", "new"),
            ("tab.app/Contents/MacOS/tab", "new"),
        ]);
        install(&bundle, &update).unwrap();

        assert!(bundle.join("marker").exists());
        assert_eq!(
            fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(bundle.join("Contents/MacOS/tab")).unwrap(),
            "new"
        );
        assert!(!bundle.join("Contents/MacOS/gone").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            assert_eq!(fs::metadata(&bundle).unwrap().ino(), ino);
            let mode = fs::metadata(bundle.join("Contents/MacOS/tab"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "the binary stays executable");
        }
    }

    #[test]
    fn leaves_the_app_alone_when_the_update_is_bad() {
        let root = tempfile::tempdir().unwrap();
        let bundle = old_bundle(root.path());

        let no_plist = archive(&[("tab.app/Contents/MacOS/tab", "new")]);
        assert!(matches!(
            install(&bundle, &no_plist),
            Err(Error::Untouched(_))
        ));

        let escapes = archive(&[("tab.app/Contents/../../evil", "x")]);
        assert!(matches!(
            install(&bundle, &escapes),
            Err(Error::Untouched(_))
        ));
        assert!(!root.path().join("evil").exists());

        assert!(matches!(
            install(&bundle, b"not a tarball"),
            Err(Error::Untouched(_))
        ));
        assert_eq!(
            fs::read_to_string(bundle.join("Contents/MacOS/tab")).unwrap(),
            "old"
        );
    }

    #[test]
    fn finds_the_bundle_of_an_executable() {
        assert_eq!(
            bundle_of(Path::new("/Applications/tab.app/Contents/MacOS/tab")),
            Some(PathBuf::from("/Applications/tab.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/tab")), None);
        assert_eq!(bundle_of(Path::new("/x/target/release/tab")), None);
    }
}
