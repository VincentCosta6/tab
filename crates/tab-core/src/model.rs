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
}

/// A connector type plus its live state, for the Connections tab.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorInfo {
    #[serde(flatten)]
    pub descriptor: ConnectorDescriptor,
    pub enabled: bool,
    pub status: ConnectorStatus,
}
