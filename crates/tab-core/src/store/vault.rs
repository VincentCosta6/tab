//! The end-to-end encrypted vault's rows (see `crate::e2e`, which is the
//! only thing that should call these). Nothing here is plaintext: public
//! keys, wrapped keys, MACs and sealed bodies.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Store, parse_ts, ts};
use crate::model::ProjectResource;

/// A device that may open the vault, or the recovery key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultDevice {
    pub id: String,
    pub name: String,
    pub public_key: String,
    /// `pending`, `active` or `removed`.
    pub state: String,
    /// `{key_id, mac}`, see `e2e`.
    pub vouch: Option<serde_json::Value>,
    pub approved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One generation of the vault key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultGeneration {
    pub id: String,
    pub prev_id: Option<String>,
    pub endorsement: Option<String>,
    pub prev_sealed: Option<serde_json::Value>,
    pub check_mac: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// One entry of the vault's audit log. `detail` is sealed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultAuditRow {
    pub id: String,
    pub at: DateTime<Utc>,
    pub device_id: String,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub resource_id: Option<String>,
    pub action: String,
    pub actor: String,
    pub detail: Option<serde_json::Value>,
}

fn json_col(r: &rusqlite::Row<'_>, col: &str) -> rusqlite::Result<Option<serde_json::Value>> {
    Ok(r.get::<_, Option<String>>(col)?
        .and_then(|s| serde_json::from_str(&s).ok()))
}

fn device_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VaultDevice> {
    Ok(VaultDevice {
        id: r.get("id")?,
        name: r.get("name")?,
        public_key: r.get("public_key")?,
        state: r.get("state")?,
        vouch: json_col(r, "vouch")?,
        approved_by: r.get("approved_by")?,
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
        updated_at: parse_ts(&r.get::<_, String>("updated_at")?)?,
    })
}

fn generation_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<VaultGeneration> {
    Ok(VaultGeneration {
        id: r.get("id")?,
        prev_id: r.get("prev_id")?,
        endorsement: r.get("endorsement")?,
        prev_sealed: json_col(r, "prev_sealed")?,
        check_mac: r.get("check_mac")?,
        created_by: r.get("created_by")?,
        created_at: parse_ts(&r.get::<_, String>("created_at")?)?,
    })
}

impl Store {
    pub fn vault_devices(&self) -> anyhow::Result<Vec<VaultDevice>> {
        Ok(self
            .conn()
            .prepare("SELECT * FROM vault_devices ORDER BY created_at")?
            .query_map([], device_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn vault_device(&self, id: &str) -> anyhow::Result<Option<VaultDevice>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT * FROM vault_devices WHERE id = ?1",
                [id],
                device_row,
            )
            .optional()?)
    }

    pub fn vault_device_put(&self, d: &VaultDevice) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO vault_devices (id, name, public_key, state, vouch, approved_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, public_key = excluded.public_key,
                 state = excluded.state, vouch = excluded.vouch, approved_by = excluded.approved_by,
                 updated_at = excluded.updated_at",
            params![
                d.id,
                d.name,
                d.public_key,
                d.state,
                d.vouch.as_ref().map(|v| v.to_string()),
                d.approved_by,
                ts(d.created_at),
                ts(d.updated_at)
            ],
        )?;
        Ok(())
    }

    /// Generations, oldest first.
    pub fn vault_generations(&self) -> anyhow::Result<Vec<VaultGeneration>> {
        Ok(self
            .conn()
            .prepare("SELECT * FROM vault_generations ORDER BY created_at, id")?
            .query_map([], generation_row)?
            .collect::<Result<_, _>>()?)
    }

    pub fn vault_generation_put(&self, g: &VaultGeneration) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO vault_generations (id, prev_id, endorsement, prev_sealed, check_mac, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                g.id,
                g.prev_id,
                g.endorsement,
                g.prev_sealed.as_ref().map(|v| v.to_string()),
                g.check_mac,
                g.created_by,
                ts(g.created_at)
            ],
        )?;
        Ok(())
    }

    /// The wrapped vault keys for one device (or `recovery`): `(key_id, wrapped)`.
    pub fn vault_wraps(&self, device_id: &str) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        Ok(self
            .conn()
            .prepare("SELECT key_id, wrapped FROM vault_keys WHERE device_id = ?1")?
            .query_map([device_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(k, w)| Some((k, serde_json::from_str(&w).ok()?)))
            .collect())
    }

    pub fn vault_wrap_put(
        &self,
        key_id: &str,
        device_id: &str,
        wrapped: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO vault_keys (id, key_id, device_id, wrapped, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT (id) DO UPDATE SET wrapped = excluded.wrapped, updated_at = excluded.updated_at",
            params![
                format!("{key_id}:{device_id}"),
                key_id,
                device_id,
                wrapped.to_string(),
                ts(now)
            ],
        )?;
        Ok(())
    }

    /// Drops every wrapped key a device had.
    pub fn vault_wraps_delete(&self, device_id: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM vault_keys WHERE device_id = ?1", [device_id])?;
        Ok(())
    }

    /// The fingerprint this device saw for a key generation.
    pub fn vault_trusted(&self, key_id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT fingerprint FROM vault_trust WHERE key_id = ?1",
                [key_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn vault_trust(
        &self,
        key_id: &str,
        fingerprint: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO vault_trust (key_id, fingerprint, seen_at) VALUES (?1, ?2, ?3)",
            params![key_id, fingerprint, ts(now)],
        )?;
        Ok(())
    }

    /// Whether this device has opened any vault key generation before.
    pub fn vault_trust_any(&self) -> anyhow::Result<bool> {
        Ok(self
            .conn()
            .query_row("SELECT EXISTS (SELECT 1 FROM vault_trust)", [], |r| {
                r.get(0)
            })?)
    }

    pub fn vault_audit_add(&self, a: &VaultAuditRow) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO vault_audit (id, at, device_id, session_id, project_id, resource_id, action, actor, detail, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?2, ?2)",
            params![
                a.id,
                ts(a.at),
                a.device_id,
                a.session_id,
                a.project_id,
                a.resource_id,
                a.action,
                a.actor,
                a.detail.as_ref().map(|v| v.to_string())
            ],
        )?;
        Ok(())
    }

    /// The newest audit entries first, for one project or all.
    pub fn vault_audit(
        &self,
        project_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<VaultAuditRow>> {
        Ok(self
            .conn()
            .prepare(
                "SELECT * FROM vault_audit WHERE ?1 IS NULL OR project_id = ?1 OR project_id IS NULL
                 ORDER BY at DESC LIMIT ?2",
            )?
            .query_map(params![project_id, limit as i64], |r| {
                Ok(VaultAuditRow {
                    id: r.get("id")?,
                    at: parse_ts(&r.get::<_, String>("at")?)?,
                    device_id: r.get("device_id")?,
                    session_id: r.get("session_id")?,
                    project_id: r.get("project_id")?,
                    resource_id: r.get("resource_id")?,
                    action: r.get("action")?,
                    actor: r.get("actor")?,
                    detail: json_col(r, "detail")?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }

    /// Vault entries: one project's, or every project's (`None`).
    pub fn vault_entries(&self, project_id: Option<&str>) -> anyhow::Result<Vec<ProjectResource>> {
        Ok(match project_id {
            Some(p) => self.project_resources(p, "vault")?,
            None => {
                let ids: Vec<String> = self
                    .conn()
                    .prepare(
                        "SELECT DISTINCT project_id FROM project_resources WHERE access = 'vault'",
                    )?
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut out = Vec::new();
                for p in ids {
                    out.extend(self.project_resources(&p, "vault")?);
                }
                out
            }
        }
        .into_iter()
        .filter(|r| r.access == "vault")
        .collect())
    }

    /// Writes a vault entry: a `project_resources` row with access `vault`
    /// whose readable fields say nothing (`title` is a placeholder) and
    /// whose body is sealed in `options.sealed`. `url` is the sealed
    /// file's hash, for files.
    pub fn vault_entry_put(
        &self,
        project_id: &str,
        id: &str,
        kind: &str,
        url: Option<&str>,
        sealed: &serde_json::Value,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            kind == "note" || kind == "file",
            "the vault keeps notes and files"
        );
        let options = serde_json::json!({ "sealed": sealed }).to_string();
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE project_resources SET url = ?3, options = ?4, updated_at = ?5
             WHERE id = ?1 AND project_id = ?2 AND access = 'vault'",
            params![id, project_id, url, options, ts(now)],
        )?;
        if n == 0 {
            conn.execute(
                "INSERT INTO project_resources (id, project_id, kind, title, url, note, access, options, status,
                     pinned, added_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, '', 'vault', ?6, 'indexed', 0, 'user', ?7, ?7)",
                params![id, project_id, kind, VAULT_TITLE, url, options, ts(now)],
            )?;
        }
        Ok(())
    }
}

/// What a vault entry is called where it isn't decrypted (the hub, a
/// locked device).
pub const VAULT_TITLE: &str = "Vault entry";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_log_only_grows() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let row = VaultAuditRow {
            id: "a1".into(),
            at: now,
            device_id: "d".into(),
            session_id: None,
            project_id: None,
            resource_id: None,
            action: "unlock".into(),
            actor: "person".into(),
            detail: None,
        };
        s.vault_audit_add(&row).unwrap();
        // Neither a change nor a delete takes, from here or from the hub.
        s.conn()
            .execute(
                "UPDATE vault_audit SET action = 'nothing' WHERE id = 'a1'",
                [],
            )
            .unwrap();
        s.conn().execute("DELETE FROM vault_audit", []).unwrap();
        let rows = s.vault_audit(None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "unlock");
    }
}
