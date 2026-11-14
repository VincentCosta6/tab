//! Model families (see [`tab_domain::families`]). The built-in ones come
//! from the harness catalog; what the person saves (their own, and edited
//! built-ins) is kept in `model_families`, which syncs through the hub.

use chrono::{DateTime, Utc};
use rusqlite::params;
use tab_domain::families::{self, ModelFamily};

use super::{Store, ts};

impl Store {
    /// Every family there is: the built-ins (or the person's copies), then
    /// the person's own.
    pub fn model_families(&self) -> anyhow::Result<Vec<ModelFamily>> {
        Ok(families::merge(
            crate::agents::catalog::built_in_families(),
            self.model_families_saved()?,
        ))
    }

    /// The ids a workflow step may name.
    pub fn model_family_ids(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.model_families()?.into_iter().map(|f| f.id).collect())
    }

    fn model_families_saved(&self) -> anyhow::Result<Vec<ModelFamily>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT body FROM model_families ORDER BY id")?;
        let out = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|b| serde_json::from_str(&b).ok())
            .collect();
        Ok(out)
    }

    /// Saves a family, replacing the built-in with its id if there is one.
    pub fn model_family_save(&self, f: &ModelFamily, now: DateTime<Utc>) -> anyhow::Result<()> {
        if let Some(why) = families::check(f) {
            anyhow::bail!(why);
        }
        self.conn().execute(
            "INSERT INTO model_families (id, body, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
            params![f.id, serde_json::to_string(f)?, ts(now)],
        )?;
        Ok(())
    }

    /// Deletes the person's family, or puts a built-in back as tab ships it.
    pub fn model_family_delete(&self, id: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM model_families WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tab_domain::families::Candidate;

    #[test]
    fn saved_families_join_and_replace_the_built_in_ones() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        assert_eq!(s.model_family_ids().unwrap(), ["fast", "balanced", "deep"]);

        let mine = ModelFamily {
            id: "cheap".into(),
            name: "Cheap".into(),
            candidates: vec![Candidate::new("pi", "")],
        };
        s.model_family_save(&mine, now).unwrap();
        let deep = ModelFamily {
            id: "deep".into(),
            name: "Deep".into(),
            candidates: vec![Candidate::new("codex", "gpt-6.1-sol")],
        };
        s.model_family_save(&deep, now).unwrap();
        let all = s.model_families().unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[2], deep);
        assert_eq!(all[3], mine);

        // Deleting an edited built-in puts tab's back.
        s.model_family_delete("deep").unwrap();
        assert_ne!(s.model_families().unwrap()[2], deep);
        assert!(
            s.model_family_save(
                &ModelFamily {
                    id: "bad id".into(),
                    ..mine
                },
                now
            )
            .is_err()
        );
    }
}
