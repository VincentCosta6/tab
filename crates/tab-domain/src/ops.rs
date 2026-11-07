//! The changes a hub applies, stores in order, and replicates.
//!
//! Every write to a hub is a batch of [`Op`]s. The hub checks them
//! ([`validate`]), the storage backend applies them in one transaction and
//! gives each a sequence number, and any device can catch up by asking for
//! the changes after the last sequence it saw. That one log is what lets the
//! same data live on the desktop, in the cloud, or both, and move between
//! them: a migration is "replay the log somewhere else".
//!
//! Records are JSON documents addressed by `(kind, id)`, such as
//! `("item", "github:pr:42")`. Edges link two node ids (`kind:id`) with a
//! relation, which is what the knowledge graph is made of.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Position in a hub's change log. 0 means "from the beginning".
pub type Seq = u64;

/// Longest kind, id or relation name.
pub const MAX_NAME: usize = 256;
/// Largest record body, as JSON.
pub const MAX_BODY_BYTES: usize = 1 << 20;
/// Most ops in one batch.
pub const MAX_BATCH: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Creates or replaces a record.
    Put {
        kind: String,
        id: String,
        body: Value,
    },
    /// Removes a record and every edge touching it.
    Delete { kind: String, id: String },
    /// Adds an edge. Adding one that exists does nothing.
    Link {
        from: String,
        to: String,
        rel: String,
    },
    /// Removes an edge. Removing one that doesn't exist does nothing.
    Unlink {
        from: String,
        to: String,
        rel: String,
    },
}

/// A stored record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub kind: String,
    pub id: String,
    pub body: Value,
    /// The sequence of the change that last wrote it.
    pub seq: Seq,
    pub updated_at: DateTime<Utc>,
}

impl Record {
    pub fn node(&self) -> String {
        node(&self.kind, &self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub rel: String,
}

/// A record that matched a search.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub kind: String,
    pub id: String,
    pub seq: Seq,
}

/// One entry of the change log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Applied {
    pub seq: Seq,
    pub op: Op,
    /// Who made the change: a device or person id.
    pub by: String,
    pub at: DateTime<Utc>,
}

/// The node id a record has in the graph.
pub fn node(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    /// Index of the op in its batch.
    pub index: usize,
    pub message: String,
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "op {}: {}", self.index, self.message)
    }
}

impl std::error::Error for OpError {}

fn name_ok(what: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.len() > MAX_NAME {
        return Err(format!("{what} is longer than {MAX_NAME} bytes"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{what} has control characters"));
    }
    Ok(())
}

fn kind_ok(kind: &str) -> Result<(), String> {
    name_ok("kind", kind)?;
    if !kind
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
    {
        return Err(format!(
            "kind {kind:?} must be lowercase letters, digits, _ or ."
        ));
    }
    Ok(())
}

/// Checks a batch before anything is written. A bad op rejects the whole
/// batch, so a batch applies entirely or not at all.
pub fn validate(ops: &[Op]) -> Result<(), OpError> {
    if ops.len() > MAX_BATCH {
        return Err(OpError {
            index: MAX_BATCH,
            message: format!("more than {MAX_BATCH} ops in one batch"),
        });
    }
    for (index, op) in ops.iter().enumerate() {
        let r = match op {
            Op::Put { kind, id, body } => {
                kind_ok(kind).and_then(|_| name_ok("id", id)).and_then(|_| {
                    if !body.is_object() {
                        return Err("body must be a JSON object".into());
                    }
                    let len = serde_json::to_vec(body).map_or(usize::MAX, |b| b.len());
                    if len > MAX_BODY_BYTES {
                        return Err(format!("body is larger than {MAX_BODY_BYTES} bytes"));
                    }
                    Ok(())
                })
            }
            Op::Delete { kind, id } => kind_ok(kind).and_then(|_| name_ok("id", id)),
            Op::Link { from, to, rel } | Op::Unlink { from, to, rel } => name_ok("from", from)
                .and_then(|_| name_ok("to", to))
                .and_then(|_| name_ok("rel", rel)),
        };
        r.map_err(|message| OpError { index, message })?;
    }
    Ok(())
}

/// Splits a search query into terms. A record matches when its
/// [`search_text`] contains every term, ignoring case. Every storage backend
/// has to agree on this, and the shared conformance tests check it.
pub fn search_terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The text a record is searched by: every string in its body.
pub fn search_text(body: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(s);
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            Value::Object(m) => m.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(body, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ops_round_trip_as_tagged_json() {
        let op = Op::Link {
            from: "item:1".into(),
            to: "project:tab".into(),
            rel: "in".into(),
        };
        let v = serde_json::to_value(&op).unwrap();
        assert_eq!(v["op"], "link");
        assert_eq!(serde_json::from_value::<Op>(v).unwrap(), op);
    }

    #[test]
    fn a_bad_op_names_its_index() {
        let ops = vec![
            Op::Put {
                kind: "item".into(),
                id: "1".into(),
                body: json!({"title": "ok"}),
            },
            Op::Put {
                kind: "Item".into(),
                id: "2".into(),
                body: json!({}),
            },
        ];
        let e = validate(&ops).unwrap_err();
        assert_eq!(e.index, 1);
        assert!(validate(&ops[..1]).is_ok());
        let not_object = [Op::Put {
            kind: "item".into(),
            id: "3".into(),
            body: json!("text"),
        }];
        assert!(validate(&not_object).is_err());
    }

    #[test]
    fn search_text_collects_strings() {
        let body = json!({"title": "Fix login", "tags": ["auth", 3], "meta": {"repo": "tab"}});
        let text = search_text(&body);
        for word in ["Fix login", "auth", "tab"] {
            assert!(text.contains(word), "{text}");
        }
        assert!(!text.contains('3'));
    }
}
