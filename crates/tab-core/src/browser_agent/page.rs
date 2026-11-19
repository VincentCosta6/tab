//! tab's script for live pages. The desktop shell runs it in an isolated
//! JavaScript world (`WKContentWorld` on macOS): it shares the DOM with the
//! page but not its globals or prototypes, so the page can't see it, call
//! it, or fake what it reports. Element handles live in that world's own
//! map, never in the DOM. Agents never send JavaScript; they pick one of the
//! [`Op`]s below. See `docs/agent-browser.md` (rules 9 and 10).

use serde::{Deserialize, Serialize};

use super::gate::Element;

/// What tab asks of a page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// The page's text and interactive elements (refs reset).
    Snapshot { max_chars: usize },
    /// One element, fresh, by ref.
    Describe {
        #[serde(rename = "ref")]
        id: String,
    },
    Click {
        #[serde(rename = "ref")]
        id: String,
    },
    Type {
        #[serde(rename = "ref")]
        id: String,
        text: String,
    },
    Select {
        #[serde(rename = "ref")]
        id: String,
        label: String,
    },
    /// Outlines an element for the person (and for the screenshot on a card).
    Highlight {
        #[serde(rename = "ref")]
        id: Option<String>,
    },
    /// Covers secret fields before a screenshot; `false` uncovers them.
    Mask { on: bool },
    /// When the person last used the page with a real (trusted) click or key.
    Touched,
    /// Whether the page finished loading, and where it is.
    Ready,
    /// Loads `url` in the tab (the gate checked it; the dock's navigation
    /// handler checks it again).
    Go { url: String },
}

/// A page as the script sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default)]
    pub truncated: bool,
}

/// The script, run fresh for each op in the isolated world: it installs
/// itself once per page (`globalThis.__tab`, which only that world sees)
/// and then runs `OP`. The shell substitutes `__OP__` with the op as JSON
/// (a JSON literal is a valid JS expression, so nothing is interpolated as
/// code). It returns a JSON string.
pub const SCRIPT: &str = include_str!("page.js");

/// The script for one op, ready to run in the isolated world.
pub fn script(op: &Op) -> String {
    let json = serde_json::to_string(op).expect("ops serialize");
    SCRIPT.replace("__OP__", &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_go_in_as_json_data_never_as_code() {
        let s = script(&Op::Type {
            id: "e1".into(),
            text: "\"); fetch('https://evil.example'); (\"".into(),
        });
        assert!(s.contains(r#"const OP = {"op":"type","ref":"e1","text":"\"); fetch('https://evil.example'); (\""};"#));
        assert_eq!(s.matches("__OP__").count(), 0);
        // `</script>` and line separators can't end the literal either: JSON
        // escapes quotes and backslashes, and the script isn't HTML.
        let s = script(&Op::Snapshot { max_chars: 10 });
        assert!(s.contains(r#"{"op":"snapshot","max_chars":10}"#));
    }

    #[test]
    fn snapshots_parse() {
        let snap: Snapshot = serde_json::from_value(serde_json::json!({
            "url": "https://dash.cloudflare.com/",
            "title": "DNS",
            "text": "Records",
            "truncated": false,
            "elements": [{ "ref": "e1", "tag": "button", "role": "", "kind": "submit", "labels": ["Save"], "secret": false, "search": false, "href": null, "submits": true, "toggles": false, "value": null, "disabled": false }]
        }))
        .unwrap();
        assert_eq!(snap.elements[0].id, "e1");
        assert!(snap.elements[0].submits);
    }
}
