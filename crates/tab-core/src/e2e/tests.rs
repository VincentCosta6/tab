use std::collections::HashMap;

use super::*;

/// The hub's file store, in memory.
#[derive(Default)]
struct MemFiles(Mutex<HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl Files for MemFiles {
    async fn put(&self, bytes: Vec<u8>) -> anyhow::Result<String> {
        let hash = tab_domain::blob::hash(&bytes);
        self.0.lock().unwrap().insert(hash.clone(), bytes);
        Ok(hash)
    }
    async fn get(&self, hash: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(hash).cloned())
    }
}

struct World {
    store: Arc<Store>,
    files: Arc<MemFiles>,
    dir: PathBuf,
    project: String,
}

impl World {
    fn new() -> Self {
        let store = Arc::new(Store::in_memory().unwrap());
        let dir = std::env::temp_dir().join(format!("tab-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = store
            .project_save(None, "Shop", "#7c5cff", "store", Utc::now())
            .unwrap()
            .id;
        Self {
            store,
            files: Default::default(),
            dir,
            project,
        }
    }

    /// A device on this hub. Devices share the store (standing in for the
    /// hub's synced rows); `as_` switches which one is "this device".
    fn device(&self, id: &str) -> (String, Arc<E2e>) {
        let d = E2e::for_tests(self.store.clone(), &self.dir.join(id), id);
        std::fs::create_dir_all(self.dir.join(id)).unwrap();
        d.set_files(self.files.clone());
        (id.to_string(), d)
    }

    fn as_(&self, d: &(String, Arc<E2e>)) -> Arc<E2e> {
        self.store.set_device_id(&d.0).unwrap();
        d.1.clone()
    }

    /// Everything the hub holds for the vault, as text.
    fn hub_text(&self) -> String {
        let conn = self.store.conn();
        let mut out = String::new();
        for t in [
            "project_resources",
            "vault_devices",
            "vault_generations",
            "vault_keys",
            "vault_audit",
        ] {
            let mut st = conn.prepare(&format!("SELECT * FROM {t}")).unwrap();
            let n = st.column_count();
            let rows = st
                .query_map([], |r| {
                    Ok((0..n)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            out.push_str(&rows.join("\n"));
        }
        out.push_str(&format!(
            "{:?}",
            self.files.0.lock().unwrap().values().collect::<Vec<_>>()
        ));
        out
    }
}

fn ein(id: Option<String>) -> EntryInput {
    EntryInput {
        id,
        title: "Company IDs".into(),
        note: "From the IRS letter".into(),
        fields: vec![FieldInput {
            label: "EIN".into(),
            value: Some("12-3456789".into()),
        }],
    }
}

#[tokio::test]
async fn devices_join_leave_and_recover_and_the_hub_sees_only_ciphertext() {
    let w = World::new();
    let laptop = w.device("laptop");
    let desk = w.device("desk");

    let a = w.as_(&laptop);
    assert!(matches!(a.status().unwrap().state, State::None));
    let words = a.create().await.unwrap();
    assert_eq!(words.split(' ').count(), 24);
    let e = a.save(&w.project, ein(None)).await.unwrap();
    assert_eq!(e.fields[0].masked, "•••• 6789");
    assert_eq!(e.fields[0].value, None);
    assert_eq!(a.reveal(&w.project, &e.id, "ein").unwrap(), "12-3456789");
    a.add_file(
        &w.project,
        "return.txt",
        b"Form 1120 for 2024, routing 021000021",
    )
    .await
    .unwrap();

    // Nothing readable reaches the hub.
    let hub = w.hub_text();
    for secret in [
        "12-3456789",
        "Company IDs",
        "IRS",
        "EIN",
        "return.txt",
        "Form 1120",
        "021000021",
    ] {
        assert!(!hub.contains(secret), "the hub can read {secret}");
    }

    // A second device asks; the laptop compares the code and lets it in.
    let b = w.as_(&desk);
    assert!(matches!(b.status().unwrap().state, State::NoAccess));
    let code = b.ask().unwrap();
    assert!(matches!(b.status().unwrap().state, State::Asked));
    let a = w.as_(&laptop);
    let asking = a
        .status()
        .unwrap()
        .devices
        .into_iter()
        .find(|d| d.id == "desk")
        .unwrap();
    assert_eq!(asking.code, code);
    a.approve("desk").unwrap();
    let b = w.as_(&desk);
    assert!(matches!(b.status().unwrap().state, State::Locked));
    assert!(b.entries(&w.project).is_err());
    b.unlock().await.unwrap();
    assert_eq!(b.entries(&w.project).unwrap().len(), 2);
    assert_eq!(b.reveal(&w.project, &e.id, "EIN").unwrap(), "12-3456789");
    b.lock();

    // Removing the desk makes a new key it doesn't get; everything is
    // sealed again under it and still opens on the laptop.
    let a = w.as_(&laptop);
    a.remove("desk").await.unwrap();
    assert_eq!(w.store.vault_generations().unwrap().len(), 2);
    let entries = a.entries(&w.project).unwrap();
    assert_eq!(entries.len(), 2);
    let file = entries.iter().find(|e| e.kind == "file").unwrap();
    let out = a.export(&w.project, &file.id, &w.dir).await.unwrap();
    assert_eq!(
        std::fs::read(out).unwrap(),
        b"Form 1120 for 2024, routing 021000021"
    );
    let b = w.as_(&desk);
    assert!(b.unlock().await.is_err());
    assert!(matches!(b.status().unwrap().state, State::NoAccess));

    // Every device lost: the recovery key lets a new one in.
    let phone = w.device("phone");
    let c = w.as_(&phone);
    assert!(c.recover("not the words").await.is_err());
    c.recover(&words).await.unwrap();
    assert_eq!(c.reveal(&w.project, &e.id, "EIN").unwrap(), "12-3456789");

    // And it's all in the audit log, append only.
    let actions: Vec<String> = c
        .audit_log(None, 100)
        .unwrap()
        .into_iter()
        .map(|a| a.action)
        .collect();
    for a in [
        "create",
        "add",
        "reveal",
        "device_ask",
        "device_approve",
        "unlock",
        "device_remove",
        "rotate",
        "recover",
        "export",
    ] {
        assert!(actions.iter().any(|x| x == a), "no {a} in {actions:?}");
    }
}

#[tokio::test]
async fn agents_see_masked_values_and_released_ones_are_redacted() {
    let w = World::new();
    let laptop = w.device("laptop");
    let a = w.as_(&laptop);
    a.create().await.unwrap();
    let e = a.save(&w.project, ein(None)).await.unwrap();
    let coord = Reader {
        actor: "coordinator",
        session_id: Some("s1"),
    };

    let hits = a.search(&w.project, "what is our ein", &coord).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].fields[0].masked, "•••• 6789");
    assert!(!serde_json::to_string(&hits).unwrap().contains("3456789"));
    assert!(
        a.search(&w.project, "bank routing", &coord)
            .unwrap()
            .is_empty()
    );

    let (masked, _) = a.get(&w.project, &e.id, &[], &coord).unwrap();
    assert_eq!(masked.fields[0].value, None);
    let (shown, _) = a.get(&w.project, &e.id, &["EIN".into()], &coord).unwrap();
    assert_eq!(shown.fields[0].value.as_deref(), Some("12-3456789"));
    // Once released, it's masked in what's stored and synced.
    assert_eq!(redact("Our EIN is 12-3456789."), "Our EIN is •••• 6789.");

    let asked = a.find_fields(&w.project, &["ein".into()]).unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, vec!["EIN"]);

    // Editing without the value keeps it.
    let edited = EntryInput {
        fields: vec![FieldInput {
            label: "EIN".into(),
            value: None,
        }],
        ..ein(Some(e.id.clone()))
    };
    a.save(&w.project, edited).await.unwrap();
    assert_eq!(a.reveal(&w.project, &e.id, "EIN").unwrap(), "12-3456789");

    // Locked, nothing reads.
    a.lock();
    assert!(a.search(&w.project, "ein", &coord).is_err());
    assert!(a.get(&w.project, &e.id, &[], &coord).is_err());
}

#[tokio::test]
async fn a_key_the_hub_made_up_is_refused() {
    let w = World::new();
    let laptop = w.device("laptop");
    let a = w.as_(&laptop);
    a.create().await.unwrap();
    a.lock();

    // The hub writes a newer generation of its own, wrapped for the
    // laptop's public key (which it knows), hoping new entries go under it.
    let public = w.store.vault_device("laptop").unwrap().unwrap().public_key;
    let fake = Key::random();
    let now = Utc::now() + chrono::Duration::seconds(5);
    w.store
        .vault_generation_put(&VaultGeneration {
            id: "k-fake".into(),
            prev_id: None,
            endorsement: None,
            prev_sealed: None,
            check_mac: fake.vouch(&check_msg("k-fake")),
            created_by: "hub".into(),
            created_at: now,
        })
        .unwrap();
    let wrapped = crypto::wrap(&fake, "k-fake", "laptop", &public).unwrap();
    w.store
        .vault_wrap_put(
            "k-fake",
            "laptop",
            &serde_json::to_value(wrapped).unwrap(),
            now,
        )
        .unwrap();
    // It's passed over: the vault opens with the real key, and what's
    // written goes under that.
    a.unlock().await.unwrap();
    let e = a.save(&w.project, ein(None)).await.unwrap();
    let r = w.store.vault_entries(Some(&w.project)).unwrap();
    let sealed = &r.iter().find(|r| r.id == e.id).unwrap().options["sealed"];
    assert_ne!(sealed["key_id"], "k-fake");
    // Alone, it's refused.
    let real = sealed["key_id"].as_str().unwrap().to_string();
    w.store.vault_wraps_delete("laptop").unwrap();
    w.store
        .vault_wrap_put(
            "k-fake",
            "laptop",
            &serde_json::to_value(crypto::wrap(&fake, "k-fake", "laptop", &public).unwrap())
                .unwrap(),
            now,
        )
        .unwrap();
    a.lock();
    let err = a.unlock().await.unwrap_err().to_string();
    assert!(err.contains("can't trace"), "{err} ({real})");

    // A device key the hub swapped in shows a different code.
    let b = w.device("desk");
    let b = w.as_(&b);
    let code = b.ask().unwrap();
    let mut d = w.store.vault_device("desk").unwrap().unwrap();
    d.public_key = KeyPair::generate().public();
    w.store.vault_device_put(&d).unwrap();
    let a = w.as_(&laptop);
    let shown = a
        .status()
        .unwrap()
        .devices
        .into_iter()
        .find(|d| d.id == "desk")
        .unwrap();
    assert_ne!(shown.code, code);
}

#[test]
fn plaintext_vault_resources_are_refused() {
    let w = World::new();
    let input = crate::store::ResourceInput {
        kind: "note".into(),
        title: "EIN".into(),
        note: "12-3456789".into(),
        access: Some("vault".into()),
        ..Default::default()
    };
    assert!(
        w.store
            .resource_save(&w.project, &input, "agent", Utc::now())
            .is_err()
    );
}
