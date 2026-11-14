//! OpenCode: its chats run over ACP (`opencode acp`, see `agents::acp`).
//! What stays here is strict mode's part that ACP can't do: finding the MCP
//! servers the person (or, for someone else's code, the checkout) set up,
//! so the chat can turn them off and spot one that loads anyway.

use std::path::Path;

use serde_json::Value;

use crate::sandbox::Trust;

pub(super) fn own_mcp_servers(directory: &Path, trust: Trust) -> Vec<String> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(d) => dirs.push(Path::new(&d).join("opencode")),
        None => dirs.extend(crate::sandbox::home().map(|h| h.join(".config/opencode"))),
    }
    if let Some(d) = std::env::var_os("OPENCODE_CONFIG_DIR") {
        dirs.push(d.into());
    }
    if trust != Trust::Person {
        dirs.push(directory.to_path_buf());
        dirs.push(directory.join(".opencode"));
    }
    let mut files: Vec<std::path::PathBuf> = dirs
        .iter()
        .flat_map(|d| ["opencode.json", "opencode.jsonc", "config.json"].map(|f| d.join(f)))
        .collect();
    files.extend(std::env::var_os("OPENCODE_CONFIG").map(Into::into));
    let mut names: Vec<String> = files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|text| mcp_names(&text))
        .filter(|n| n != "tab")
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The `mcp` keys of an OpenCode config, which may be JSONC.
fn mcp_names(text: &str) -> Vec<String> {
    serde_json::from_str::<Value>(&strip_jsonc(text))
        .ok()
        .and_then(|v| v["mcp"].as_object().map(|m| m.keys().cloned().collect()))
        .unwrap_or_default()
}

/// JSONC to JSON: drops comments and trailing commas outside strings.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            (',', _) => {
                let next = chars.clone().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Servers OpenCode's `GET /mcp` reports as connected, other than tab's
/// and the remote servers tab added (`allowed`).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mcp_server_names_from_jsonc() {
        let text = r#"{
            // personal servers
            "mcp": {
                "health": { "type": "remote", "url": "https://h.example/mcp?a=1//2" },
                /* old */ "notes": { "type": "local", "command": ["notes"], },
            },
        }"#;
        assert_eq!(mcp_names(text), ["health", "notes"]);
        assert!(mcp_names("{}").is_empty());
        assert!(mcp_names("not json").is_empty());
    }

    #[test]
    fn review_chats_also_turn_off_the_checkouts_servers() {
        let dir = std::env::temp_dir().join(format!("tab-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencode.json"),
            r#"{"mcp":{"pr-server":{"type":"local"}}}"#,
        )
        .unwrap();
        assert!(own_mcp_servers(&dir, Trust::Agent).contains(&"pr-server".to_string()));
        assert!(!own_mcp_servers(&dir, Trust::Person).contains(&"pr-server".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
