//! How a chat's messages are shown, worked out without GPUI: which rows a
//! message becomes, what a tool call says it's doing, the agent's step list.
//! Mirrors the logic in `src/modules/work/Chat.tsx`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tab_core::model::{QueueItem, SessionMessage, SessionStatus};

/// Each tool call's output, keyed by the call's message id: the
/// `tool_result` whose `meta.tool_use_id` is the call's `meta.id`, or else
/// an unlabelled `tool_result` right after the call.
pub fn results(messages: &[SessionMessage]) -> HashMap<i64, SessionMessage> {
    let by_use: HashMap<&str, &SessionMessage> = messages
        .iter()
        .filter(|m| m.kind == "tool_result")
        .filter_map(|m| {
            m.meta
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(|u| (u, m))
        })
        .collect();
    let mut out = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        if m.kind != "tool" {
            continue;
        }
        let by_id = m
            .meta
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| by_use.get(id).copied());
        let next = messages
            .get(i + 1)
            .filter(|n| n.kind == "tool_result" && n.meta.get("tool_use_id").is_none());
        if let Some(r) = by_id.or(next) {
            out.insert(m.id, r.clone());
        }
    }
    out
}

/// Drops MCP prefixes: `mcp__tab__tab_search` → `tab_search`.
pub fn tool_name(t: &str) -> &str {
    let t = match t.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").map_or(rest, |(_, name)| name),
        None => t,
    };
    t.split(':').next().unwrap_or(t)
}

const TODO_TOOLS: [&str; 2] = ["TodoWrite", "todowrite"];
const TASK_TOOLS: [&str; 2] = ["TaskCreate", "TaskUpdate"];

fn is_steps_tool(tool: &str) -> bool {
    tool == "tab_steps" || tool.ends_with("_tab_steps")
}

/// Step-list tools show in the step card, not as rows.
pub fn is_step_tool(tool: &str) -> bool {
    is_steps_tool(tool)
        || TODO_TOOLS.contains(&tool)
        || TASK_TOOLS.contains(&tool)
        || tool == "TaskList"
        || tool == "TaskGet"
}

fn base(p: Option<&Value>) -> String {
    p.and_then(Value::as_str)
        .and_then(|s| s.split('/').filter(|s| !s.is_empty()).next_back())
        .unwrap_or("")
        .to_string()
}

/// What a tool call is doing, in words.
pub fn describe_tool(m: &SessionMessage) -> String {
    let raw = m
        .meta
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or(&m.content);
    let tool = tool_name(raw);
    let input = m.meta.get("input").cloned().unwrap_or(Value::Null);
    let desc = input
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let file = || {
        let b = base(
            input
                .get("file_path")
                .or(input.get("filePath"))
                .or(input.get("notebook_path")),
        );
        if b.is_empty() {
            "a file".to_string()
        } else {
            b
        }
    };
    match tool {
        "Bash" | "bash" => {
            return if desc.is_empty() {
                "Running a command".into()
            } else {
                desc
            };
        }
        "Read" | "read" => return format!("Reading {}", file()),
        "Edit" | "edit" | "MultiEdit" | "NotebookEdit" => return format!("Editing {}", file()),
        "Write" | "write" => return format!("Writing {}", file()),
        "Grep" | "grep" | "Glob" | "glob" => return "Searching the code".into(),
        "WebSearch" | "websearch" => return "Searching the web".into(),
        "WebFetch" | "webfetch" => return "Reading a web page".into(),
        "Agent" | "Task" | "task" => {
            return if desc.is_empty() {
                "Delegating to a helper agent".into()
            } else {
                format!("Delegating: {desc}")
            };
        }
        "ToolSearch" => return "Loading tools".into(),
        _ => {}
    }
    if tool.starts_with("tab_") {
        return match tool {
            "tab_search" => "Searching your queue",
            "tab_get_item" => "Reading a queue item",
            "tab_attach_context" => "Attaching context",
            "tab_add_found" => "Filing something under Found",
            "tab_create_task" => "Creating a follow-up task",
            "tab_project" => "Reading the project",
            "tab_project_note" => "Saving a project note",
            "tab_graph_search" | "tab_graph_expand" => "Looking up what tab knows",
            _ => "Working in tab",
        }
        .into();
    }
    if desc.is_empty() {
        tool.replace('_', " ")
    } else {
        desc
    }
}

/// One line of the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    /// A tool call, its line, and its output once it has one.
    Tool {
        line: String,
        output: Option<String>,
        failed: bool,
        tab: bool,
    },
    /// Small grey text: status, timing, wake-ups.
    Note {
        text: String,
    },
    Error {
        text: String,
    },
}

/// The rows a chat's messages become.
pub fn rows(messages: &[SessionMessage]) -> Vec<Row> {
    let results = results(messages);
    let paired: HashSet<i64> = results.values().map(|m| m.id).collect();
    let mut out = Vec::new();
    for m in messages {
        let tool = m.meta.get("tool").and_then(Value::as_str).unwrap_or("");
        let row = match (m.role.as_str(), m.kind.as_str()) {
            (_, "suggestion") => None,
            ("user", _) => Some(Row::User {
                text: m.content.clone(),
            }),
            (_, "text") => Some(Row::Assistant {
                text: m.content.clone(),
            }),
            (_, "tool") if tool == "AskUserQuestion" || is_step_tool(tool_name(tool)) => None,
            (_, "tool") => {
                let result = results.get(&m.id);
                Some(Row::Tool {
                    line: describe_tool(m),
                    output: result.map(|r| r.content.clone()).filter(|c| !c.is_empty()),
                    failed: result.is_some_and(|r| {
                        r.meta.get("is_error").and_then(Value::as_bool) == Some(true)
                    }),
                    tab: tool.starts_with("mcp__tab") || tool.starts_with("tab_"),
                })
            }
            (_, "tool_result") if paired.contains(&m.id) => None,
            (_, "tool_result") => Some(Row::Tool {
                line: "Output".into(),
                output: Some(m.content.clone()),
                failed: false,
                tab: false,
            }),
            (_, "error") => Some(Row::Error {
                text: m.content.clone(),
            }),
            (_, "result") => {
                let secs = m
                    .meta
                    .get("duration_ms")
                    .and_then(Value::as_f64)
                    .map(|ms| (ms / 1000.).round() as i64);
                let cost = m.meta.get("cost_usd").and_then(Value::as_f64);
                let parts: Vec<String> = [
                    secs.map(|s| format!("{s}s")),
                    cost.map(|c| format!("${c:.3}")),
                ]
                .into_iter()
                .flatten()
                .collect();
                (!parts.is_empty()).then(|| Row::Note {
                    text: parts.join(" · "),
                })
            }
            (_, "compacted") => Some(Row::Note {
                text: "Earlier messages were compacted".into(),
            }),
            _ => (!m.content.is_empty()).then(|| Row::Note {
                text: m.content.clone(),
            }),
        };
        out.extend(row);
    }
    out
}

/// What the agent is doing, as the chat header shows it.
pub fn status_label(status: SessionStatus, waiting_on_you: bool) -> &'static str {
    if waiting_on_you {
        return "Needs you";
    }
    match status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Working => "Working…",
        SessionStatus::NeedsInput => "Needs you",
        SessionStatus::Monitoring => "Monitoring",
        SessionStatus::Failed => "Failed",
    }
}

/// Things to ask about an item before its chat has started.
pub fn suggestions(item: &QueueItem) -> Vec<&'static str> {
    let kind = item.event.kind.as_str();
    let app = kind.split('.').next().unwrap_or(kind);
    match (kind, app) {
        ("github.review_requested", _) => vec![
            "Review this PR and list the risks",
            "Check out the branch and run the tests",
            "Draft review comments",
        ],
        ("github.ci_failed", _) => {
            vec!["Find why CI failed and fix it", "Show me the failing test"]
        }
        (_, "slack" | "email") => vec![
            "Draft a reply",
            "Find everything related to this",
            "Summarize the thread",
        ],
        ("meeting.action_item", _) => {
            vec!["Start a draft of this", "Find related docs and tickets"]
        }
        (_, "tab") => vec![
            "Work on this",
            "Break this into steps",
            "Find related work in my queue",
        ],
        _ => vec![
            "Summarize this and suggest next steps",
            "Work on this",
            "Find related work in my queue",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(id: i64, role: &str, kind: &str, content: &str, meta: Value) -> SessionMessage {
        SessionMessage {
            id,
            session_id: "s".into(),
            role: role.into(),
            kind: kind.into(),
            content: content.into(),
            meta,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn tool_names_lose_mcp_prefixes() {
        assert_eq!(tool_name("mcp__tab__tab_search"), "tab_search");
        assert_eq!(tool_name("mcp__github_api__list_prs"), "list_prs");
        assert_eq!(tool_name("Bash"), "Bash");
        assert_eq!(tool_name("tab_search:x"), "tab_search");
    }

    #[test]
    fn tools_describe_themselves() {
        let read = msg(
            1,
            "assistant",
            "tool",
            "Read",
            json!({"tool": "Read", "input": {"file_path": "/a/b/main.rs"}}),
        );
        assert_eq!(describe_tool(&read), "Reading main.rs");
        let bash = msg(
            2,
            "assistant",
            "tool",
            "Bash",
            json!({"tool": "Bash", "input": {"description": "Run tests"}}),
        );
        assert_eq!(describe_tool(&bash), "Run tests");
        let tab = msg(
            3,
            "assistant",
            "tool",
            "",
            json!({"tool": "mcp__tab__tab_search"}),
        );
        assert_eq!(describe_tool(&tab), "Searching your queue");
        let other = msg(4, "assistant", "tool", "", json!({"tool": "list_open_prs"}));
        assert_eq!(describe_tool(&other), "list open prs");
    }

    #[test]
    fn tool_calls_pair_with_their_output() {
        let ms = vec![
            msg(1, "user", "text", "fix it", Value::Null),
            msg(
                2,
                "assistant",
                "tool",
                "Bash",
                json!({"tool": "Bash", "id": "u1", "input": {"description": "Run tests"}}),
            ),
            msg(
                3,
                "tool",
                "tool_result",
                "1 failed",
                json!({"tool_use_id": "u1", "is_error": true}),
            ),
            msg(4, "assistant", "text", "Fixed.", Value::Null),
            msg(5, "assistant", "tool", "", json!({"tool": "TodoWrite"})),
            msg(
                6,
                "system",
                "result",
                "",
                json!({"duration_ms": 4200, "cost_usd": 0.0123}),
            ),
            msg(7, "assistant", "tool", "Grep", json!({"tool": "Grep"})),
            msg(8, "tool", "tool_result", "3 matches", Value::Null),
        ];
        assert_eq!(
            rows(&ms),
            vec![
                Row::User {
                    text: "fix it".into()
                },
                Row::Tool {
                    line: "Run tests".into(),
                    output: Some("1 failed".into()),
                    failed: true,
                    tab: false
                },
                Row::Assistant {
                    text: "Fixed.".into()
                },
                Row::Note {
                    text: "4s · $0.012".into()
                },
                Row::Tool {
                    line: "Searching the code".into(),
                    output: Some("3 matches".into()),
                    failed: false,
                    tab: false
                },
            ]
        );
    }

    #[test]
    fn status_prefers_waiting_on_you() {
        assert_eq!(status_label(SessionStatus::Working, true), "Needs you");
        assert_eq!(status_label(SessionStatus::Working, false), "Working…");
    }
}
