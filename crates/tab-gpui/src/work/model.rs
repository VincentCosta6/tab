//! What the queue shows, worked out from items alone: groups, counts,
//! bylines and kinds. No GPUI here, so it's tested on its own.

use std::collections::HashMap;

use tab_core::model::{ConnectorInfo, KindInfo, QueueItem, Tone, Urgency};

pub const URGENCY_ORDER: [Urgency; 4] = [
    Urgency::Now,
    Urgency::Today,
    Urgency::ThisWeek,
    Urgency::Whenever,
];

pub fn urgency_heading(u: Urgency) -> &'static str {
    match u {
        Urgency::Now => "Now",
        Urgency::Today => "Today",
        Urgency::ThisWeek => "This week",
        Urgency::Whenever => "Whenever",
    }
}

/// Apps (kind prefixes) for the filter chips: id, label, icon.
pub const APPS: &[(&str, &str, &str)] = &[
    ("tab", "Tasks", "tab"),
    ("github", "GitHub", "github"),
    ("slack", "Slack", "slack"),
    ("email", "Email", "mail"),
    ("meeting", "Meetings", "video"),
    ("jira", "Jira", "ticket"),
    ("linear", "Linear", "ticket"),
    ("notion", "Notion", "file"),
    ("sentry", "Sentry", "alert"),
    ("agent", "Agents", "bot"),
];

/// The app an item's kind belongs to: `github.review_requested` → `github`.
pub fn app_of(kind: &str) -> &str {
    kind.split('.').next().unwrap_or(kind)
}

/// Who and where, under a row's title.
pub fn byline(item: &QueueItem) -> String {
    let e = &item.event;
    let same = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case(&e.source))
    };
    let actor = if e.source == "tab" && e.actor.as_deref() == Some("you") {
        None
    } else {
        e.actor.clone()
    };
    let context = if same(&e.context) {
        None
    } else {
        e.context.clone()
    };
    [actor, context]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// How many items each app has.
pub fn counts(items: &[QueueItem]) -> HashMap<String, usize> {
    let mut c = HashMap::new();
    for i in items {
        *c.entry(app_of(&i.event.kind).to_string()).or_insert(0) += 1;
    }
    c
}

/// Items grouped under urgency headings, in order; an item with no ranking
/// yet counts as today, as in the React queue. Follow-ups whose parent is
/// in the list fold under it, so they're left out here.
pub fn grouped(items: &[QueueItem]) -> Vec<(Urgency, Vec<&QueueItem>)> {
    let ids: std::collections::HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
    URGENCY_ORDER
        .iter()
        .map(|u| {
            let rows: Vec<&QueueItem> = items
                .iter()
                .filter(|i| !i.parent_id.as_deref().is_some_and(|p| ids.contains(p)))
                .filter(|i| i.ranking.as_ref().map_or(Urgency::Today, |r| r.urgency) == *u)
                .collect();
            (*u, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// Kind labels, icons and tones, from the connectors.
#[derive(Default, Clone)]
pub struct Kinds(HashMap<String, KindInfo>);

impl Kinds {
    pub fn from_connectors(cs: &[ConnectorInfo]) -> Self {
        Kinds(
            cs.iter()
                .flat_map(|c| c.descriptor.kinds.iter().cloned())
                .map(|k| (k.kind.clone(), k))
                .collect(),
        )
    }

    pub fn get(&self, kind: &str) -> KindInfo {
        if let Some(k) = self.0.get(kind) {
            return k.clone();
        }
        let app = app_of(kind);
        let icon = APPS
            .iter()
            .find(|(id, _, _)| *id == app)
            .map_or("dot", |a| a.2);
        let rest = kind
            .split_once('.')
            .map_or(kind, |(_, r)| r)
            .replace('_', " ");
        KindInfo::new(
            kind,
            if rest.is_empty() { kind } else { &rest },
            icon,
            Tone::Neutral,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tab_core::model::{IncomingEvent, ItemState, Ranking};

    fn item(id: &str, kind: &str, score: Option<u8>) -> QueueItem {
        QueueItem {
            id: id.into(),
            event: IncomingEvent {
                source: app_of(kind).into(),
                external_id: id.into(),
                kind: kind.into(),
                title: id.into(),
                body: None,
                url: None,
                actor: Some("dana".into()),
                context: Some("acme/api".into()),
                occurred_at: Utc::now(),
                blocks: vec![],
                entities: vec![],
                metadata: serde_json::Value::Null,
            },
            state: ItemState::Open,
            ranking: score.map(|s| Ranking {
                score: s,
                base_score: s,
                ai_adjustment: None,
                urgency: Urgency::from_score(s),
                reason: String::new(),
                ranked_by: "rules".into(),
                fired: vec![],
            }),
            snoozed_until: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_id: None,
            parent_id: None,
            blocked_by: vec![],
        }
    }

    #[test]
    fn groups_in_urgency_order_and_skip_empty_ones() {
        let items = vec![
            item("a", "github.ci", Some(40)),
            item("b", "slack.dm", Some(92)),
            item("c", "tab.task", None),
        ];
        let g = grouped(&items);
        let heads: Vec<_> = g.iter().map(|(u, rows)| (*u, rows.len())).collect();
        assert_eq!(
            heads,
            vec![
                (Urgency::Now, 1),
                (Urgency::Today, 1),
                (Urgency::ThisWeek, 1)
            ]
        );
    }

    #[test]
    fn followups_fold_under_their_parent() {
        let mut child = item("c", "tab.task", Some(90));
        child.parent_id = Some("p".into());
        let orphan = {
            let mut o = item("o", "tab.task", Some(90));
            o.parent_id = Some("gone".into());
            o
        };
        let items = vec![item("p", "tab.task", Some(90)), child, orphan];
        let ids: Vec<_> = grouped(&items)[0].1.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["p", "o"]);
    }

    #[test]
    fn byline_drops_what_repeats_the_source() {
        let mut i = item("a", "slack.dm", None);
        assert_eq!(byline(&i), "dana · acme/api");
        i.event.context = Some("Slack".into());
        assert_eq!(byline(&i), "dana");
        i.event.source = "tab".into();
        i.event.actor = Some("you".into());
        assert_eq!(byline(&i), "Slack");
    }

    #[test]
    fn unknown_kinds_get_a_readable_label() {
        let k = Kinds::default().get("github.review_requested");
        assert_eq!(k.label, "review requested");
        assert_eq!(k.icon, "github");
        assert_eq!(Kinds::default().get("weird").icon, "dot");
    }

    #[test]
    fn counts_by_app() {
        let items = vec![
            item("a", "github.ci", None),
            item("b", "github.pr", None),
            item("c", "slack.dm", None),
        ];
        let c = counts(&items);
        assert_eq!(c["github"], 2);
        assert_eq!(c["slack"], 1);
    }
}
