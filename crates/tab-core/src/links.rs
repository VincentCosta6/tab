//! Links to things the agent creates for a task (a PR, an issue, a ticket).
//!
//! tab attaches these to the task's Context itself, so it doesn't depend on
//! the agent remembering. To support a new service, add one entry to
//! [`KINDS`]: how its URLs look, and which agent calls create one.

use serde_json::Value;

/// One kind of linkable thing.
pub struct LinkKind {
    pub name: &'static str,
    /// Bash commands that create one (`gh pr create`). Matched as substrings.
    pub commands: &'static [&'static str],
    /// Recognises a URL of this kind and returns a short label for it.
    pub label: fn(&str) -> Option<String>,
}

pub const KINDS: &[LinkKind] = &[
    LinkKind {
        name: "github-pr",
        commands: &["gh pr create"],
        label: |u| github(u, "pull"),
    },
    LinkKind {
        name: "github-issue",
        commands: &["gh issue create"],
        label: |u| github(u, "issues"),
    },
    // Add services here, e.g. Linear, Jira, GitLab.
];

/// `owner/repo#12` for `https://github.com/owner/repo/<segment>/12`.
fn github(url: &str, segment: &str) -> Option<String> {
    let p: Vec<&str> = url
        .strip_prefix("https://github.com/")?
        .split('/')
        .collect();
    (p.len() == 4
        && p[2] == segment
        && !p[0].is_empty()
        && !p[1].is_empty()
        && p[3].chars().all(|c| c.is_ascii_digit())
        && !p[3].is_empty())
    .then(|| format!("{}/{}#{}", p[0], p[1], p[3]))
}

/// Whether this tool call creates something whose link we should attach: a
/// registered Bash command, or any MCP tool whose name says it creates.
/// Claude Code names tools `Bash` and `mcp__server__tool`; opencode names
/// them `bash` and `server_tool`.
pub fn is_creator(tool: &str, input: &Value) -> bool {
    if tool.eq_ignore_ascii_case("bash") {
        let cmd = input["command"].as_str().unwrap_or_default();
        return KINDS
            .iter()
            .flat_map(|k| k.commands)
            .any(|c| cmd.contains(c));
    }
    !tool.starts_with("mcp__tab__")
        && !tool.starts_with("tab_")
        && tool.to_lowercase().contains("create")
}

/// The URLs in some text, once each, with trailing punctuation trimmed.
pub fn urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || "<>()[]\"'`,".contains(c)) {
        let word = word.trim_end_matches(['.', ';', ':', '!', '?']);
        if (word.starts_with("https://") || word.starts_with("http://"))
            && !out.iter().any(|u| u == word)
        {
            out.push(word.to_string());
        }
    }
    out
}

/// The registered-kind label for a URL (`owner/repo#12`), if it is one.
pub fn label(url: &str) -> Option<String> {
    KINDS.iter().find_map(|k| (k.label)(url))
}

/// The links in a creating call's output, as `(url, label)`, once each.
pub fn found(output: &str) -> Vec<(String, String)> {
    urls(output)
        .into_iter()
        .filter_map(|u| label(&u).map(|l| (u, l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_created_links_only() {
        let out = "Warning: dirty\nhttps://github.com/o/tab/pull/18\n";
        assert_eq!(
            found(out),
            [("https://github.com/o/tab/pull/18".into(), "o/tab#18".into())]
        );
        assert_eq!(found("see https://github.com/o/r/issues/3.")[0].1, "o/r#3");
        assert!(
            found("https://github.com/o/r/pull/new/branch https://github.com/o/r/pull/9/files")
                .is_empty()
        );
    }

    #[test]
    fn knows_which_calls_create() {
        assert!(is_creator(
            "Bash",
            &json!({"command": "gh pr create --draft"})
        ));
        assert!(!is_creator("Bash", &json!({"command": "gh pr list"})));
        assert!(is_creator("mcp__linear__create_issue", &json!({})));
        assert!(!is_creator("mcp__linear__list_issues", &json!({})));
        assert!(!is_creator("mcp__tab__tab_create_task", &json!({})));
        // opencode's names
        assert!(is_creator("bash", &json!({"command": "gh pr create"})));
        assert!(is_creator("github_create_pull_request", &json!({})));
        assert!(!is_creator("tab_tab_create_task", &json!({})));
    }

    #[test]
    fn finds_urls_in_prose() {
        assert_eq!(
            urls("see https://github.com/o/r/pull/3, and (https://x.io/a)."),
            ["https://github.com/o/r/pull/3", "https://x.io/a"]
        );
    }
}
