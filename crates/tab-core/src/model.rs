//! Domain types shared by connectors, the engine, the store and the UI.
//!
//! Everything the UI shows about an item is data: connectors describe their
//! kinds (label, icon, tone) and item content as generic [`Block`]s, and the
//! design system renders those. Adding a connector should never require a
//! new UI component unless it brings a whole new surface (like code review).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Something a connector observed that may need a human.
///
/// `(source, external_id)` identifies the thing across polls, so a PR that
/// gets a new comment updates the existing queue item instead of duplicating it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IncomingEvent {
    /// Connector id, e.g. `github`, `slack`, `demo`.
    pub source: String,
    /// Stable id within the source (GitHub notification thread id, Slack ts, ...).
    pub external_id: String,
    /// Dotted kind, e.g. `github.review_requested`. The prefix names the app
    /// (`github`, `slack`, `email`, `meeting`, `agent`) and picks UI surfaces.
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<String>,
    pub actor: Option<String>,
    /// Where it happened: repo, channel, mailbox, meeting.
    pub context: Option<String>,
    pub occurred_at: DateTime<Utc>,
    /// Cheap rich content known at ingest time. Richer content comes from
    /// `Connector::preview`, served through the gateway cache.
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// Things this item is about. Feeds the knowledge graph.
    #[serde(default)]
    pub entities: Vec<EntityRef>,
    /// Source-specific payload for plugin surfaces (PR number, repo, ...).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Rich content
// ---------------------------------------------------------------------------

/// Semantic color. The design system maps tones to tokens; connectors never pick colors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Info,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub tone: Tone,
}

impl Field {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            tone: Tone::Neutral,
        }
    }
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// A unit of rich content. The UI has one component per variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Paragraph text. `markdown` renders basic markdown.
    Text {
        text: String,
        #[serde(default)]
        markdown: bool,
    },
    /// Someone's words, e.g. a comment or message.
    Quote {
        text: String,
        #[serde(default)]
        author: Option<String>,
    },
    /// Label/value rows.
    Fields {
        items: Vec<Field>,
    },
    /// Big-number row, e.g. `+612` `−203` `14 files`.
    Stats {
        items: Vec<Field>,
    },
    /// Small pills.
    Badges {
        items: Vec<Field>,
    },
    Links {
        items: Vec<Link>,
    },
    Code {
        text: String,
        #[serde(default)]
        language: Option<String>,
    },
    /// A formatted email (or similar) body. The UI renders it isolated, with
    /// scripts off and remote images blocked until the person loads them.
    Html {
        html: String,
    },
    /// A question with options, e.g. an agent asking which approach to take.
    Choices {
        prompt: String,
        options: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Knowledge graph
// ---------------------------------------------------------------------------

/// A reference to a thing in the world: a person, repo, channel, PR, doc.
/// `(kind, key)` is the identity; connectors should use canonical keys
/// (`github:acme/api#12`, `slack:C123`, an email address) so the same thing
/// seen from two connectors becomes one node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EntityRef {
    pub kind: String,
    pub key: String,
    pub label: String,
    /// How the item relates to it: `from`, `in`, `about`, `mentions`.
    #[serde(default = "default_rel")]
    pub rel: String,
}

fn default_rel() -> String {
    "about".into()
}

impl EntityRef {
    pub fn new(kind: &str, key: impl Into<String>, label: impl Into<String>, rel: &str) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
            label: label.into(),
            rel: rel.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    pub id: i64,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub item_count: i64,
}

// ---------------------------------------------------------------------------
// Ranking and triage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Now,
    Today,
    ThisWeek,
    Whenever,
}

impl Urgency {
    pub fn as_str(self) -> &'static str {
        match self {
            Urgency::Now => "now",
            Urgency::Today => "today",
            Urgency::ThisWeek => "this_week",
            Urgency::Whenever => "whenever",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "now" => Urgency::Now,
            "today" => Urgency::Today,
            "this_week" => Urgency::ThisWeek,
            "whenever" => Urgency::Whenever,
            _ => return None,
        })
    }

    /// Urgency is a pure function of the final score, so it is deterministic too.
    pub fn from_score(score: u8) -> Self {
        match score {
            80.. => Urgency::Now,
            60..=79 => Urgency::Today,
            35..=59 => Urgency::ThisWeek,
            _ => Urgency::Whenever,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ranking {
    /// Final score, 0 (ignore) to 100 (drop everything).
    pub score: u8,
    /// Score from the deterministic rules alone.
    pub base_score: u8,
    /// Bounded nudge from the AI layer, if it ran.
    pub ai_adjustment: Option<i16>,
    pub urgency: Urgency,
    /// Short explanation shown under the title.
    pub reason: String,
    /// e.g. `rules` or `rules+claude-haiku-5-5`.
    pub ranked_by: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Snoozed,
    Done,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Open => "open",
            ItemState::Snoozed => "snoozed",
            ItemState::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "snoozed" => ItemState::Snoozed,
            "done" => ItemState::Done,
            _ => ItemState::Open,
        }
    }
}

/// A row in the queue: the latest event for a `(source, external_id)` plus
/// its ranking and triage state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueItem {
    pub id: String,
    #[serde(flatten)]
    pub event: IncomingEvent,
    pub state: ItemState,
    pub ranking: Option<Ranking>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The project the person put this in, if any.
    #[serde(default)]
    pub project_id: Option<String>,
}

/// A group of work the person defines: chats started in it belong to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// CSS color, e.g. `#7c5cff`.
    pub color: String,
    /// Icon name from the design system.
    pub icon: String,
    pub created_at: DateTime<Utc>,
}

/// A durable fact about a project, left by an agent or the person, so the
/// next chat starts with it. One note per topic: writing a topic again
/// replaces it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectNote {
    pub id: String,
    pub project_id: String,
    pub topic: String,
    pub text: String,
    /// `agent` or `user`.
    pub added_by: String,
    pub session_id: Option<String>,
    /// Item ids the note is based on.
    pub sources: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Who the queue is for. Fed to the AI layer so "important" means important to *this* person.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UserProfile {
    #[serde(default)]
    pub name: String,
    /// e.g. "Staff engineer on the payments team".
    #[serde(default)]
    pub role: String,
    /// Handles that mean "me" in other systems (GitHub login, Slack name, email).
    #[serde(default)]
    pub handles: Vec<String>,
}

// ---------------------------------------------------------------------------
// Connector metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ConnectorStatus {
    Disabled,
    Starting,
    Running { last_sync: Option<DateTime<Utc>> },
    RateLimited { until: DateTime<Utc> },
    Error { message: String },
    Stopped,
}

/// How the UI should show items of one kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KindInfo {
    pub kind: String,
    pub label: String,
    /// Icon name from the design system's icon set.
    pub icon: String,
    #[serde(default)]
    pub tone: Tone,
}

impl KindInfo {
    pub fn new(kind: &str, label: &str, icon: &str, tone: Tone) -> Self {
        Self {
            kind: kind.into(),
            label: label.into(),
            icon: icon.into(),
            tone,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Text,
    Secret,
    Number,
    Toggle,
}

/// One field of a connector's settings form. The Connections tab renders
/// these generically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Static description of a connector type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorDescriptor {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub kinds: Vec<KindInfo>,
    pub settings: Vec<SettingField>,
    /// False for connectors that are designed but not built yet.
    pub available: bool,
    /// Limits the provider enforces. The gateway budgets against these.
    #[serde(default)]
    pub rate_limits: Vec<crate::ratelimit::RateLimitSpec>,
    /// Marketplace shelf: `built_in`, `work`, `code`, `communication`, `agents`.
    #[serde(default)]
    pub category: String,
    /// Extra capabilities shown in the marketplace (e.g. a review tool).
    #[serde(default)]
    pub features: Vec<String>,
    /// Built-in connectors can't be uninstalled.
    #[serde(default)]
    pub builtin: bool,
    /// What workflows can ask this connector to do.
    #[serde(default)]
    pub actions: Vec<ActionInfo>,
}

// ---------------------------------------------------------------------------
// Actions: what a connector can do, as data
// ---------------------------------------------------------------------------

/// How much an action changes the world. Workflows gate on this.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads only. Runs in dry runs too.
    #[default]
    Read,
    /// Changes something, in tab or upstream (a comment, a label, a snooze).
    Write,
    /// Hard to undo or seen by other people (merge, close, move mail).
    /// Only runs once the person allows destructive steps on that workflow.
    Destructive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    #[default]
    Text,
    /// Multi-line text, usually markdown.
    LongText,
    Number,
    Bool,
    /// One of `options`.
    Select,
    /// Comma separated, sent as a list.
    List,
}

/// One input or output of an action. The builder renders inputs as a form.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ActionField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type", default)]
    pub ty: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Template filled in when the step is added, e.g. `{{item.metadata.repo}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl ActionField {
    pub fn new(key: &str, label: &str, ty: FieldType) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ty,
            ..Default::default()
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn help(mut self, h: &str) -> Self {
        self.help = Some(h.into());
        self
    }
    pub fn default(mut self, d: &str) -> Self {
        self.default = Some(d.into());
        self
    }
    pub fn options(mut self, o: &[&str]) -> Self {
        self.options = o.iter().map(|s| s.to_string()).collect();
        self
    }
}

/// Something a connector can do when a workflow asks. Described as data so
/// the builder, agents and validation all read the same thing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ActionInfo {
    /// Unique within the connector, dotted: `pr.comment`, `item.snooze`.
    pub id: String,
    pub label: String,
    pub description: String,
    pub icon: String,
    pub effect: Effect,
    #[serde(default)]
    pub inputs: Vec<ActionField>,
    /// What the step's result holds, for `{{steps.<id>.<key>}}`.
    #[serde(default)]
    pub outputs: Vec<ActionField>,
    /// Rate bucket it spends from, `provider/resource`, if it calls an API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spends: Option<String>,
    /// Item kinds (exact or `prefix.*`) the action makes sense for, so the
    /// builder can suggest it. Empty means any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub for_kinds: Vec<String>,
}

impl ActionInfo {
    pub fn new(id: &str, label: &str, icon: &str, effect: Effect, description: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: icon.into(),
            effect,
            description: description.into(),
            ..Default::default()
        }
    }
    pub fn inputs(mut self, f: Vec<ActionField>) -> Self {
        self.inputs = f;
        self
    }
    pub fn outputs(mut self, f: Vec<ActionField>) -> Self {
        self.outputs = f;
        self
    }
    pub fn spends(mut self, bucket: &str) -> Self {
        self.spends = Some(bucket.into());
        self
    }
    pub fn for_kinds(mut self, k: &[&str]) -> Self {
        self.for_kinds = k.iter().map(|s| s.to_string()).collect();
        self
    }
}

/// A connector type plus its live state, for the Connections tab.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorInfo {
    #[serde(flatten)]
    pub descriptor: ConnectorDescriptor,
    pub enabled: bool,
    pub status: ConnectorStatus,
}

// ---------------------------------------------------------------------------
// Context, sessions, approvals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSection {
    /// Given to the agent with every message.
    Context,
    /// Suggested by an agent or the graph; the person can promote it.
    Found,
}

impl ContextSection {
    pub fn as_str(self) -> &'static str {
        match self {
            ContextSection::Context => "context",
            ContextSection::Found => "found",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "found" {
            ContextSection::Found
        } else {
            ContextSection::Context
        }
    }
}

/// One piece of context attached to an item: another item, or a link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextEntry {
    pub id: String,
    pub item_id: String,
    pub section: ContextSection,
    pub pinned: bool,
    /// Set when the context is another queue item; the UI shows its rich preview.
    pub ref_item: Option<Box<QueueItem>>,
    pub url: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    /// `user`, `agent` or `graph`.
    pub added_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewContext {
    pub section: ContextSection,
    #[serde(default)]
    pub ref_item_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub added_by: String,
}

/// Everything known around an item, rolled up for the chat view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemContext {
    pub item: QueueItem,
    pub entries: Vec<ContextEntry>,
    /// Graph neighbors not yet in `entries`; shown under Found.
    pub related: Vec<QueueItem>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    OpenCode,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude_code",
            AgentKind::OpenCode => "opencode",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "opencode" {
            AgentKind::OpenCode
        } else {
            AgentKind::ClaudeCode
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Working,
    /// Waiting on the person (an approval or a question).
    NeedsInput,
    Failed,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Working => "working",
            SessionStatus::NeedsInput => "needs_input",
            SessionStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "working" => SessionStatus::Working,
            "needs_input" => SessionStatus::NeedsInput,
            "failed" => SessionStatus::Failed,
            _ => SessionStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub item_id: Option<String>,
    pub agent: AgentKind,
    pub agent_session_id: Option<String>,
    pub title: String,
    pub status: SessionStatus,
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    pub id: i64,
    pub session_id: String,
    /// `user`, `assistant`, `tool` or `system`.
    pub role: String,
    /// `text`, `tool`, `status` or `error`.
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub meta: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Rich preview blocks for an item, cached so rendering never waits on an API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preview {
    pub item_id: String,
    pub blocks: Vec<Block>,
    pub fetched_at: Option<DateTime<Utc>>,
    /// Older than the connector's preview TTL; a background refresh is queued.
    pub stale: bool,
}

/// An agent asking permission to do something, shown in the chat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub id: String,
    pub session_id: String,
    pub tool: String,
    pub summary: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

/// One thing that happened to an item, for its timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    pub id: i64,
    pub item_id: String,
    pub at: DateTime<Utc>,
    /// `arrived`, `updated`, `snoozed`, `woke`, `done`, `reopened`,
    /// `finished_upstream`, `project`, `linked`, `session_started`,
    /// `session_needs_input`, `session_failed`, `workflow_run`.
    pub kind: String,
    pub title: String,
    /// `you`, `agent`, `upstream`, `workflow:<name>`, or who sent it.
    pub by: Option<String>,
    #[serde(default)]
    pub detail: serde_json::Value,
    /// Set when it happened on a linked item (a PR in the task's context),
    /// with that item's title.
    #[serde(default)]
    pub via: Option<String>,
}

/// A task created in tab, by the person or an agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// `low`, `normal`, `high` or `urgent`.
    #[serde(default)]
    pub priority: Option<String>,
    /// Scheduled tasks stay snoozed until this time, then wake into the queue.
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub url: Option<String>,
    /// `user` or `agent`.
    #[serde(default)]
    pub created_by: String,
    /// Project to put the task in.
    #[serde(default)]
    pub project_id: Option<String>,
}
