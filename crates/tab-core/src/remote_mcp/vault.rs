//! Where MCP sign-ins are kept, out of `config.json`.
//!
//! - Windows: encrypted with DPAPI (only this Windows account can read it),
//!   one file per connection. Credential Manager caps entries at 2.5 KB,
//!   too small for some access tokens.
//! - macOS: the login keychain.
//! - Elsewhere: a file only this user can read.
//!
//! Values are opaque strings (JSON, see [`super::SignIn`]).

use std::path::PathBuf;

pub struct Vault {
    dir: PathBuf,
}

impl Vault {
    /// `dir` is tab's data folder for MCP sign-ins. Each app (dev build,
    /// installed app) has its own, and on macOS it names the keychain entry.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn get(&self, id: &str) -> Option<String> {
        match imp::get(self, id) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(id, error = %e, "couldn't read an MCP sign-in");
                None
            }
        }
    }

    pub fn set(&self, id: &str, value: &str) -> anyhow::Result<()> {
        imp::set(self, id, value)
    }

    pub fn remove(&self, id: &str) {
        if let Err(e) = imp::remove(self, id) {
            tracing::warn!(id, error = %e, "couldn't remove an MCP sign-in");
        }
    }

    #[allow(dead_code)]
    fn file(&self, id: &str, ext: &str) -> PathBuf {
        self.dir.join(format!("{id}.{ext}"))
    }
}

#[cfg(windows)]
mod imp {
    use super::Vault;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn run(data: &[u8], protect: bool) -> anyhow::Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len())?,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` points at `data` for the call; DPAPI allocates
        // `out`, which is copied and freed with LocalFree below.
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: DPAPI returned `cbData` bytes at `pbData`.
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(v)
    }

    pub fn get(v: &Vault, id: &str) -> anyhow::Result<Option<String>> {
        match std::fs::read(v.file(id, "dpapi")) {
            Ok(bytes) => Ok(Some(String::from_utf8(run(&bytes, false)?)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set(v: &Vault, id: &str, value: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&v.dir)?;
        let sealed = run(value.as_bytes(), true)?;
        let tmp = v.file(id, "tmp");
        std::fs::write(&tmp, sealed)?;
        std::fs::rename(tmp, v.file(id, "dpapi"))?;
        Ok(())
    }

    pub fn remove(v: &Vault, id: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(v.file(id, "dpapi")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Vault;

    const SERVICE: &str = "tab MCP sign-in";

    fn entry(v: &Vault, id: &str) -> anyhow::Result<keyring::Entry> {
        // The data folder tells the dev build's entries from the app's.
        Ok(keyring::Entry::new(
            SERVICE,
            &format!("{}:{id}", v.dir.display()),
        )?)
    }

    pub fn get(v: &Vault, id: &str) -> anyhow::Result<Option<String>> {
        match entry(v, id)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set(v: &Vault, id: &str, value: &str) -> anyhow::Result<()> {
        Ok(entry(v, id)?.set_password(value)?)
    }

    pub fn remove(v: &Vault, id: &str) -> anyhow::Result<()> {
        match entry(v, id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::Vault;

    pub fn get(v: &Vault, id: &str) -> anyhow::Result<Option<String>> {
        match std::fs::read_to_string(v.file(id, "json")) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set(v: &Vault, id: &str, value: &str) -> anyhow::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        dir.mode(0o700);
        dir.create(&v.dir)?;
        let tmp = v.file(id, "tmp");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        o.mode(0o600);
        o.open(&tmp)?.write_all(value.as_bytes())?;
        std::fs::rename(tmp, v.file(id, "json"))?;
        Ok(())
    }

    pub fn remove(v: &Vault, id: &str) -> anyhow::Result<()> {
        match std::fs::remove_file(v.file(id, "json")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_and_forgets_a_sign_in() {
        // macOS would touch the real keychain; the file backends are what CI
        // can check.
        if cfg!(target_os = "macos") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tab-vault-{}", uuid::Uuid::new_v4()));
        let v = Vault::new(dir.clone());
        assert_eq!(v.get("linear"), None);
        v.set("linear", "{\"access_token\":\"a\"}").unwrap();
        assert_eq!(v.get("linear").as_deref(), Some("{\"access_token\":\"a\"}"));
        // Never stored in the clear on Windows.
        if cfg!(windows) {
            let raw = std::fs::read(dir.join("linear.dpapi")).unwrap();
            assert!(!String::from_utf8_lossy(&raw).contains("access_token"));
        }
        v.remove("linear");
        assert_eq!(v.get("linear"), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
