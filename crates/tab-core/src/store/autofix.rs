//! The chat bar's links and autofix's state for them ([`Watch`]).

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::{Store, parse_ts, ts};
use crate::autofix::Watch;

const COLS: &str =
    "item_id, url, enabled, dismissed, active_at, next_at, checked_at, head, handled, last";

fn read(r: &Row) -> rusqlite::Result<Watch> {
    let opt_ts = |i: usize| -> rusqlite::Result<Option<DateTime<Utc>>> {
        r.get::<_, Option<String>>(i)?
            .map(|s| parse_ts(&s))
            .transpose()
    };
    Ok(Watch {
        item_id: r.get(0)?,
        url: r.get(1)?,
        enabled: r.get(2)?,
        dismissed: r.get(3)?,
        active_at: parse_ts(&r.get::<_, String>(4)?)?,
        next_at: opt_ts(5)?,
        checked_at: opt_ts(6)?,
        head: r.get(7)?,
        handled: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        last: r.get(9)?,
    })
}

impl Store {
    /// Every link with a state in this task's chat.
    pub fn autofix_for_item(&self, item_id: &str) -> anyhow::Result<Vec<Watch>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM autofix WHERE item_id = ?1 ORDER BY url"
        ))?;
        let rows = stmt.query_map([item_id], read)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn autofix_get(&self, item_id: &str, url: &str) -> anyhow::Result<Option<Watch>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM autofix WHERE item_id = ?1 AND url = ?2"),
                params![item_id, url],
                read,
            )
            .optional()?)
    }

    /// Turns autofix on or off. Turning it on counts as activity and looks
    /// right away; turning it off forgets what was handed over.
    pub fn autofix_enable(
        &self,
        item_id: &str,
        url: &str,
        on: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO autofix (item_id, url, enabled, active_at, next_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(item_id, url) DO UPDATE SET enabled = excluded.enabled,
               active_at = excluded.active_at, next_at = excluded.next_at, dismissed = 0,
               handled = CASE WHEN excluded.enabled THEN handled ELSE '[]' END,
               last = CASE WHEN excluded.enabled THEN last ELSE NULL END",
            params![item_id, url, on, ts(now), on.then(|| ts(now))],
        )?;
        Ok(())
    }

    /// Hides the bar for a link (and stops autofix on it), or shows it again.
    pub fn autofix_dismiss(
        &self,
        item_id: &str,
        url: &str,
        dismissed: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO autofix (item_id, url, dismissed, active_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(item_id, url) DO UPDATE SET dismissed = excluded.dismissed,
               enabled = CASE WHEN excluded.dismissed THEN 0 ELSE enabled END",
            params![item_id, url, dismissed, ts(now)],
        )?;
        Ok(())
    }

    /// Links with autofix on whose next look is due.
    pub fn autofix_due(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Watch>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLS} FROM autofix
             WHERE enabled = 1 AND dismissed = 0 AND next_at IS NOT NULL AND next_at <= ?1
             ORDER BY next_at"
        ))?;
        let rows = stmt.query_map([ts(now)], read)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Writes what a look found and when to look next.
    pub fn autofix_checked(&self, w: &Watch) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE autofix SET enabled = ?3, active_at = ?4, next_at = ?5, checked_at = ?6,
               head = ?7, handled = ?8, last = ?9
             WHERE item_id = ?1 AND url = ?2 AND enabled = 1",
            params![
                w.item_id,
                w.url,
                w.enabled,
                ts(w.active_at),
                w.next_at.map(ts),
                w.checked_at.map(ts),
                w.head,
                serde_json::to_string(&w.handled)?,
                w.last,
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn enable_check_dismiss() {
        let s = Store::in_memory().unwrap();
        let now = Utc::now();
        let url = "https://github.com/o/r/pull/1";
        s.autofix_enable("i", url, true, now).unwrap();
        let mut w = s.autofix_due(now).unwrap().pop().unwrap();
        assert!(w.enabled && !w.dismissed && w.handled.is_empty());

        w.handled = vec!["a".into()];
        w.next_at = Some(now + Duration::minutes(1));
        w.checked_at = Some(now);
        s.autofix_checked(&w).unwrap();
        assert!(s.autofix_due(now).unwrap().is_empty());
        assert_eq!(s.autofix_get("i", url).unwrap().unwrap().handled, ["a"]);

        // Dismissing stops autofix; a check finishing afterwards doesn't
        // turn it back on.
        s.autofix_dismiss("i", url, true, now).unwrap();
        s.autofix_checked(&w).unwrap();
        let got = s.autofix_for_item("i").unwrap();
        assert!(got[0].dismissed && !got[0].enabled);

        // Off forgets what was handed over.
        s.autofix_enable("i", url, false, now).unwrap();
        assert!(s.autofix_get("i", url).unwrap().unwrap().handled.is_empty());
    }
}
