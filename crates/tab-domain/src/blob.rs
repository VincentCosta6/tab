//! Files a hub keeps beside its records: chat attachments now, encrypted
//! handoff bundles later.
//!
//! A file is stored once, named by the SHA-256 of its bytes
//! (`sha256:<64 hex>`), so the same screenshot attached twice costs nothing
//! and a name can't point at the wrong bytes. Records refer to a file by
//! putting that name anywhere in their body, as a JSON string. That's all a
//! hub needs to know which files are still in use ([`refs`]); a file nothing
//! refers to is deleted after a grace period, which gives a device time to
//! upload a file before the record that mentions it.
//!
//! The bytes never travel over the sync socket: devices upload and download
//! them over HTTP (the hub's own port locally, signed URLs in the cloud).

use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Largest file a hub takes.
pub const MAX_BLOB_BYTES: usize = 50 * 1024 * 1024;

const PREFIX: &str = "sha256:";

/// The name of a file with these bytes.
pub fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(PREFIX.len() + 64);
    s.push_str(PREFIX);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Whether `s` is a file name in the form [`hash`] makes: `sha256:` and 64
/// lowercase hex digits.
pub fn is_hash(s: &str) -> bool {
    s.strip_prefix(PREFIX).is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The hex part of a hash, for file and folder names (`:` isn't allowed in
/// Windows paths).
pub fn hex(hash: &str) -> Option<&str> {
    is_hash(hash).then(|| &hash[PREFIX.len()..])
}

/// The hash a hex part from [`hex`] came from.
pub fn from_hex(hex: &str) -> Option<String> {
    let h = format!("{PREFIX}{hex}");
    is_hash(&h).then_some(h)
}

/// Every file a record body refers to: each string anywhere in it that is a
/// hash.
pub fn refs(body: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(body, &mut out);
    out
}

fn collect(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) if is_hash(s) => {
            out.insert(s.clone());
        }
        // Rows from SQL tables keep nested JSON (a message's `meta`) as
        // text, so look inside strings that hold JSON too.
        Value::String(s) if s.starts_with('{') || s.starts_with('[') => {
            if let Ok(inner) = serde_json::from_str::<Value>(s) {
                collect(&inner, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hashes_are_sha256_hex() {
        assert_eq!(
            hash(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(is_hash(&hash(b"abc")));
        assert_ne!(hash(b"a"), hash(b"b"));
    }

    #[test]
    fn only_well_formed_hashes_count() {
        let good = hash(b"x");
        assert!(is_hash(&good));
        assert!(!is_hash(&good.to_uppercase()));
        assert!(!is_hash(&good[..good.len() - 1]));
        assert!(!is_hash(&format!("{good}0")));
        assert!(!is_hash(&good.replace("sha256:", "md5:")));
        assert!(!is_hash(&good.replace('a', "g")));
        assert!(!is_hash(""));
    }

    #[test]
    fn hex_round_trips() {
        let h = hash(b"x");
        let x = hex(&h).unwrap();
        assert_eq!(x.len(), 64);
        assert_eq!(from_hex(x).as_deref(), Some(h.as_str()));
        assert_eq!(hex("nope"), None);
        assert_eq!(from_hex("../etc"), None);
    }

    #[test]
    fn refs_are_found_anywhere_including_json_in_strings() {
        let (a, b, c) = (hash(b"a"), hash(b"b"), hash(b"c"));
        let meta = json!({ "files": [{ "hash": c, "name": "x.png" }] }).to_string();
        let body = json!({
            "top": a,
            "nested": { "list": [1, b, "not a hash"] },
            "meta": meta,
            "again": a,
        });
        let found: Vec<String> = refs(&body).into_iter().collect();
        let mut want = vec![a, b, c];
        want.sort();
        assert_eq!(found, want);
        assert!(refs(&json!({ "meta": "{not json" })).is_empty());
    }
}
