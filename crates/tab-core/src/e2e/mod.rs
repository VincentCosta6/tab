//! The end-to-end encrypted vault: a project's most sensitive facts (EIN,
//! tax returns, bank details, IDs), which the hub, tab cloud and cloud jobs
//! can't read. See docs/projects-hub.md, "Vault design".
//!
//! - **Keys.** One random vault key per account. Each device has an X25519
//!   key pair whose private half stays in the OS keychain / DPAPI
//!   ([`crate::vault`]). The vault key is wrapped once per device in
//!   `vault_keys`, and once for the recovery key (24 words, shown once).
//! - **Joining.** A new device publishes its public key and shows a short
//!   code; an open device shows the same code, the person compares them,
//!   and the open device wraps the key for it and vouches for its public
//!   key (a MAC under the vault key), so a key the hub swapped in is never
//!   wrapped for. Lost every device: the recovery key.
//! - **Removing** a device makes a new vault key generation, vouched for by
//!   the one before, wrapped for everyone else, and reseals every entry
//!   and file under it.
//! - **Entries** are `project_resources` rows with access `vault`: their
//!   readable fields are a placeholder title, the project and dates; the
//!   title, note, fields and file name are sealed in `options.sealed`, bound
//!   to the row. Files are sealed before they go into the hub.
//! - **Use.** The vault opens with Touch ID / Windows Hello for 10 minutes
//!   ([`UNLOCK_FOR`]), on this device only. Only a project's coordinator
//!   searches it, and sees values masked unless the person releases them
//!   (`crate::mcp`); other threads ask for fields. Every read is written
//!   to the append-only `vault_audit`. Values released to an agent are
//!   masked again in what's stored and synced ([`redact`]).
//! - **Index.** Built on this device when the vault opens, kept in memory,
//!   and saved sealed under a key derived from the vault key (`vault.db`),
//!   never in `tab.db`.

pub mod crypto;
mod index;
pub mod presence;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::model::ProjectResource;
use crate::store::{Store, VaultAuditRow, VaultDevice, VaultGeneration};
use crypto::{Key, KeyPair, Recovery, Sealed, Wrapped};
pub use presence::Presence;

/// How long the vault stays open after the person unlocks it.
pub const UNLOCK_FOR: Duration = Duration::from_secs(10 * 60);

/// The recovery key's row in `vault_devices`.
pub const RECOVERY: &str = "recovery";

/// Largest file the vault takes.
pub const MAX_FILE: usize = 50 * 1024 * 1024;

/// Where sealed files go: the hub's file store.
#[async_trait::async_trait]
pub trait Files: Send + Sync {
    /// Stores the bytes; returns their hash.
    async fn put(&self, bytes: Vec<u8>) -> anyhow::Result<String>;
    async fn get(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>>;
}

/// What's in an entry, sealed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Body {
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<FileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
}

/// An entry as the person or an agent sees it: values masked.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub title: String,
    pub note: String,
    pub fields: Vec<FieldView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileInfo>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FieldView {
    pub label: String,
    pub masked: String,
    /// Only when released.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// A field to save. `value: None` keeps the one it has (the person edits
/// an entry without seeing its values).
#[derive(Debug, Clone, Deserialize)]
pub struct FieldInput {
    pub label: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntryInput {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub fields: Vec<FieldInput>,
}

/// Where this device stands with the vault.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Nobody has set one up.
    None,
    /// There is one, and this device has no key for it.
    NoAccess,
    /// This device asked to join; an open device has to approve it.
    Asked,
    Locked,
    Open,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub state: String,
    pub code: String,
    pub this: bool,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: State,
    pub open_until: Option<DateTime<Utc>>,
    pub device: DeviceView,
    /// Devices (not the recovery key), newest asks first.
    pub devices: Vec<DeviceView>,
    /// How this device confirms it's the person: `Touch ID`, `Windows Hello`, or none.
    pub presence: Option<&'static str>,
}

/// An audit entry, with its detail opened when the vault is.
#[derive(Debug, Clone, Serialize)]
pub struct AuditView {
    pub id: String,
    pub at: DateTime<Utc>,
    pub device: String,
    pub action: String,
    pub actor: String,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub resource_id: Option<String>,
    pub detail: Option<Value>,
}

/// A search hit for the coordinator: masked.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub fields: Vec<FieldView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The best passage, numbers masked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passage: Option<String>,
}

/// Who read the vault, for the audit log.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    /// `person`, `coordinator` or `thread`.
    pub actor: &'a str,
    pub session_id: Option<&'a str>,
}

impl Reader<'_> {
    pub const PERSON: Reader<'static> = Reader {
        actor: "person",
        session_id: None,
    };
}

struct Opened {
    keys: HashMap<String, Key>,
    current: String,
    until: Instant,
    until_utc: DateTime<Utc>,
    index: index::Index,
}

/// Where this device's private key is kept.
enum DeviceKeys {
    Keychain(crate::vault::Vault),
    #[cfg(test)]
    Memory(Mutex<Option<String>>),
}

impl DeviceKeys {
    fn get(&self) -> anyhow::Result<Option<String>> {
        match self {
            Self::Keychain(v) => v.try_get(DEVICE_KEY),
            #[cfg(test)]
            Self::Memory(m) => Ok(m.lock().unwrap().clone()),
        }
    }

    fn set(&self, secret: &str) -> anyhow::Result<()> {
        match self {
            Self::Keychain(v) => v.set(DEVICE_KEY, secret),
            #[cfg(test)]
            Self::Memory(m) => {
                *m.lock().unwrap() = Some(secret.into());
                Ok(())
            }
        }
    }
}

pub struct E2e {
    store: Arc<Store>,
    keychain: DeviceKeys,
    /// Where `vault.db` (the sealed index) lives.
    dir: PathBuf,
    files: RwLock<Option<Arc<dyn Files>>>,
    presence: Arc<dyn Presence>,
    opened: Mutex<Option<Opened>>,
    device_name: String,
}

/// The keychain entry with this device's private key.
const DEVICE_KEY: &str = "vault-device";
/// The folder the device key is kept in where there's no keychain.
/// Hidden from agents ([`crate::sandbox`]).
pub const DIR: &str = "vault";

fn check_msg(key_id: &str) -> Vec<u8> {
    format!("tab vault check {key_id}").into_bytes()
}
fn device_msg(id: &str, public: &str) -> Vec<u8> {
    format!("tab vault device {id} {public}").into_bytes()
}
fn endorse_msg(key_id: &str, fingerprint: &str) -> Vec<u8> {
    format!("tab vault next {key_id} {fingerprint}").into_bytes()
}
fn prev_aad(key_id: &str) -> Vec<u8> {
    format!("tab vault previous key of {key_id}").into_bytes()
}
fn entry_aad(project_id: &str, id: &str, kind: &str) -> Vec<u8> {
    format!("tab vault entry {project_id} {id} {kind}").into_bytes()
}
fn file_aad(project_id: &str, id: &str) -> Vec<u8> {
    format!("tab vault file {project_id} {id}").into_bytes()
}
fn audit_aad(id: &str) -> Vec<u8> {
    format!("tab vault audit {id}").into_bytes()
}
fn fingerprint(k: &Key) -> String {
    let h = Sha256::digest([b"tab vault fingerprint ".as_slice(), k.bytes()].concat());
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}
fn new_key_id() -> String {
    format!("k-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

fn locked() -> anyhow::Error {
    anyhow::anyhow!("The vault is locked. Unlock it on this device first.")
}

impl E2e {
    /// `dir` is the app's data folder.
    pub fn new(store: Arc<Store>, dir: &Path, presence: Arc<dyn Presence>) -> Arc<Self> {
        let device_name = sysinfo::System::host_name()
            .ok()
            .map(|n| n.trim_end_matches(".local").to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "This device".into());
        Arc::new(Self {
            store,
            keychain: DeviceKeys::Keychain(crate::vault::Vault::named(
                dir.join(DIR),
                "tab vault device key",
            )),
            dir: dir.to_path_buf(),
            files: RwLock::new(None),
            presence,
            opened: Mutex::new(None),
            device_name,
        })
    }

    /// A device whose key is kept in memory, for tests.
    #[cfg(test)]
    pub(crate) fn for_tests(store: Arc<Store>, dir: &Path, name: &str) -> Arc<Self> {
        Arc::new(Self {
            store,
            keychain: DeviceKeys::Memory(Mutex::new(None)),
            dir: dir.to_path_buf(),
            files: RwLock::new(None),
            presence: Arc::new(presence::Yes),
            opened: Mutex::new(None),
            device_name: name.into(),
        })
    }

    /// Where sealed files go, once the hub is up.
    pub fn set_files(&self, files: Arc<dyn Files>) {
        *self.files.write().unwrap() = Some(files);
    }

    fn files(&self) -> anyhow::Result<Arc<dyn Files>> {
        self.files
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the hub isn't running, so vault files can't be read"))
    }

    fn me(&self) -> anyhow::Result<(String, KeyPair)> {
        let id = self.store.device_id()?;
        if let Some(s) = self.keychain.get()? {
            return Ok((id, KeyPair::from_secret(&s)?));
        }
        let kp = KeyPair::generate();
        self.keychain.set(&kp.to_secret())?;
        Ok((id, kp))
    }

    fn presence_name() -> Option<&'static str> {
        if cfg!(target_os = "macos") {
            Some("Touch ID")
        } else if cfg!(windows) {
            Some("Windows Hello")
        } else {
            None
        }
    }

    pub fn is_open(&self) -> bool {
        let mut o = self.opened.lock().unwrap();
        if o.as_ref().is_some_and(|o| o.until <= Instant::now()) {
            *o = None;
        }
        o.is_some()
    }

    /// Runs `f` with the vault open, or says it's locked.
    fn with_open<T>(&self, f: impl FnOnce(&mut Opened) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut o = self.opened.lock().unwrap();
        if o.as_ref().is_some_and(|o| o.until <= Instant::now()) {
            *o = None;
        }
        f(o.as_mut().ok_or_else(locked)?)
    }

    pub fn status(&self) -> anyhow::Result<Status> {
        let (my_id, me) = self.me()?;
        let devices = self.store.vault_devices()?;
        let mine = devices.iter().find(|d| d.id == my_id);
        let has_vault = !self.store.vault_generations()?.is_empty();
        let has_key = !self.store.vault_wraps(&my_id)?.is_empty();
        let open = self.is_open();
        let state = match (has_vault, mine.map(|d| d.state.as_str())) {
            (false, _) => State::None,
            (true, Some("active")) if has_key => {
                if open {
                    State::Open
                } else {
                    State::Locked
                }
            }
            (true, Some("pending")) => State::Asked,
            _ => State::NoAccess,
        };
        let view = |d: &VaultDevice| DeviceView {
            id: d.id.clone(),
            name: d.name.clone(),
            state: d.state.clone(),
            code: crypto::device_code(&d.id, &d.public_key),
            this: d.id == my_id,
            approved_by: d.approved_by.as_ref().map(|a| {
                devices
                    .iter()
                    .find(|x| &x.id == a)
                    .map_or(a.clone(), |x| x.name.clone())
            }),
            created_at: d.created_at,
        };
        let me_view = mine.map(view).unwrap_or_else(|| DeviceView {
            id: my_id.clone(),
            name: self.device_name.clone(),
            state: "none".into(),
            code: crypto::device_code(&my_id, &me.public()),
            this: true,
            approved_by: None,
            created_at: Utc::now(),
        });
        let mut list: Vec<DeviceView> = devices
            .iter()
            .filter(|d| d.id != RECOVERY && d.state != "removed")
            .map(view)
            .collect();
        list.sort_by_key(|d| (d.state != "pending", std::cmp::Reverse(d.created_at)));
        Ok(Status {
            state,
            open_until: self
                .opened
                .lock()
                .unwrap()
                .as_ref()
                .filter(|_| open)
                .map(|o| o.until_utc),
            device: me_view,
            devices: list,
            presence: Self::presence_name(),
        })
    }

    /// Sets the vault up, with this device in it. Returns the recovery
    /// key's 24 words, which are shown once and kept nowhere.
    pub async fn create(&self) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.store.vault_generations()?.is_empty(),
            "there's already a vault; ask a device that has it to let this one in"
        );
        let now = Utc::now();
        let (my_id, me) = self.me()?;
        let vk = Key::random();
        let kid = new_key_id();
        self.store.vault_generation_put(&VaultGeneration {
            id: kid.clone(),
            prev_id: None,
            endorsement: None,
            prev_sealed: None,
            check_mac: vk.vouch(&check_msg(&kid)),
            created_by: my_id.clone(),
            created_at: now,
        })?;
        let recovery = Recovery::generate();
        let rp = recovery.key_pair();
        for (id, name, public) in [
            (my_id.clone(), self.device_name.clone(), me.public()),
            (
                RECOVERY.to_string(),
                "Recovery key".to_string(),
                rp.public(),
            ),
        ] {
            self.admit(&vk, &kid, &id, &name, &public, &my_id, now)?;
        }
        self.store.vault_trust(&kid, &fingerprint(&vk), now)?;
        self.set_open(HashMap::from([(kid.clone(), vk)]), kid);
        self.audit("create", &Reader::PERSON, None, None, None)?;
        self.reindex().await;
        Ok(recovery.words())
    }

    /// Wraps the vault key for a device and vouches for its public key.
    #[allow(clippy::too_many_arguments)]
    fn admit(
        &self,
        vk: &Key,
        kid: &str,
        id: &str,
        name: &str,
        public: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let w = crypto::wrap(vk, kid, id, public)?;
        self.store
            .vault_wrap_put(kid, id, &serde_json::to_value(w)?, now)?;
        let created_at = self
            .store
            .vault_device(id)?
            .map(|d| d.created_at)
            .unwrap_or(now);
        self.store.vault_device_put(&VaultDevice {
            id: id.into(),
            name: name.into(),
            public_key: public.into(),
            state: "active".into(),
            vouch: Some(json!({ "key_id": kid, "mac": vk.vouch(&device_msg(id, public)) })),
            approved_by: Some(by.into()),
            created_at,
            updated_at: now,
        })
    }

    fn set_open(&self, keys: HashMap<String, Key>, current: String) {
        let index = index::Index::load(&self.dir, &keys).unwrap_or_default();
        *self.opened.lock().unwrap() = Some(Opened {
            keys,
            current,
            until: Instant::now() + UNLOCK_FOR,
            until_utc: Utc::now() + chrono::Duration::from_std(UNLOCK_FOR).unwrap(),
            index,
        });
    }

    /// Opens the vault for [`UNLOCK_FOR`], once the person confirms it's
    /// them (Touch ID / Windows Hello).
    pub async fn unlock(&self) -> anyhow::Result<()> {
        if self.is_open() {
            return Ok(());
        }
        let (my_id, me) = self.me()?;
        anyhow::ensure!(
            !self.store.vault_wraps(&my_id)?.is_empty(),
            "this device has no key to the vault yet"
        );
        self.presence.confirm("open the vault").await?;
        let (keys, current) = self.open_keys(&my_id, &me)?;
        self.set_open(keys, current);
        self.audit("unlock", &Reader::PERSON, None, None, None)?;
        self.reindex().await;
        Ok(())
    }

    pub fn lock(&self) {
        *self.opened.lock().unwrap() = None;
    }

    /// Unwraps the newest generation wrapped for `me` that checks out,
    /// and every one before it. Each must be vouched for by the one before,
    /// and the chain must reach one this device trusted already (if it
    /// trusted any), so a key the hub made up is passed over.
    fn open_keys(
        &self,
        my_id: &str,
        me: &KeyPair,
    ) -> anyhow::Result<(HashMap<String, Key>, String)> {
        let gens = self.store.vault_generations()?;
        let wraps: HashMap<String, Value> = self.store.vault_wraps(my_id)?.into_iter().collect();
        let mut why = anyhow::anyhow!("this device has no key to the vault");
        for g in gens.iter().rev().filter(|g| wraps.contains_key(&g.id)) {
            match self.chain(&gens, g, &wraps[&g.id], my_id, me) {
                Ok(ring) => {
                    let now = Utc::now();
                    for (id, k) in &ring {
                        self.store.vault_trust(id, &fingerprint(k), now)?;
                    }
                    return Ok((ring, g.id.clone()));
                }
                Err(e) => {
                    tracing::warn!(key = g.id, error = %e, "passing over a vault key");
                    why = e;
                }
            }
        }
        Err(why)
    }

    fn chain(
        &self,
        gens: &[VaultGeneration],
        newest: &VaultGeneration,
        wrapped: &Value,
        my_id: &str,
        me: &KeyPair,
    ) -> anyhow::Result<HashMap<String, Key>> {
        let by_id: HashMap<&str, &VaultGeneration> =
            gens.iter().map(|g| (g.id.as_str(), g)).collect();
        let w: Wrapped = serde_json::from_value(wrapped.clone())?;
        let key = crypto::unwrap(me, &newest.id, my_id, &w)?;
        anyhow::ensure!(
            key.vouches(&check_msg(&newest.id), &newest.check_mac),
            "the vault key didn't check out"
        );
        let mut trusted =
            self.store.vault_trusted(&newest.id)?.as_deref() == Some(&fingerprint(&key));
        let mut ring = HashMap::from([(newest.id.clone(), key)]);
        let mut cur = newest;
        while let Some(prev_id) = cur.prev_id.as_deref() {
            let Some(prev) = by_id.get(prev_id) else {
                break;
            };
            let k = &ring[&cur.id];
            let sealed: Sealed =
                serde_json::from_value(cur.prev_sealed.clone().ok_or_else(|| {
                    anyhow::anyhow!("a vault key generation is missing its predecessor")
                })?)?;
            let mut b = crypto::open(k, &prev_aad(&cur.id), &sealed)?;
            let pk = Key::from_bytes(
                b.as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("a vault key is 32 bytes"))?,
            );
            zeroize::Zeroize::zeroize(&mut b);
            anyhow::ensure!(
                cur.endorsement
                    .as_deref()
                    .is_some_and(|e| pk.vouches(&endorse_msg(&cur.id, &fingerprint(k)), e)),
                "a newer vault key wasn't vouched for by the one before it"
            );
            anyhow::ensure!(
                pk.vouches(&check_msg(prev_id), &prev.check_mac),
                "an older vault key didn't check out"
            );
            trusted |= self.store.vault_trusted(prev_id)?.as_deref() == Some(&fingerprint(&pk));
            ring.insert(prev_id.to_string(), pk);
            cur = prev;
        }
        anyhow::ensure!(
            trusted || !self.store.vault_trust_any()?,
            "the hub offered a vault key this device can't trace back to one it had"
        );
        Ok(ring)
    }

    /// Asks to join the vault: publishes this device's public key. Returns
    /// the code to compare on the device that approves it.
    pub fn ask(&self) -> anyhow::Result<String> {
        let (my_id, me) = self.me()?;
        anyhow::ensure!(
            !self.store.vault_generations()?.is_empty(),
            "nobody has set up a vault yet"
        );
        let now = Utc::now();
        self.store.vault_device_put(&VaultDevice {
            id: my_id.clone(),
            name: self.device_name.clone(),
            public_key: me.public(),
            state: "pending".into(),
            vouch: None,
            approved_by: None,
            created_at: now,
            updated_at: now,
        })?;
        self.audit("device_ask", &Reader::PERSON, None, None, None)?;
        Ok(crypto::device_code(&my_id, &me.public()))
    }

    /// Lets a device that asked in. Its code was compared by the person.
    pub fn approve(&self, device_id: &str) -> anyhow::Result<()> {
        let d = self
            .store
            .vault_device(device_id)?
            .filter(|d| d.state == "pending")
            .ok_or_else(|| anyhow::anyhow!("that device isn't asking to join"))?;
        let (my_id, _) = self.me()?;
        let now = Utc::now();
        self.with_open(|o| {
            let vk = &o.keys[&o.current];
            self.admit(vk, &o.current, &d.id, &d.name, &d.public_key, &my_id, now)
        })?;
        self.audit(
            "device_approve",
            &Reader::PERSON,
            None,
            None,
            Some(json!({ "device": d.name })),
        )
    }

    /// Turns a device's ask down.
    pub fn deny(&self, device_id: &str) -> anyhow::Result<()> {
        let mut d = self
            .store
            .vault_device(device_id)?
            .filter(|d| d.state == "pending")
            .ok_or_else(|| anyhow::anyhow!("that device isn't asking to join"))?;
        d.state = "removed".into();
        d.updated_at = Utc::now();
        self.store.vault_device_put(&d)
    }

    /// Takes a device out of the vault and makes a new vault key the
    /// others get, so it can't read anything written from now on.
    pub async fn remove(&self, device_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(device_id != RECOVERY, "the recovery key can't be removed");
        let mut d = self
            .store
            .vault_device(device_id)?
            .ok_or_else(|| anyhow::anyhow!("no such device"))?;
        anyhow::ensure!(self.is_open(), locked());
        d.state = "removed".into();
        d.vouch = None;
        d.updated_at = Utc::now();
        self.store.vault_device_put(&d)?;
        self.store.vault_wraps_delete(device_id)?;
        self.audit(
            "device_remove",
            &Reader::PERSON,
            None,
            None,
            Some(json!({ "device": d.name })),
        )?;
        self.rotate().await?;
        if device_id == self.store.device_id()? {
            self.lock();
        }
        Ok(())
    }

    /// A new vault key generation for every active device, then every
    /// entry and file sealed again under it.
    pub async fn rotate(&self) -> anyhow::Result<()> {
        let (my_id, _) = self.me()?;
        let now = Utc::now();
        let vk2 = Key::random();
        let kid2 = new_key_id();
        let devices = self.store.vault_devices()?;
        self.with_open(|o| {
            let old = &o.keys[&o.current];
            for d in devices.iter().filter(|d| d.state == "active") {
                // Only keys someone holding the vault key vouched for.
                let vouched = d.vouch.as_ref().is_some_and(|v| {
                    let mac = v["mac"].as_str().unwrap_or_default();
                    v["key_id"]
                        .as_str()
                        .and_then(|k| o.keys.get(k))
                        .is_some_and(|k| k.vouches(&device_msg(&d.id, &d.public_key), mac))
                });
                if !vouched {
                    tracing::warn!(device = d.id, "skipping a vault device nobody vouched for");
                    continue;
                }
                self.admit(
                    &vk2,
                    &kid2,
                    &d.id,
                    &d.name,
                    &d.public_key,
                    d.approved_by.as_deref().unwrap_or(&my_id),
                    now,
                )?;
            }
            self.store.vault_generation_put(&VaultGeneration {
                id: kid2.clone(),
                prev_id: Some(o.current.clone()),
                endorsement: Some(old.vouch(&endorse_msg(&kid2, &fingerprint(&vk2)))),
                prev_sealed: Some(serde_json::to_value(crypto::seal(
                    &vk2,
                    &kid2,
                    &prev_aad(&kid2),
                    old.bytes(),
                ))?),
                check_mac: vk2.vouch(&check_msg(&kid2)),
                created_by: my_id.clone(),
                created_at: now,
            })?;
            self.store.vault_trust(&kid2, &fingerprint(&vk2), now)?;
            o.keys.insert(kid2.clone(), vk2);
            o.current = kid2.clone();
            Ok(())
        })?;
        // Everything sealed again under the new key.
        for r in self.store.vault_entries(None)? {
            let body = self.open_entry(&r)?;
            let url = match (&r.url, r.kind.as_str()) {
                (Some(hash), "file") => {
                    let bytes = self.file_plain(&r.project_id, &r.id, hash).await?;
                    Some(self.put_file(&r.project_id, &r.id, &bytes).await?)
                }
                _ => r.url.clone(),
            };
            self.write_entry(
                &r.project_id,
                &r.id,
                &r.kind,
                url.as_deref(),
                &body,
                Utc::now(),
            )?;
        }
        self.audit("rotate", &Reader::PERSON, None, None, None)
    }

    /// Lets this device in with the recovery key's words, and opens it.
    pub async fn recover(&self, words: &str) -> anyhow::Result<()> {
        let rp = Recovery::parse(words)?.key_pair();
        let (keys, current) = self.open_keys(RECOVERY, &rp)?;
        let (my_id, me) = self.me()?;
        let now = Utc::now();
        self.admit(
            &keys[&current],
            &current,
            &my_id,
            &self.device_name,
            &me.public(),
            RECOVERY,
            now,
        )?;
        self.set_open(keys, current);
        self.audit("recover", &Reader::PERSON, None, None, None)?;
        self.reindex().await;
        Ok(())
    }

    fn open_entry(&self, r: &ProjectResource) -> anyhow::Result<Body> {
        let sealed: Sealed = serde_json::from_value(r.options["sealed"].clone())
            .map_err(|_| anyhow::anyhow!("this vault entry isn't sealed"))?;
        self.with_open(|o| {
            let k = o.keys.get(&sealed.key_id).ok_or_else(|| {
                anyhow::anyhow!("this entry is sealed under a key this device doesn't have")
            })?;
            let plain = crypto::open(k, &entry_aad(&r.project_id, &r.id, &r.kind), &sealed)?;
            Ok(serde_json::from_slice(&plain)?)
        })
    }

    fn write_entry(
        &self,
        project_id: &str,
        id: &str,
        kind: &str,
        url: Option<&str>,
        body: &Body,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let sealed = self.with_open(|o| {
            Ok(crypto::seal(
                &o.keys[&o.current],
                &o.current,
                &entry_aad(project_id, id, kind),
                &serde_json::to_vec(body)?,
            ))
        })?;
        self.store.vault_entry_put(
            project_id,
            id,
            kind,
            url,
            &serde_json::to_value(sealed)?,
            now,
        )
    }

    async fn put_file(&self, project_id: &str, id: &str, plain: &[u8]) -> anyhow::Result<String> {
        let sealed = self.with_open(|o| {
            Ok(crypto::seal_bytes(
                &o.keys[&o.current],
                &file_aad(project_id, id),
                plain,
            ))
        })?;
        self.files()?.put(sealed).await
    }

    /// A vault file's bytes, opened. The sealed copy's first bytes don't
    /// say which key; every key in the ring is tried.
    async fn file_plain(&self, project_id: &str, id: &str, hash: &str) -> anyhow::Result<Vec<u8>> {
        let sealed = self
            .files()?
            .get(hash)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the hub doesn't have this file any more"))?;
        self.with_open(|o| {
            let aad = file_aad(project_id, id);
            let mut keys: Vec<&String> = o.keys.keys().collect();
            keys.sort_by_key(|k| *k != &o.current);
            keys.into_iter()
                .find_map(|k| crypto::open_bytes(&o.keys[k], &aad, &sealed).ok())
                .ok_or_else(|| anyhow::anyhow!("couldn't decrypt the file"))
        })
    }

    fn view(&self, r: &ProjectResource, body: &Body, reveal: &[String]) -> Entry {
        Entry {
            id: r.id.clone(),
            project_id: r.project_id.clone(),
            kind: r.kind.clone(),
            title: body.title.clone(),
            note: body.note.clone(),
            fields: body
                .fields
                .iter()
                .map(|f| FieldView {
                    label: f.label.clone(),
                    masked: crypto::mask(&f.value),
                    value: reveal
                        .iter()
                        .any(|l| l.eq_ignore_ascii_case(&f.label))
                        .then(|| f.value.clone()),
                })
                .collect(),
            file: body.file.clone(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }

    /// A project's entries, values masked.
    pub fn entries(&self, project_id: &str) -> anyhow::Result<Vec<Entry>> {
        anyhow::ensure!(self.is_open(), locked());
        let mut out = Vec::new();
        for r in self.store.vault_entries(Some(project_id))? {
            match self.open_entry(&r) {
                Ok(b) => out.push(self.view(&r, &b, &[])),
                Err(e) => tracing::warn!(id = r.id, error = %e, "couldn't open a vault entry"),
            }
        }
        out.sort_by_key(|e| e.title.to_lowercase());
        Ok(out)
    }

    fn entry_resource(&self, project_id: &str, id: &str) -> anyhow::Result<ProjectResource> {
        self.store
            .vault_entries(Some(project_id))?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("no vault entry {id} in this project"))
    }

    /// Adds or edits an entry of fields and a note.
    pub async fn save(&self, project_id: &str, input: EntryInput) -> anyhow::Result<Entry> {
        let title = input.title.trim();
        anyhow::ensure!(!title.is_empty(), "a vault entry needs a name");
        let (id, kind, url, old) = match &input.id {
            Some(id) => {
                let r = self.entry_resource(project_id, id)?;
                let b = self.open_entry(&r)?;
                (r.id, r.kind, r.url, b)
            }
            None => (
                uuid::Uuid::new_v4().to_string(),
                "note".to_string(),
                None,
                Body::default(),
            ),
        };
        let mut fields = Vec::new();
        for f in &input.fields {
            let label = f.label.trim();
            if label.is_empty() {
                continue;
            }
            let value = match &f.value {
                Some(v) => v.trim().to_string(),
                None => old
                    .fields
                    .iter()
                    .find(|o| o.label.eq_ignore_ascii_case(label))
                    .map(|o| o.value.clone())
                    .unwrap_or_default(),
            };
            fields.push(Field {
                label: label.into(),
                value,
            });
        }
        let body = Body {
            title: title.into(),
            note: input.note.trim().into(),
            fields,
            file: old.file,
        };
        let now = Utc::now();
        self.write_entry(project_id, &id, &kind, url.as_deref(), &body, now)?;
        self.audit(
            if input.id.is_some() { "edit" } else { "add" },
            &Reader::PERSON,
            Some(project_id),
            Some(&id),
            Some(json!({ "title": body.title })),
        )?;
        self.reindex().await;
        let r = self.entry_resource(project_id, &id)?;
        Ok(self.view(&r, &body, &[]))
    }

    /// Adds a file: sealed here, then put in the hub.
    pub async fn add_file(
        &self,
        project_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> anyhow::Result<Entry> {
        anyhow::ensure!(bytes.len() <= MAX_FILE, "that file is over 50 MB");
        anyhow::ensure!(self.is_open(), locked());
        let id = uuid::Uuid::new_v4().to_string();
        let hash = self.put_file(project_id, &id, bytes).await?;
        let body = Body {
            title: name.into(),
            file: Some(FileInfo {
                name: name.into(),
                size: bytes.len() as u64,
            }),
            ..Default::default()
        };
        self.write_entry(project_id, &id, "file", Some(&hash), &body, Utc::now())?;
        self.audit(
            "add",
            &Reader::PERSON,
            Some(project_id),
            Some(&id),
            Some(json!({ "title": name })),
        )?;
        self.reindex().await;
        let r = self.entry_resource(project_id, &id)?;
        Ok(self.view(&r, &body, &[]))
    }

    pub async fn delete(&self, project_id: &str, id: &str) -> anyhow::Result<()> {
        let r = self.entry_resource(project_id, id)?;
        let title = self.open_entry(&r).map(|b| b.title).unwrap_or_default();
        self.store.resource_delete(project_id, id)?;
        self.audit(
            "delete",
            &Reader::PERSON,
            Some(project_id),
            Some(id),
            Some(json!({ "title": title })),
        )?;
        self.reindex().await;
        Ok(())
    }

    /// One field's value, for the person (the eye button).
    pub fn reveal(&self, project_id: &str, id: &str, label: &str) -> anyhow::Result<String> {
        let r = self.entry_resource(project_id, id)?;
        let b = self.open_entry(&r)?;
        let v = b
            .fields
            .iter()
            .find(|f| f.label.eq_ignore_ascii_case(label))
            .map(|f| f.value.clone())
            .ok_or_else(|| anyhow::anyhow!("no field {label}"))?;
        self.audit(
            "reveal",
            &Reader::PERSON,
            Some(project_id),
            Some(id),
            Some(json!({ "fields": [label] })),
        )?;
        Ok(v)
    }

    /// Writes a vault file, opened, into `dir` for the person. Returns its path.
    pub async fn export(&self, project_id: &str, id: &str, dir: &Path) -> anyhow::Result<PathBuf> {
        let r = self.entry_resource(project_id, id)?;
        let b = self.open_entry(&r)?;
        let (Some(f), Some(hash)) = (b.file, r.url) else {
            anyhow::bail!("that entry has no file");
        };
        let bytes = self.file_plain(project_id, id, &hash).await?;
        let name = Path::new(&f.name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "vault file".into());
        let mut path = dir.join(&name);
        let mut n = 1;
        while path.exists() {
            n += 1;
            let p = Path::new(&name);
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            path = dir.join(format!("{stem} ({n}){ext}"));
        }
        std::fs::write(&path, bytes)?;
        self.audit(
            "export",
            &Reader::PERSON,
            Some(project_id),
            Some(id),
            Some(json!({ "title": f.name })),
        )?;
        Ok(path)
    }

    /// Searches a project's entries. Values come back masked.
    pub fn search(
        &self,
        project_id: &str,
        query: &str,
        who: &Reader<'_>,
    ) -> anyhow::Result<Vec<Hit>> {
        let entries: Vec<(ProjectResource, Body)> = self
            .store
            .vault_entries(Some(project_id))?
            .into_iter()
            .filter_map(|r| self.open_entry(&r).ok().map(|b| (r, b)))
            .collect();
        let found = self.with_open(|o| Ok(o.index.search(query, &entries)))?;
        let hits: Vec<Hit> = found
            .into_iter()
            .map(|(i, passage)| {
                let (r, b) = &entries[i];
                let v = self.view(r, b, &[]);
                Hit {
                    id: v.id,
                    title: v.title,
                    kind: v.kind,
                    fields: v.fields,
                    file: v.file.map(|f| f.name),
                    passage: passage.map(|p| index::mask_numbers(&p)),
                }
            })
            .collect();
        self.audit(
            "search",
            who,
            Some(project_id),
            None,
            Some(json!({ "query": query, "results": hits.len() })),
        )?;
        Ok(hits)
    }

    /// An entry for an agent: masked, with `reveal` fields' values (which
    /// the person released). A file's text comes with it when revealed.
    pub fn get(
        &self,
        project_id: &str,
        id: &str,
        reveal: &[String],
        who: &Reader<'_>,
    ) -> anyhow::Result<(Entry, Option<String>)> {
        let r = self.entry_resource(project_id, id)?;
        let b = self.open_entry(&r)?;
        let v = self.view(&r, &b, reveal);
        let text = if reveal.iter().any(|l| l == "file") {
            self.with_open(|o| Ok(o.index.text(&r.id)))?
        } else {
            None
        };
        remember(v.fields.iter().filter_map(|f| f.value.clone()));
        if let Some(t) = &text {
            remember(index::numbers(t));
        }
        self.audit(
            if reveal.is_empty() { "get" } else { "reveal" },
            who,
            Some(project_id),
            Some(id),
            Some(json!({ "title": b.title, "fields": reveal })),
        )?;
        Ok((v, text))
    }

    /// The fields that answer an ask, by label or entry name, masked.
    pub fn find_fields(
        &self,
        project_id: &str,
        wanted: &[String],
    ) -> anyhow::Result<Vec<(Entry, Vec<String>)>> {
        let mut out = Vec::new();
        for r in self.store.vault_entries(Some(project_id))? {
            let Ok(b) = self.open_entry(&r) else { continue };
            let labels: Vec<String> = b
                .fields
                .iter()
                .filter(|f| {
                    wanted
                        .iter()
                        .any(|w| index::matches(w, &f.label) || index::matches(w, &b.title))
                })
                .map(|f| f.label.clone())
                .collect();
            if !labels.is_empty() {
                out.push((self.view(&r, &b, &[]), labels));
            }
        }
        Ok(out)
    }

    /// Writes one audit entry; what was read is sealed when the vault is open.
    pub fn audit(
        &self,
        action: &str,
        who: &Reader<'_>,
        project_id: Option<&str>,
        resource_id: Option<&str>,
        detail: Option<Value>,
    ) -> anyhow::Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let detail = match detail {
            Some(d) => self
                .with_open(|o| {
                    Ok(serde_json::to_value(crypto::seal(
                        &o.keys[&o.current],
                        &o.current,
                        &audit_aad(&id),
                        d.to_string().as_bytes(),
                    ))?)
                })
                .ok(),
            None => None,
        };
        self.store.vault_audit_add(&VaultAuditRow {
            id,
            at: Utc::now(),
            device_id: self.store.device_id()?,
            session_id: who.session_id.map(str::to_string),
            project_id: project_id.map(str::to_string),
            resource_id: resource_id.map(str::to_string),
            action: action.into(),
            actor: who.actor.into(),
            detail,
        })
    }

    /// The audit log, newest first; details open when the vault is.
    pub fn audit_log(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<AuditView>> {
        let names: HashMap<String, String> = self
            .store
            .vault_devices()?
            .into_iter()
            .map(|d| (d.id, d.name))
            .collect();
        let open = self.is_open();
        Ok(self
            .store
            .vault_audit(project_id, limit)?
            .into_iter()
            .map(|a| {
                let detail = a.detail.as_ref().filter(|_| open).and_then(|d| {
                    let s: Sealed = serde_json::from_value(d.clone()).ok()?;
                    self.with_open(|o| {
                        let k = o.keys.get(&s.key_id).ok_or_else(locked)?;
                        Ok(serde_json::from_slice(&crypto::open(
                            k,
                            &audit_aad(&a.id),
                            &s,
                        )?)?)
                    })
                    .ok()
                });
                AuditView {
                    device: names
                        .get(&a.device_id)
                        .cloned()
                        .unwrap_or_else(|| a.device_id.clone()),
                    id: a.id,
                    at: a.at,
                    action: a.action,
                    actor: a.actor,
                    session_id: a.session_id,
                    project_id: a.project_id,
                    resource_id: a.resource_id,
                    detail,
                }
            })
            .collect())
    }

    /// Brings the index up to date with the entries, and saves it sealed.
    async fn reindex(&self) {
        if let Err(e) = self.try_reindex().await {
            tracing::warn!(error = %e, "couldn't index the vault");
        }
    }

    async fn try_reindex(&self) -> anyhow::Result<()> {
        let entries = self.store.vault_entries(None)?;
        let stale: Vec<ProjectResource> = self.with_open(|o| Ok(o.index.stale(&entries)))?;
        let mut docs = Vec::new();
        for r in stale {
            let body = self.open_entry(&r)?;
            let mut text = format!("{}\n{}", body.title, body.note);
            for f in &body.fields {
                text.push_str(&format!("\n{}", f.label));
            }
            if let (Some(f), Some(hash)) = (&body.file, &r.url) {
                match self.file_plain(&r.project_id, &r.id, hash).await {
                    Ok(bytes) => {
                        let name = f.name.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::knowledge::extract::file_text(&name, &bytes)
                        })
                        .await?
                        {
                            Ok(t) => text.push_str(&format!("\n{t}")),
                            Err(e) => {
                                tracing::warn!(error = %e, "couldn't read a vault file's text")
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "couldn't fetch a vault file"),
                }
            }
            docs.push((r, text));
        }
        let dir = self.dir.clone();
        self.with_open(|o| {
            o.index.update(&entries, docs);
            o.index.save(&dir, &o.current, &o.keys[&o.current])
        })
    }
}

/// Whether an asked-for field name means `label` ("tax id" is "Federal tax ID").
pub fn label_matches(wanted: &str, label: &str) -> bool {
    index::matches(wanted, label)
}

/// Values released to agents in this run of the app, so what's stored
/// and synced shows them masked ([`redact`]).
static RELEASED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Remembers values an agent was given.
pub fn remember(values: impl IntoIterator<Item = String>) {
    let mut r = RELEASED.lock().unwrap();
    for v in values {
        let v = v.trim().to_string();
        if v.chars().count() >= 4 && !r.contains(&v) {
            r.push(v);
        }
    }
    // Longest first, so a value inside another is masked as the longer.
    r.sort_by_key(|v| std::cmp::Reverse(v.len()));
}

/// `text` with every released vault value masked.
pub fn redact(text: &str) -> Cow<'_, str> {
    let r = RELEASED.lock().unwrap();
    if r.is_empty() || !r.iter().any(|v| text.contains(v.as_str())) {
        return Cow::Borrowed(text);
    }
    let mut out = text.to_string();
    for v in r.iter() {
        out = out.replace(v.as_str(), &crypto::mask(v));
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests;
