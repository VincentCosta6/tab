//! Settings: one page per part of the config, edited as a draft and saved
//! together. The counterpart of `src/modules/settings/SettingsTab.tsx`.

use gpui::{
    AppContext, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::Button as MenuButton;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::switch::Switch;
use tab_app::storage::StorageUsage;
use tab_core::TabConfig;
use tab_core::config::AiBackend;
use tab_core::model::AgentKind;

use crate::backend::backend;
use crate::components::{Button, ButtonKind, icon};
use crate::theme::{FONT_MONO, size, theme};

type Get = fn(&TabConfig) -> String;
type Set = fn(&mut TabConfig, &str);

enum Control {
    Line(Entity<InputState>, Get),
    Area(Entity<TextareaState>, Get),
    Toggle(fn(&TabConfig) -> bool, fn(&mut TabConfig, bool)),
    Choice(&'static [(&'static str, &'static str)], Get, Set),
}

struct Field {
    label: &'static str,
    help: &'static str,
    control: Control,
}

struct Section {
    page: Page,
    title: &'static str,
    description: &'static str,
    fields: Vec<Field>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Account,
    Hub,
    Agents,
    Ranking,
    Storage,
    Mcp,
    Updates,
}

impl Page {
    const ALL: [Page; 7] = [
        Page::Account,
        Page::Hub,
        Page::Agents,
        Page::Ranking,
        Page::Storage,
        Page::Mcp,
        Page::Updates,
    ];
    fn label(self) -> &'static str {
        match self {
            Page::Account => "Account",
            Page::Hub => "Hub",
            Page::Agents => "Agents",
            Page::Ranking => "Ranking",
            Page::Storage => "Storage",
            Page::Mcp => "MCP server",
            Page::Updates => "Updates",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Page::Account => "user",
            Page::Hub => "cloud",
            Page::Agents => "bot",
            Page::Ranking => "filter",
            Page::Storage => "folder",
            Page::Mcp => "terminal",
            Page::Updates => "refresh",
        }
    }
    /// Whether this page's part of the config differs between `a` and `b`,
    /// to mark it in the nav.
    fn changed(self, a: &TabConfig, b: &TabConfig) -> bool {
        match self {
            Page::Account => a.profile != b.profile,
            Page::Hub => a.hub != b.hub,
            Page::Agents => a.agent != b.agent || a.sandbox != b.sandbox,
            Page::Ranking => a.ranking != b.ranking,
            Page::Storage => a.storage != b.storage,
            Page::Mcp | Page::Updates => false,
        }
    }
}

const AGENTS: &[(&str, &str)] = &[
    ("", "Not set up"),
    ("claude_code", "Claude Code"),
    ("opencode", "OpenCode"),
    ("codex", "Codex"),
    ("pi", "Pi"),
];
const AI_BACKENDS: &[(&str, &str)] = &[
    (
        "auto",
        "Automatic (API key if set, otherwise your Claude plan)",
    ),
    ("claude_code", "Your Claude plan (Claude Code)"),
    ("api", "Anthropic API key"),
];
const CONTAINERS: &[(&str, &str)] = &[
    ("", "Whichever is installed"),
    ("docker", "Docker"),
    ("podman", "Podman"),
];
const PLAN_CAPS: &[(&str, &str)] = &[
    ("0.5", "Pause at 50%"),
    ("0.6", "Pause at 60%"),
    ("0.7", "Pause at 70%"),
    ("0.8", "Pause at 80%"),
    ("0.9", "Pause at 90%"),
    ("1", "Never pause"),
];

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Keeps blank lines out but otherwise what was typed, for path lists.
fn raw_lines(s: &str) -> Vec<String> {
    s.split('\n').map(String::from).collect()
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn dollars(v: Option<f64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

/// Empty is no limit; anything that isn't a number leaves it as it was.
fn parse_dollars(s: &str, old: Option<f64>) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().map(|v| Some(v.max(0.))).unwrap_or(old)
}

/// `word: +10` per line.
pub fn keywords_text(rules: &[tab_core::ranker::KeywordRule]) -> String {
    rules
        .iter()
        .map(|k| format!("{}: {:+}", k.contains, k.delta))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn parse_keywords(s: &str) -> Vec<tab_core::ranker::KeywordRule> {
    lines(s)
        .iter()
        .filter_map(|l| {
            let (word, delta) = l.rsplit_once(':')?;
            let delta = delta.trim().trim_start_matches('+').parse::<i16>().ok()?;
            let word = word.trim();
            (!word.is_empty()).then(|| tab_core::ranker::KeywordRule {
                contains: word.into(),
                delta,
            })
        })
        .collect()
}

/// "1.2 GB", "340 KB".
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024. && u < UNITS.len() - 1 {
        v /= 1024.;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub struct SettingsTab {
    page: Page,
    saved: TabConfig,
    draft: TabConfig,
    sections: Vec<Section>,
    note: Option<String>,
    busy: bool,
    usage: Option<StorageUsage>,
    sandbox: Option<String>,
    mcp: Option<(String, String)>,
    _subs: Vec<Subscription>,
}

impl SettingsTab {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let b = backend(cx);
        let saved = crate::runtime::handle().block_on(b.config_masked());
        let mut this = SettingsTab {
            page: Page::Account,
            draft: saved.clone(),
            saved,
            sections: Vec::new(),
            note: None,
            busy: false,
            usage: None,
            sandbox: None,
            mcp: None,
            _subs: Vec::new(),
        };
        this.build(window, cx);
        this.load_status(cx);
        this
    }

    fn line(
        &mut self,
        secret: bool,
        placeholder: &'static str,
        get: Get,
        set: Set,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Control {
        let value = get(&self.draft);
        let state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(secret)
                .placeholder(placeholder)
                .default_value(value)
        });
        self._subs
            .push(cx.subscribe(&state, move |this, s, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    let v = s.read(cx).value().to_string();
                    set(&mut this.draft, &v);
                    this.note = None;
                    cx.notify();
                }
            }));
        Control::Line(state, get)
    }

    fn area(
        &mut self,
        rows: usize,
        placeholder: &'static str,
        get: Get,
        set: Set,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Control {
        let value = get(&self.draft);
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(rows, rows.max(12))
                .placeholder(placeholder)
                .default_value(value)
        });
        self._subs
            .push(cx.subscribe(&state, move |this, s, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    let v = s.read(cx).value().to_string();
                    set(&mut this.draft, &v);
                    this.note = None;
                    cx.notify();
                }
            }));
        Control::Area(state, get)
    }

    /// Makes every page's fields. Called once; [`Self::reset`] refills them.
    fn build(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        macro_rules! field {
            ($label:expr, $help:expr, $control:expr) => {
                Field {
                    label: $label,
                    help: $help,
                    control: $control,
                }
            };
        }
        let profile = vec![
            field!(
                "Name",
                "",
                self.line(
                    false,
                    "",
                    |c| c.profile.name.clone(),
                    |c, v| c.profile.name = v.into(),
                    window,
                    cx
                )
            ),
            field!(
                "Role",
                "e.g. Staff engineer on payments",
                self.line(
                    false,
                    "",
                    |c| c.profile.role.clone(),
                    |c, v| c.profile.role = v.into(),
                    window,
                    cx
                )
            ),
            field!(
                "Handles",
                "Your usernames elsewhere, comma separated",
                self.line(
                    false,
                    "",
                    |c| c.profile.handles.join(", "),
                    |c, v| {
                        c.profile.handles = v
                            .split(',')
                            .map(str::trim)
                            .filter(|h| !h.is_empty())
                            .map(String::from)
                            .collect()
                    },
                    window,
                    cx
                )
            ),
        ];
        let agent = vec![
            field!(
                "Agent",
                "The coding agent tab runs for chats.",
                Control::Choice(
                    AGENTS,
                    |c| c
                        .agent
                        .kind
                        .map(|k| k.as_str().to_string())
                        .unwrap_or_default(),
                    |c, v| {
                        c.agent.kind = (!v.is_empty()).then(|| AgentKind::parse(v));
                        c.agent.model = c
                            .agent
                            .kind
                            .and_then(|k| c.agent.models.get(k.as_str()).cloned());
                    }
                )
            ),
            field!(
                "Command",
                "Leave empty to find it on PATH, or give a full path.",
                self.line(
                    false,
                    "",
                    |c| c.agent.command.clone().unwrap_or_default(),
                    |c, v| c.agent.command = opt(v),
                    window,
                    cx
                )
            ),
            field!(
                "Work folder",
                "Where sessions start.",
                self.line(
                    false,
                    "",
                    |c| c.agent.workdir.clone().unwrap_or_default(),
                    |c, v| c.agent.workdir = opt(v),
                    window,
                    cx
                )
            ),
            field!(
                "Model",
                "For new chats on this agent. Empty uses the agent's own default.",
                self.line(
                    false,
                    "",
                    |c| c.agent.model.clone().unwrap_or_default(),
                    |c, v| {
                        c.agent.model = opt(v);
                        if let Some(k) = c.agent.kind {
                            match opt(v) {
                                Some(m) => c.agent.models.insert(k.as_str().into(), m),
                                None => c.agent.models.remove(k.as_str()),
                            };
                        }
                    },
                    window,
                    cx
                )
            ),
            field!(
                "Strict mode",
                "Chats load only tab's connections, never your personal MCP servers, connectors, plugins, hooks or user settings.",
                Control::Toggle(|c| c.agent.strict, |c, v| c.agent.strict = v)
            ),
        ];
        let sandbox = vec![
            field!(
                "On",
                "Applies from the next message or run.",
                Control::Toggle(|c| c.sandbox.enabled, |c, v| c.sandbox.enabled = v)
            ),
            field!(
                "SSH keys in chats",
                "Off keeps ~/.ssh and your SSH agent out of chats, so GitHub is only reachable through tab.",
                Control::Toggle(
                    |c| c.sandbox.ssh_in_chats,
                    |c, v| c.sandbox.ssh_in_chats = v
                )
            ),
            field!(
                "Only what's needed",
                "Agents can't read, list or change anything in your home folder except the chat's own folder, the folders below, its project's repos, toolchains and caches.",
                Control::Toggle(|c| c.sandbox.confine, |c, v| c.sandbox.confine = v)
            ),
            field!(
                "Folders agents can use",
                "Your code folders, one per line.",
                self.area(
                    4,
                    "~/code",
                    |c| c.sandbox.folders.join("\n"),
                    |c, v| c.sandbox.folders = raw_lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Also protect",
                "Folders agents may read but not change, one per line.",
                self.area(
                    3,
                    "~/code/production-config",
                    |c| c.sandbox.protected.join("\n"),
                    |c, v| c.sandbox.protected = raw_lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Also hide",
                "Files or folders agents can't read at all, one per line.",
                self.area(
                    3,
                    "~/.config/acme/credentials",
                    |c| c.sandbox.hidden.join("\n"),
                    |c, v| c.sandbox.hidden = raw_lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Language servers may reach",
                "Hosts code intelligence on a pull request may fetch dependencies from, one per line.",
                self.area(
                    4,
                    "",
                    |c| c.sandbox.lsp_domains.join("\n"),
                    |c, v| c.sandbox.lsp_domains = raw_lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Containers",
                "Workflow commands that name an image need Docker or Podman running.",
                Control::Choice(
                    CONTAINERS,
                    |c| c.sandbox.container_runtime.clone(),
                    |c, v| c.sandbox.container_runtime = v.into()
                )
            ),
        ];
        let titles = vec![
            field!(
                "Enabled",
                "",
                Control::Toggle(
                    |c| c.agent.titler.enabled,
                    |c, v| c.agent.titler.enabled = v
                )
            ),
            field!(
                "Your rules",
                "Plain language. Added after the built-in rules, and yours win.",
                self.area(
                    4,
                    "Start with the repo name, e.g. \"tab: Fix badge overflow\".",
                    |c| c.agent.titler.instructions.clone(),
                    |c, v| c.agent.titler.instructions = v.into(),
                    window,
                    cx
                )
            ),
        ];
        let spend = vec![
            field!(
                "Daily limit",
                "New chat turns are refused once today's spend reaches this.",
                self.line(
                    false,
                    "No limit",
                    |c| dollars(c.agent.budget.daily_usd),
                    |c, v| c.agent.budget.daily_usd = parse_dollars(v, c.agent.budget.daily_usd),
                    window,
                    cx
                )
            ),
            field!(
                "Monthly limit",
                "",
                self.line(
                    false,
                    "No limit",
                    |c| dollars(c.agent.budget.monthly_usd),
                    |c, v| c.agent.budget.monthly_usd =
                        parse_dollars(v, c.agent.budget.monthly_usd),
                    window,
                    cx
                )
            ),
            field!(
                "Per turn",
                "Claude Code stops a single turn past this.",
                self.line(
                    false,
                    "No limit",
                    |c| dollars(c.agent.budget.per_turn_usd),
                    |c, v| c.agent.budget.per_turn_usd =
                        parse_dollars(v, c.agent.budget.per_turn_usd),
                    window,
                    cx
                )
            ),
            field!(
                "Save plan headroom",
                "On a Claude plan, reading the queue pauses once a usage window passes this.",
                Control::Choice(
                    PLAN_CAPS,
                    |c| c.agent.budget.ranker_plan_cap.to_string(),
                    |c, v| c.agent.budget.ranker_plan_cap = v.parse().unwrap_or(1.)
                )
            ),
        ];
        let rules = vec![
            field!(
                "Important people",
                "One per line. Their items get a boost.",
                self.area(
                    3,
                    "",
                    |c| c.ranking.rules.vip_actors.join("\n"),
                    |c, v| c.ranking.rules.vip_actors = lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Muted",
                "Channels, repos or senders, one per line. Scored 0 and never sent to the AI.",
                self.area(
                    3,
                    "",
                    |c| c.ranking.rules.muted.join("\n"),
                    |c, v| c.ranking.rules.muted = lines(v),
                    window,
                    cx
                )
            ),
            field!(
                "Keywords",
                "One per line as word: +10 or word: -15",
                self.area(
                    3,
                    "",
                    |c| keywords_text(&c.ranking.rules.keywords),
                    |c, v| c.ranking.rules.keywords = parse_keywords(v),
                    window,
                    cx
                )
            ),
        ];
        let ai = vec![
            field!(
                "Enabled",
                "",
                Control::Toggle(|c| c.ranking.ai.enabled, |c, v| c.ranking.ai.enabled = v)
            ),
            field!(
                "Runs on",
                "On your Claude plan it goes through the Claude Code CLI, so there's no key to manage.",
                Control::Choice(
                    AI_BACKENDS,
                    |c| match c.ranking.ai.backend {
                        AiBackend::Auto => "auto".into(),
                        AiBackend::Api => "api".into(),
                        AiBackend::ClaudeCode => "claude_code".into(),
                    },
                    |c, v| c.ranking.ai.backend = match v {
                        "api" => AiBackend::Api,
                        "claude_code" => AiBackend::ClaudeCode,
                        _ => AiBackend::Auto,
                    }
                )
            ),
            field!(
                "Anthropic API key",
                "Or set ANTHROPIC_API_KEY before launching.",
                self.line(
                    true,
                    "",
                    |c| c.ranking.ai.api_key.clone().unwrap_or_default(),
                    |c, v| c.ranking.ai.api_key = opt(v),
                    window,
                    cx
                )
            ),
            field!(
                "Model",
                "Defaults to claude-haiku-5-5",
                self.line(
                    false,
                    "claude-haiku-5-5",
                    |c| c.ranking.ai.model.clone().unwrap_or_default(),
                    |c, v| c.ranking.ai.model = opt(v),
                    window,
                    cx
                )
            ),
            field!(
                "Your instructions",
                "Plain language. What matters to you, and what can wait.",
                self.area(
                    5,
                    "Anything from on-call or my manager is urgent. Dependabot PRs can wait until Friday.",
                    |c| c.ranking.ai.instructions.clone(),
                    |c, v| c.ranking.ai.instructions = v.into(),
                    window,
                    cx
                )
            ),
        ];
        let storage = vec![
            field!(
                "Keep for (days)",
                "Attachments of chats idle this long, and review checkouts not opened this long, are removed.",
                self.line(
                    false,
                    "7",
                    |c| c.storage.keep_days.to_string(),
                    |c, v| {
                        if let Ok(d) = v.trim().parse::<u32>() {
                            c.storage.keep_days = d.max(1)
                        }
                    },
                    window,
                    cx
                )
            ),
            field!(
                "Size limit (GB)",
                "Past this, the least recently used attachments and checkouts are removed until it fits.",
                self.line(
                    false,
                    "No limit",
                    |c| c.storage.max_gb.map(|g| g.to_string()).unwrap_or_default(),
                    |c, v| c.storage.max_gb = v.trim().parse::<f64>().ok().filter(|g| *g > 0.),
                    window,
                    cx
                )
            ),
        ];
        self.sections = vec![
            Section {
                page: Page::Account,
                title: "About you",
                description: "Used by the rules (mentions of your handles) and given to the AI layer as context.",
                fields: profile,
            },
            Section {
                page: Page::Agents,
                title: "Agent",
                description: "The coding agent tab runs for chats. Tool approvals come to you in the chat; nothing runs with permissions bypassed.",
                fields: agent,
            },
            Section {
                page: Page::Agents,
                title: "Sandbox",
                description: "Agents, language servers on pull requests, and workflow commands run with limits.",
                fields: sandbox,
            },
            Section {
                page: Page::Agents,
                title: "Chat titles",
                description: "After the first few turns of a chat, a small model names it for what the thread is actually doing.",
                fields: titles,
            },
            Section {
                page: Page::Agents,
                title: "Spend limits",
                description: "Costs come from what the agent reports. Leave a limit empty for none.",
                fields: spend,
            },
            Section {
                page: Page::Ranking,
                title: "Rules",
                description: "Deterministic: the same item always gets the same base score. Changes re-rank the queue.",
                fields: rules,
            },
            Section {
                page: Page::Ranking,
                title: "AI layer",
                description: "Claude Haiku reads each item with your instructions and nudges the rules score by up to ±25, with a one-line reason.",
                fields: ai,
            },
            Section {
                page: Page::Storage,
                title: "Storage",
                description: "Cleared automatically every few hours. Review checkouts also go as soon as their PR closes or merges.",
                fields: storage,
            },
        ];
    }

    /// Refills every field from the draft, after a save or discard.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for s in &self.sections {
            for f in &s.fields {
                match &f.control {
                    Control::Line(state, get) => {
                        let v = get(&self.draft);
                        state.update(cx, |s, cx| s.set_value(v, window, cx));
                    }
                    Control::Area(state, get) => {
                        let v = get(&self.draft);
                        state.update(cx, |s, cx| s.set_value(v, window, cx));
                    }
                    Control::Toggle(..) | Control::Choice(..) => {}
                }
            }
        }
        cx.notify();
    }

    fn load_status(&mut self, cx: &mut Context<Self>) {
        let b = backend(cx);
        let task = crate::runtime::spawn(async move {
            let usage = b.storage_usage().await.ok();
            let sandbox = tab_core::sandbox::current().status().await;
            let sandbox = match sandbox.fix {
                Some(fix) => format!("{} {fix}", sandbox.summary),
                None => sandbox.summary,
            };
            let url = b.mcp_url.lock().unwrap().clone();
            let token = b.config.lock().await.mcp.token.clone();
            (usage, sandbox, url.map(|u| (u, token)))
        });
        cx.spawn(async move |this, cx| {
            if let Ok((usage, sandbox, mcp)) = task.await {
                this.update(cx, |this, cx| {
                    this.usage = usage;
                    this.sandbox = Some(sandbox);
                    this.mcp = mcp;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        let b = backend(cx);
        let draft = self.draft.clone();
        let task = crate::runtime::spawn(async move { b.config_save(draft).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.and_then(|r| r);
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match r {
                    Ok(c) => {
                        this.saved = c.clone();
                        this.draft = c;
                        this.note = Some(
                            "Saved. Open items are being re-ranked; unchanged AI verdicts come from cache."
                                .into(),
                        );
                        this.reset(window, cx);
                    }
                    Err(e) => this.note = Some(format!("Couldn't save: {e}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = self.saved.clone();
        self.note = None;
        self.reset(window, cx);
    }

    fn clean(&mut self, which: Option<Option<(String, u64)>>, cx: &mut Context<Self>) {
        self.busy = true;
        let b = backend(cx);
        let task = crate::runtime::spawn(async move {
            let s = match which {
                None => b.storage_clean().await,
                Some(which) => b.storage_remove_checkouts(which).await,
            };
            (s, b.storage_usage().await.ok())
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                if let Ok((s, usage)) = r {
                    this.note = Some(if s.removed > 0 {
                        format!("Freed {}.", bytes(s.bytes))
                    } else {
                        "Nothing to clear.".into()
                    });
                    this.usage = usage;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_control(
        &self,
        i: usize,
        j: usize,
        c: &Control,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme(cx).clone();
        match c {
            Control::Line(state, _) => Input::new(state).into_any_element(),
            Control::Area(state, _) => Textarea::new(state).into_any_element(),
            Control::Toggle(get, set) => {
                let set = *set;
                let this = cx.entity().downgrade();
                Switch::new(SharedString::from(format!("sw-{i}-{j}")))
                    .checked(get(&self.draft))
                    .on_click(move |v, _, cx| {
                        let v = *v;
                        this.update(cx, |this, cx| {
                            set(&mut this.draft, v);
                            this.note = None;
                            cx.notify();
                        })
                        .ok();
                    })
                    .into_any_element()
            }
            Control::Choice(options, get, set) => {
                let set = *set;
                let current = get(&self.draft);
                let label = options
                    .iter()
                    .find(|(v, _)| *v == current)
                    .map(|(_, l)| *l)
                    .unwrap_or(options[0].1);
                let this = cx.entity().downgrade();
                let options = *options;
                div()
                    .child(
                        MenuButton::new(SharedString::from(format!("ch-{i}-{j}")))
                            .outline()
                            .label(label)
                            .dropdown_menu(move |mut menu, _, _| {
                                for (value, label) in options {
                                    let this = this.clone();
                                    menu = menu.item(PopupMenuItem::new(*label).on_click(
                                        move |_, _, cx| {
                                            this.update(cx, |this, cx| {
                                                set(&mut this.draft, value);
                                                this.note = None;
                                                cx.notify();
                                            })
                                            .ok();
                                        },
                                    ));
                                }
                                menu
                            }),
                    )
                    .text_color(t.fg)
                    .into_any_element()
            }
        }
    }

    fn render_section(&self, i: usize, s: &Section, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(size::RADIUS_LG))
            .border_1()
            .border_color(t.border)
            .bg(t.bg_raised);
        for (j, f) in s.fields.iter().enumerate() {
            card = card.child(row(
                f.label,
                f.help,
                self.render_control(i, j, &f.control, cx),
                j > 0,
                cx,
            ));
        }
        section(s.title, s.description, card, cx)
    }

    fn render_extras(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let t = theme(cx).clone();
        let mono = |s: String| {
            div()
                .font_family(FONT_MONO)
                .text_size(px(size::TEXT_SM))
                .text_color(t.fg_muted)
                .child(s)
                .into_any_element()
        };
        match self.page {
            Page::Agents => {
                let status = self.sandbox.clone().unwrap_or_else(|| "Checking…".into());
                vec![
                    section(
                        "On this computer",
                        "How the sandbox works here.",
                        card(cx).child(row(
                            "Sandbox",
                            "",
                            div()
                                .text_color(t.fg_muted)
                                .child(status)
                                .into_any_element(),
                            false,
                            cx,
                        )),
                        cx,
                    )
                    .into_any_element(),
                ]
            }
            Page::Storage => {
                let size = |s: Option<tab_core::storage::Size>| {
                    s.map(|s| format!("{} · {} files", bytes(s.bytes), s.files))
                        .unwrap_or_else(|| "…".into())
                };
                let u = self.usage.clone();
                let mut c =
                    card(cx)
                        .child(row(
                            "Chat attachments",
                            "Files you attach to chats.",
                            mono(size(u.as_ref().map(|u| u.attachments))),
                            false,
                            cx,
                        ))
                        .child(row(
                            "Chat folders",
                            "Each chat's prompt and settings for its agent.",
                            mono(size(u.as_ref().map(|u| u.sessions))),
                            true,
                            cx,
                        ))
                        .child(row(
                            "Review checkouts",
                            "Local copies of repositories for PR reviews.",
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(mono(
                                    u.as_ref()
                                        .map(|u| bytes(u.workspaces.bytes))
                                        .unwrap_or_else(|| "…".into()),
                                ))
                                .when(u.as_ref().is_some_and(|u| !u.checkouts.is_empty()), |el| {
                                    el.child(
                                        Button::new("rm-all")
                                            .label("Remove all")
                                            .disabled(self.busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.clean(Some(None), cx)
                                            })),
                                    )
                                })
                                .into_any_element(),
                            true,
                            cx,
                        ));
                for (n, co) in u
                    .map(|u| u.checkouts)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    let which = (co.repo.clone(), co.number);
                    c = c.child(row_owned(
                        format!("{} #{}", co.repo, co.number),
                        match crate::components::ago(co.last_used, chrono::Utc::now()) {
                            a if a == "now" => "Opened just now".into(),
                            a => format!("Opened {a} ago"),
                        },
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(mono(bytes(co.bytes)))
                            .child(
                                Button::new(SharedString::from(format!("rm-{n}")))
                                    .kind(ButtonKind::Ghost)
                                    .icon("trash")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.clean(Some(Some(which.clone())), cx)
                                    })),
                            )
                            .into_any_element(),
                        cx,
                    ));
                }
                vec![
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            Button::new("clean")
                                .label(if self.busy {
                                    "Cleaning…"
                                } else {
                                    "Clean up now"
                                })
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.clean(None, cx))),
                        )
                        .into_any_element(),
                    section("On disk", "What tab keeps on this computer.", c, cx)
                        .into_any_element(),
                ]
            }
            Page::Mcp => {
                let (url, token) = self
                    .mcp
                    .clone()
                    .map(|(u, t)| (u, Some(t)))
                    .unwrap_or_else(|| ("not running".into(), None));
                vec![section(
                    "MCP server",
                    "Agents tab starts are connected automatically. Point any other MCP client here to let it search your queue, file what it finds, and create tasks.",
                    card(cx)
                        .child(row("URL", "", mono(url), false, cx))
                        .child(row(
                            "Token",
                            "Send as Authorization: Bearer …",
                            Button::new("copy-token")
                                .icon("copy")
                                .label("Copy token")
                                .disabled(token.is_none())
                                .on_click(move |_, _, cx| {
                                    if let Some(t) = &token {
                                        cx.write_to_clipboard(ClipboardItem::new_string(t.clone()))
                                    }
                                })
                                .into_any_element(),
                            true,
                            cx,
                        )),
                    cx,
                )
                .into_any_element()]
            }
            Page::Hub => {
                let (mode, help) = match &self.draft.hub.cloud_hub {
                    Some(id) => (
                        format!("tab cloud ({id})"),
                        "Your work syncs through tab cloud.",
                    ),
                    None => (
                        "This computer".to_string(),
                        "Your work lives on this computer. Moving it to tab cloud is in the Tauri app for now.",
                    ),
                };
                vec![
                    section(
                        "Hub",
                        "Where your queue, chats and projects live.",
                        card(cx).child(row("Kept on", help, mono(mode), false, cx)),
                        cx,
                    )
                    .into_any_element(),
                ]
            }
            Page::Updates => vec![
                section(
                    "Version",
                    "The GPUI app updates with the rest of tab once it ships.",
                    card(cx).child(row(
                        "tab",
                        "",
                        mono(env!("CARGO_PKG_VERSION").into()),
                        false,
                        cx,
                    )),
                    cx,
                )
                .into_any_element(),
            ],
            Page::Account | Page::Ranking => Vec::new(),
        }
    }
}

fn card(cx: &mut Context<SettingsTab>) -> gpui::Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .rounded(px(size::RADIUS_LG))
        .border_1()
        .border_color(t.border)
        .bg(t.bg_raised)
}

fn section(
    title: &'static str,
    description: &'static str,
    body: impl IntoElement,
    cx: &mut Context<SettingsTab>,
) -> impl IntoElement {
    let t = theme(cx).clone();
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(size::TEXT_LG))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(t.fg)
                        .child(title),
                )
                .when(!description.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(size::TEXT_MD))
                            .text_color(t.fg_muted)
                            .child(description),
                    )
                }),
        )
        .child(body)
}

fn row(
    label: &'static str,
    help: &'static str,
    control: gpui::AnyElement,
    divider: bool,
    cx: &mut Context<SettingsTab>,
) -> impl IntoElement {
    row_inner(label.into(), help.into(), control, divider, cx)
}

fn row_owned(
    label: String,
    help: String,
    control: gpui::AnyElement,
    cx: &mut Context<SettingsTab>,
) -> impl IntoElement {
    row_inner(label.into(), help.into(), control, true, cx)
}

fn row_inner(
    label: SharedString,
    help: SharedString,
    control: gpui::AnyElement,
    divider: bool,
    cx: &mut Context<SettingsTab>,
) -> impl IntoElement {
    let t = theme(cx).clone();
    div()
        .flex()
        .gap(px(24.))
        .px(px(16.))
        .py(px(12.))
        .when(divider, |el| el.border_t_1().border_color(t.border))
        .child(
            div()
                .w(px(220.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(size::TEXT_MD))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(t.fg)
                        .child(label),
                )
                .when(!help.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(size::TEXT_SM))
                            .text_color(t.fg_subtle)
                            .child(help),
                    )
                }),
        )
        .child(div().flex_1().min_w_0().child(control))
}

impl Render for SettingsTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let dirty = self.draft != self.saved;
        let mut nav = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .w(px(220.))
            .flex_none()
            .h_full()
            .px(px(12.))
            .py(px(20.))
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .px(px(8.))
                    .pb(px(12.))
                    .text_size(px(size::TEXT_XL))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Settings"),
            );
        for p in Page::ALL {
            let active = p == self.page;
            let changed = p.changed(&self.draft, &self.saved);
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("settings-{}", p.label())))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(size::RADIUS_MD))
                    .cursor_pointer()
                    .text_size(px(size::TEXT_MD))
                    .when(active, |el| el.bg(t.bg_raised).text_color(t.fg))
                    .when(!active, |el| {
                        el.text_color(t.fg_muted).hover(|s| s.text_color(t.fg))
                    })
                    .child(icon(p.icon(), 15., if active { t.fg } else { t.fg_muted }))
                    .child(div().flex_1().child(p.label()))
                    .when(changed, |el| {
                        el.child(div().size(px(6.)).rounded_full().bg(t.accent.fg))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = p;
                        cx.notify();
                    })),
            );
        }
        let mut page = div()
            .flex()
            .flex_col()
            .gap(px(28.))
            .max_w(px(820.))
            .px(px(40.))
            .py(px(28.));
        let sections: Vec<_> = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.page == self.page)
            .map(|(i, s)| self.render_section(i, s, cx).into_any_element())
            .collect();
        page = page.children(sections).children(self.render_extras(cx));
        let savebar = (dirty || self.note.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(40.))
                .py(px(10.))
                .border_t_1()
                .border_color(t.border)
                .bg(t.bg_raised)
                .text_size(px(size::TEXT_MD))
                .child(
                    self.note
                        .clone()
                        .unwrap_or_else(|| "You have unsaved changes.".into()),
                )
                .when(dirty, |el| {
                    el.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("discard")
                                    .kind(ButtonKind::Ghost)
                                    .label("Discard")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, w, cx| this.discard(w, cx))),
                            )
                            .child(
                                Button::new("save")
                                    .kind(ButtonKind::Primary)
                                    .label(if self.busy {
                                        "Saving…"
                                    } else {
                                        "Save changes"
                                    })
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, w, cx| this.save(w, cx))),
                            ),
                    )
                })
        });
        div().size_full().flex().bg(t.bg).child(nav).child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("settings-page")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(page),
                )
                .children(savebar),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_round_trip() {
        let k = parse_keywords("outage: +20\n  dependabot: -15\nnonsense\n: 4");
        assert_eq!(k.len(), 2);
        assert_eq!(keywords_text(&k), "outage: +20\ndependabot: -15");
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_limit_that_is_not_a_number_is_left_alone() {
        assert_eq!(parse_dollars("", Some(5.)), None);
        assert_eq!(parse_dollars("abc", Some(5.)), Some(5.));
        assert_eq!(parse_dollars("-3", None), Some(0.));
    }
}
