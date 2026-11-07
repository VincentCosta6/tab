//! Command-line tools a connection signs in, described as data.
//!
//! A connection that has a CLI agents already know (`gh` for GitHub) lists
//! it on its [`ConnectorDescriptor`](crate::ConnectorDescriptor). tab then
//! runs that CLI for agents with the connection's token, instead of letting
//! the agent hold the token. The rules here say which commands only read,
//! which change something, and which are hard to undo, the same three
//! levels as connector actions, so a chat asks the person before the last
//! kind. They also say what a command changed, so the app can refresh what
//! shows it.

use serde::{Deserialize, Serialize};

/// What running a command means.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CliEffect {
    /// Never leaves the computer and needs no token: runs in the chat's
    /// sandbox as usual (`git status`, `git commit`).
    Local,
    /// Reads from the service.
    #[default]
    Read,
    /// Changes something on the service.
    Write,
    /// Hard to undo or seen by other people. Always asks the person.
    Destructive,
    /// Decided by the HTTP method the command sends, the way `gh api` does:
    /// GET reads, DELETE is destructive, anything else writes. A GraphQL
    /// call writes when it is a mutation.
    ByMethod,
    /// Not run for agents at all; the rule's `reason` says why.
    Blocked,
}

/// How the connection's token reaches the tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CliAuth {
    /// An environment variable, like `GH_TOKEN`.
    Env { var: String },
    /// git reaches the host through tab: a chat's git rewrites `remotes`
    /// (`https://github.com/`, `git@github.com:`) to a local endpoint, and
    /// tab adds the token as it forwards each request to `url`. git itself
    /// never holds the token, and tab checks every push's ref updates.
    GitEndpoint { url: String, remotes: Vec<String> },
}

/// One rule. The first rule that matches decides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CliRule {
    /// The subcommand path, one word per level. `*` matches any word, and
    /// `a|b` either. Empty matches only the bare command with no words.
    #[serde(default)]
    pub command: Vec<String>,
    /// If set, at least one must be present. `--force` matches `--force`
    /// and `--force=…`; `-f` matches `-f` and `-fvalue`; `arg:+` matches a
    /// later argument starting with `+`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    pub effect: CliEffect,
    /// Shown to the agent when the command is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What a change touches (`pull`, `issue`, `repo`, `branch`), so the
    /// app refreshes it afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

impl CliRule {
    pub fn new(command: &str, effect: CliEffect) -> Self {
        Self {
            command: command.split_whitespace().map(String::from).collect(),
            effect,
            ..Default::default()
        }
    }

    pub fn flags(mut self, flags: &[&str]) -> Self {
        self.flags = flags.iter().map(|f| f.to_string()).collect();
        self
    }

    pub fn reason(mut self, r: &str) -> Self {
        self.reason = Some(r.into());
        self
    }

    pub fn resource(mut self, r: &str) -> Self {
        self.resource = Some(r.into());
        self
    }
}

/// A CLI the connection signs in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliInfo {
    /// The command agents type, and the program tab runs.
    pub name: String,
    pub description: String,
    pub auth: CliAuth,
    /// Options that take a value, so the value isn't read as a subcommand.
    #[serde(default)]
    pub value_flags: Vec<String>,
    pub rules: Vec<CliRule>,
    /// When no rule matches.
    pub default: CliEffect,
}

/// What [`CliInfo::classify`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliMatch {
    /// Never [`CliEffect::ByMethod`].
    pub effect: CliEffect,
    pub reason: Option<String>,
    pub resource: Option<String>,
}

impl CliInfo {
    /// The words of a command line that aren't options or their values.
    pub fn positionals<'a>(&self, args: &'a [String]) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut skip = false;
        for a in args {
            if skip {
                skip = false;
                continue;
            }
            if a == "--" {
                break;
            }
            if a.starts_with('-') && a.len() > 1 {
                skip = self.value_flags.iter().any(|f| f == a);
                continue;
            }
            out.push(a.as_str());
        }
        out
    }

    pub fn classify(&self, args: &[String]) -> CliMatch {
        let pos = self.positionals(args);
        let rule = self.rules.iter().find(|r| {
            let words = if r.command.is_empty() {
                pos.is_empty()
            } else {
                r.command.len() <= pos.len()
                    && r.command
                        .iter()
                        .zip(&pos)
                        .all(|(want, got)| want == "*" || want.split('|').any(|w| w == *got))
            };
            words
                && (r.flags.is_empty()
                    || r.flags
                        .iter()
                        .any(|f| flag_present(f, args, &pos[r.command.len().min(pos.len())..])))
        });
        let (effect, reason, resource) = match rule {
            Some(r) => (r.effect, r.reason.clone(), r.resource.clone()),
            None => (self.default, None, None),
        };
        let effect = match effect {
            CliEffect::ByMethod => by_method(args, &pos),
            e => e,
        };
        CliMatch {
            effect,
            reason,
            resource,
        }
    }
}

fn flag_present(f: &str, args: &[String], rest: &[&str]) -> bool {
    if let Some(prefix) = f.strip_prefix("arg:") {
        return rest.iter().any(|a| a.starts_with(prefix));
    }
    args.iter().any(|a| {
        a == f
            || (f.starts_with("--") && a.starts_with(&format!("{f}=")))
            || (!f.starts_with("--")
                && f.len() == 2
                && a.len() > 2
                && a.starts_with(f)
                && !a.starts_with("--"))
    })
}

fn flag_value<'a>(args: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        for n in names {
            if a == n {
                return it.next().map(String::as_str);
            }
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v);
            }
            if !n.starts_with("--") && n.len() == 2 && a.len() > 2 && a.starts_with(n) {
                return Some(&a[2..]);
            }
        }
    }
    None
}

fn by_method(args: &[String], pos: &[&str]) -> CliEffect {
    if pos
        .get(1)
        .is_some_and(|e| e.trim_start_matches('/') == "graphql")
    {
        return if args.iter().any(|a| a.contains("mutation")) {
            CliEffect::Write
        } else {
            CliEffect::Read
        };
    }
    let fields = args.iter().any(|a| {
        ["-f", "-F", "--field", "--raw-field", "--input"]
            .iter()
            .any(|f| a == f || a.starts_with(&format!("{f}=")))
    });
    match flag_value(args, &["-X", "--method"])
        .map(|m| m.to_ascii_uppercase())
        .as_deref()
    {
        Some("GET") | Some("HEAD") => CliEffect::Read,
        Some("DELETE") => CliEffect::Destructive,
        Some(_) => CliEffect::Write,
        None if fields => CliEffect::Write,
        None => CliEffect::Read,
    }
}

/// What a brokered command changed, for refreshing the app.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliChange {
    /// The connection, like `github`.
    pub connector: String,
    /// `gh pr merge 12 --squash`.
    pub command: String,
    pub effect: CliEffect,
    /// `pull`, `issue`, `repo`, `branch`, or empty when unknown.
    #[serde(default)]
    pub resource: String,
    /// `owner/name`, when tab could tell.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub number: Option<u64>,
    /// The page that shows what changed, when there is one.
    #[serde(default)]
    pub url: Option<String>,
}
