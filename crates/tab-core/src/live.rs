//! What the desktop app sends over the hub's live channel (see
//! [`tab_hub::live`]): the commands another device steers a session with,
//! and how attached files are named so any device can fetch them.
//!
//! Events going the other way are the app's own [`EngineEvent`]s, as JSON:
//! only the ones that aren't in the synced transcript (streamed text and
//! approvals). Stored messages and status reach watchers through sync.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::Decision;
use crate::engine::EngineEvent;
use crate::model::Approval;

/// A file attached to a chat message, as stored in the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    /// `sha256:<hex>`; the hub keeps the file while a record mentions it.
    pub hash: String,
    pub name: String,
    pub size: u64,
    /// For a file an agent's reply named: the path the reply says, which
    /// other devices look the file up by (see [`crate::named_files`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Where an attachment lives in a device's attachments folder:
/// `<dir>/<hash hex>/<name>`. The folder is a cache; the hub has the file.
pub fn attachment_path(dir: &Path, hash: &str, name: &str) -> Option<PathBuf> {
    let hex = tab_domain::blob::hex(hash)?;
    Some(dir.join(hex).join(safe_name(name)))
}

/// A file name that stays inside its folder.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || ":*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    match clean.trim().trim_matches('.') {
        "" => "file".into(),
        s => s.chars().take(120).collect(),
    }
}

/// The file an attachment path refers to, if it's in the hub's naming
/// (`.../<hash hex>/<name>`). Older attachments (random folder names)
/// aren't in the hub and give `None`.
pub fn file_ref(path: &Path) -> Option<FileRef> {
    let hash = tab_domain::blob::from_hex(path.parent()?.file_name()?.to_str()?)?;
    Some(FileRef {
        hash,
        name: path.file_name()?.to_string_lossy().into_owned(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        path: None,
    })
}

/// A message's `files`, for the attachments it carries.
pub fn file_refs(paths: &[String]) -> Vec<FileRef> {
    paths
        .iter()
        .filter_map(|p| file_ref(Path::new(p)))
        .collect()
}

/// What another device asks of a session it doesn't run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SteerCommand {
    /// The person's message: steers a running turn, or starts the next.
    Message {
        text: String,
        #[serde(default)]
        files: Vec<FileRef>,
    },
    /// An answer to an approval prompt (same words as `approval_resolve`).
    Approval {
        id: String,
        decision: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        answers: Option<Value>,
    },
    /// "Send now": stop the tool the turn is waiting on so the steering
    /// already sent is read right away.
    Interrupt,
    Stop,
}

impl Decision {
    /// From the words the UI uses: `allow`, `always`, `answer`, or anything
    /// else to deny.
    pub fn from_parts(decision: &str, message: Option<String>, answers: Option<Value>) -> Self {
        match decision {
            "allow" => Decision::Allow,
            "answer" => Decision::Answer(answers.unwrap_or_default()),
            "always" => Decision::AllowAlways,
            _ => Decision::Deny(message.unwrap_or_else(|| "The person declined.".into())),
        }
    }
}

/// Longest string in an approval prompt's tool input sent to watchers. The
/// summary says what's being approved; a file an edit would write needn't
/// travel whole.
pub const MAX_LIVE_STRING: usize = 2000;

/// Cuts every string in `v` to `max` characters.
fn trim_strings(v: &mut Value, max: usize) {
    match v {
        Value::String(s) if s.chars().count() > max => {
            let cut: String = s.chars().take(max).collect();
            *s = format!("{cut}… (cut off)");
        }
        Value::Array(a) => a.iter_mut().for_each(|x| trim_strings(x, max)),
        Value::Object(o) => o.values_mut().for_each(|x| trim_strings(x, max)),
        _ => {}
    }
}

/// A live event as watchers receive it: only what the person sees in a
/// chat as it happens (streamed reply text, approval prompts). Tool calls
/// and their output don't go live; the call's summary arrives with the
/// synced transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveEvent {
    SessionDelta { session_id: String, text: String },
    ApprovalRequested { approval: Approval },
    ApprovalResolved { session_id: String, id: String },
}

impl LiveEvent {
    /// The engine events that go over the live channel, and their session.
    pub fn from_engine(e: &EngineEvent) -> Option<Self> {
        Some(match e {
            EngineEvent::SessionDelta { session_id, text } => LiveEvent::SessionDelta {
                session_id: session_id.clone(),
                text: text.clone(),
            },
            EngineEvent::ApprovalRequested { approval } => {
                let mut approval = approval.clone();
                trim_strings(&mut approval.input, MAX_LIVE_STRING);
                LiveEvent::ApprovalRequested { approval }
            }
            EngineEvent::ApprovalResolved { session_id, id } => LiveEvent::ApprovalResolved {
                session_id: session_id.clone(),
                id: id.clone(),
            },
            _ => return None,
        })
    }

    pub fn session(&self) -> &str {
        match self {
            LiveEvent::SessionDelta { session_id, .. }
            | LiveEvent::ApprovalResolved { session_id, .. } => session_id,
            LiveEvent::ApprovalRequested { approval } => &approval.session_id,
        }
    }

    pub fn into_engine(self) -> EngineEvent {
        match self {
            LiveEvent::SessionDelta { session_id, text } => {
                EngineEvent::SessionDelta { session_id, text }
            }
            LiveEvent::ApprovalRequested { approval } => {
                EngineEvent::ApprovalRequested { approval }
            }
            LiveEvent::ApprovalResolved { session_id, id } => {
                EngineEvent::ApprovalResolved { session_id, id }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attachment_paths_carry_the_hash() {
        let dir = std::env::temp_dir().join(format!("tab-live-{}", std::process::id()));
        let hash = tab_domain::blob::hash(b"png bytes");
        let p = attachment_path(&dir, &hash, "../shot 1.png").unwrap();
        assert_eq!(p.parent().unwrap().parent().unwrap(), dir);
        assert_eq!(p.file_name().unwrap(), "shot 1.png");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"png bytes").unwrap();
        assert_eq!(
            file_ref(&p),
            Some(FileRef {
                hash: hash.clone(),
                name: "shot 1.png".into(),
                size: 9,
                path: None,
            })
        );
        // A pre-hub attachment folder isn't a hash.
        assert_eq!(file_ref(&dir.join("6f1c-uuid").join("x.png")), None);
        assert_eq!(attachment_path(&dir, "nope", "x"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn names_stay_in_their_folder() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Users\\x\\a:b.png"), "a_b.png");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
    }

    #[test]
    fn steer_commands_read_as_json() {
        let c: SteerCommand =
            serde_json::from_value(json!({"type": "message", "text": "hi"})).unwrap();
        assert_eq!(
            c,
            SteerCommand::Message {
                text: "hi".into(),
                files: vec![]
            }
        );
        let c: SteerCommand = serde_json::from_value(json!({"type": "stop"})).unwrap();
        assert_eq!(c, SteerCommand::Stop);
        let c: SteerCommand = serde_json::from_value(json!({"type": "interrupt"})).unwrap();
        assert_eq!(c, SteerCommand::Interrupt);
        assert_eq!(
            Decision::from_parts("nope", None, None),
            Decision::Deny("The person declined.".into())
        );
    }

    #[test]
    fn only_streamed_events_go_live() {
        let delta = EngineEvent::SessionDelta {
            session_id: "s".into(),
            text: "Hel".into(),
        };
        let live = LiveEvent::from_engine(&delta).unwrap();
        assert_eq!(live.session(), "s");
        let json = serde_json::to_value(&live).unwrap();
        assert_eq!(json, serde_json::to_value(&delta).unwrap());
        assert!(LiveEvent::from_engine(&EngineEvent::ConfigChanged).is_none());
    }

    #[test]
    fn approval_prompts_go_live_without_huge_inputs() {
        let big = "x".repeat(MAX_LIVE_STRING + 500);
        let e = EngineEvent::ApprovalRequested {
            approval: Approval {
                id: "a".into(),
                session_id: "s".into(),
                tool: "Write".into(),
                summary: "Write src/big.rs".into(),
                input: json!({ "file_path": "src/big.rs", "content": big, "n": 3 }),
            },
        };
        let LiveEvent::ApprovalRequested { approval } = LiveEvent::from_engine(&e).unwrap() else {
            panic!()
        };
        assert_eq!(approval.input["file_path"], "src/big.rs");
        assert_eq!(approval.input["n"], 3);
        let content = approval.input["content"].as_str().unwrap();
        assert!(content.ends_with("(cut off)") && content.len() < MAX_LIVE_STRING + 50);
        assert_eq!(approval.summary, "Write src/big.rs");
    }
}
