//! Grafana alerts → queue items, marked done when they stop firing.
//!
//! Polls the Alertmanager API of a Grafana instance (Grafana Cloud or
//! self-hosted) with a service account token. Every firing alert, such as a
//! failing synthetic monitoring check, becomes one item keyed by its
//! fingerprint. When an alert drops out of the firing list, its item is
//! marked done; if it fires again later it comes back with its new start time.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::ConnectorConfig;
use crate::connector::{Connector, ConnectorContext, ConnectorFactory};
use crate::gateway::{Call, Gateway, GatewayError};
use crate::model::{
    Block, ConnectorDescriptor, ConnectorStatus, EntityRef, Field, IncomingEvent, KindInfo, Link,
    QueueItem, SettingField, SettingKind, Tone,
};
use crate::ratelimit::RateLimitSpec;

const ID: &str = "grafana";
const MAX_BODY: usize = 4000;
/// How long the firing list counts as fresh for `is_resolved`, which the
/// engine asks once per open item on start.
const LIST_TTL: Duration = Duration::from_secs(30);

pub struct GrafanaFactory;

impl ConnectorFactory for GrafanaFactory {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: ID.into(),
            name: "Grafana".into(),
            icon: "grafana".into(),
            description: "Firing alerts, like failing synthetic checks, marked done when they recover.".into(),
            kinds: vec![KindInfo::new("grafana.alert", "Alert firing", "alert", Tone::Danger)],
            settings: vec![
                SettingField {
                    key: "url".into(),
                    label: "Grafana URL".into(),
                    kind: SettingKind::Text,
                    help: Some("For example https://yourstack.grafana.net".into()),
                    required: true,
                },
                SettingField {
                    key: "token".into(),
                    label: "Service account token".into(),
                    kind: SettingKind::Secret,
                    help: Some("A service account with the Viewer role (Administration → Service accounts).".into()),
                    required: true,
                },
                SettingField {
                    key: "filter".into(),
                    label: "Only alerts matching".into(),
                    kind: SettingKind::Text,
                    help: Some("Optional label matchers, separated by semicolons, e.g. grafana_folder=\"Synthetic Monitoring\"; severity=~\"critical|high\"".into()),
                    required: false,
                },
                SettingField {
                    key: "alertmanagers".into(),
                    label: "Alertmanagers".into(),
                    kind: SettingKind::Text,
                    help: Some("Optional. Comma-separated Alertmanager data source uids; default grafana (Grafana-managed alerts). Add your Mimir Alertmanager's uid for data-source-managed rules.".into()),
                    required: false,
                },
                SettingField {
                    key: "poll_seconds".into(),
                    label: "Poll interval (seconds)".into(),
                    kind: SettingKind::Number,
                    help: Some("Default 60. Minimum 15.".into()),
                    required: false,
                },
            ],
            available: true,
            rate_limits: vec![
                RateLimitSpec::new(ID, "default", 300, 60, 30)
                    .note("tab's own budget; Grafana publishes no API limit"),
            ],
            category: "code".into(),
            features: vec![
                "Each firing alert is one item, with its labels, summary and runbook".into(),
                "Items are marked done when the alert resolves".into(),
            ],
            builtin: false,
            actions: Vec::new(),
            clis: Vec::new(),
        }
    }

    fn build(
        &self,
        cfg: &ConnectorConfig,
        gateway: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>> {
        let (Some(url), Some(token)) = (cfg.str("url"), cfg.str("token")) else {
            return Some(Err(anyhow::anyhow!(
                "Add your Grafana URL and a service account token."
            )));
        };
        let url = url.trim().trim_end_matches('/');
        if !url.starts_with("https://") && !url.starts_with("http://") {
            return Some(Err(anyhow::anyhow!(
                "The Grafana URL should start with https://"
            )));
        }
        let alertmanagers = cfg
            .str("alertmanagers")
            .map(|s| split(s, ','))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["grafana".into()]);
        Some(Ok(Arc::new(GrafanaConnector::new(
            gateway,
            url,
            token,
            cfg.str("filter").map(|s| split(s, ';')).unwrap_or_default(),
            alertmanagers,
            cfg.u64("poll_seconds"),
        ))))
    }
}

fn split(s: &str, sep: char) -> Vec<String> {
    s.split(sep)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// One alert as Alertmanager's v2 API lists it.
#[derive(Debug, Clone, Deserialize)]
struct Alert {
    fingerprint: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(rename = "startsAt")]
    starts_at: DateTime<Utc>,
    #[serde(rename = "generatorURL", default)]
    generator_url: Option<String>,
}

impl Alert {
    fn label(&self, k: &str) -> Option<&str> {
        self.labels
            .get(k)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
    fn annotation(&self, k: &str) -> Option<&str> {
        self.annotations
            .get(k)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

/// Labels Grafana adds for its own bookkeeping; not worth a badge.
fn is_internal(label: &str) -> bool {
    label.starts_with("__") || matches!(label, "alertname" | "grafana_folder")
}

pub struct GrafanaConnector {
    gateway: Arc<Gateway>,
    base: String,
    filter: Vec<String>,
    alertmanagers: Vec<String>,
    poll: Duration,
    /// External ids firing at the last poll.
    firing: Mutex<HashSet<String>>,
}

impl GrafanaConnector {
    pub fn new(
        gateway: Arc<Gateway>,
        base: &str,
        token: &str,
        filter: Vec<String>,
        alertmanagers: Vec<String>,
        poll_seconds: Option<u64>,
    ) -> Self {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {}", token.trim())) {
            v.set_sensitive(true);
            h.insert(AUTHORIZATION, v);
        }
        gateway.set_provider_headers(ID, h);
        Self {
            gateway,
            base: base.to_string(),
            filter,
            alertmanagers,
            poll: Duration::from_secs(poll_seconds.unwrap_or(60).max(15)),
            firing: Mutex::new(HashSet::new()),
        }
    }

    /// Every alert firing now (not silenced or inhibited), across the
    /// configured Alertmanagers, keyed by external id.
    async fn firing_now(&self, ttl: Duration) -> Result<Vec<(String, Alert)>, GatewayError> {
        let mut out = Vec::new();
        for am in &self.alertmanagers {
            let mut call = Call::get(
                ID,
                ID,
                "list firing alerts",
                format!("{}/api/alertmanager/{am}/api/v2/alerts", self.base),
            )
            .query("active", "true")
            .query("silenced", "false")
            .query("inhibited", "false")
            .cache(ttl);
            for f in &self.filter {
                call = call.query("filter", f.clone());
            }
            let alerts: Vec<Alert> = self.gateway.send(call).await?.json()?;
            out.extend(
                alerts
                    .into_iter()
                    .map(|a| (external_id(am, &a.fingerprint), a)),
            );
        }
        Ok(out)
    }

    async fn poll(&self, ctx: &ConnectorContext) -> Result<(), GatewayError> {
        let now = self.firing_now(Duration::ZERO).await?;
        let ids: HashSet<String> = now.iter().map(|(id, _)| id.clone()).collect();
        let (new, gone) = {
            let mut firing = self.firing.lock().await;
            let gone: Vec<String> = firing.difference(&ids).cloned().collect();
            let new: Vec<_> = now
                .into_iter()
                .filter(|(id, _)| !firing.contains(id))
                .collect();
            *firing = ids;
            (new, gone)
        };
        for id in gone {
            ctx.resolve(&id).await;
        }
        for (id, a) in new {
            ctx.emit(to_event(&self.base, id, &a)).await?;
        }
        Ok(())
    }
}

fn external_id(alertmanager: &str, fingerprint: &str) -> String {
    format!("{alertmanager}:{fingerprint}")
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn to_event(base: &str, external_id: String, a: &Alert) -> IncomingEvent {
    let name = a.label("alertname").unwrap_or("Alert");
    // Synthetic checks name the probed target in `instance` and the check in `job`.
    let target = a.label("instance").or(a.label("job"));
    let title = match (a.annotation("summary"), target) {
        (Some(s), _) => s.to_string(),
        (None, Some(t)) => format!("{name}: {t}"),
        (None, None) => name.to_string(),
    };
    let body = a
        .annotation("description")
        .map(|d| truncate(d.to_string(), MAX_BODY));
    let folder = a.label("grafana_folder");

    let mut badges: Vec<Field> = a
        .labels
        .iter()
        .filter(|(k, v)| !is_internal(k) && !v.is_empty())
        .map(|(k, v)| {
            let f = Field::new(k.clone(), v.clone());
            if k == "severity" {
                f.tone(Tone::Danger)
            } else {
                f
            }
        })
        .collect();
    badges.truncate(12);
    let mut blocks = Vec::new();
    if !badges.is_empty() {
        blocks.push(Block::Badges { items: badges });
    }
    if let Some(d) = &body {
        blocks.push(Block::Text {
            text: d.clone(),
            markdown: false,
        });
    }
    let mut links = Vec::new();
    if let Some(u) = &a.generator_url {
        links.push(Link {
            label: "Alert rule".into(),
            url: u.clone(),
        });
    }
    if let Some(dash) = a.annotation("__dashboardUid__") {
        let url = match a.annotation("__panelId__") {
            Some(p) => format!("{base}/d/{dash}?viewPanel={p}"),
            None => format!("{base}/d/{dash}"),
        };
        links.push(Link {
            label: "Dashboard".into(),
            url,
        });
    }
    if let Some(r) = a.annotation("runbook_url") {
        links.push(Link {
            label: "Runbook".into(),
            url: r.to_string(),
        });
    }
    if !links.is_empty() {
        blocks.push(Block::Links { items: links });
    }

    let mut entities = Vec::new();
    if let Some(uid) = a.label("__alert_rule_uid__") {
        entities.push(EntityRef::new(
            "alert_rule",
            format!("grafana:rule:{uid}"),
            name,
            "about",
        ));
    }
    if let Some(t) = target {
        entities.push(EntityRef::new(
            "service",
            format!("grafana:target:{t}"),
            t,
            "about",
        ));
    }

    IncomingEvent {
        source: ID.into(),
        external_id,
        kind: "grafana.alert".into(),
        title,
        body,
        url: a.generator_url.clone(),
        actor: None,
        context: folder.map(String::from),
        occurred_at: a.starts_at,
        blocks,
        entities,
        metadata: json!({
            "fingerprint": a.fingerprint,
            "alertname": name,
            "labels": a.labels,
            "severity": a.label("severity"),
            "folder": folder,
        }),
    }
}

#[async_trait]
impl Connector for GrafanaConnector {
    fn id(&self) -> &str {
        ID
    }

    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
        let mut backoff = Duration::from_secs(15);
        while !ctx.is_cancelled() {
            let wait = match self.poll(&ctx).await {
                Ok(()) => {
                    ctx.synced().await;
                    backoff = Duration::from_secs(15);
                    self.poll
                }
                Err(GatewayError::Http {
                    status: 401 | 403, ..
                }) => {
                    return Err(crate::connector::NeedsReconnect(
                        "Grafana rejected the token. Check it can read alerts, then reconnect."
                            .into(),
                    )
                    .into());
                }
                Err(GatewayError::RateLimited { until, .. }) => {
                    ctx.set_status(ConnectorStatus::RateLimited { until }).await;
                    (until - Utc::now())
                        .to_std()
                        .unwrap_or(self.poll)
                        .max(self.poll)
                }
                Err(e) => {
                    ctx.set_status(ConnectorStatus::Error {
                        message: e.to_string(),
                    })
                    .await;
                    backoff = (backoff * 2).min(Duration::from_secs(600));
                    backoff
                }
            };
            if !ctx.sleep(wait).await {
                break;
            }
        }
        Ok(())
    }

    /// Asked on start for items left open from before: done if the alert
    /// isn't firing any more.
    async fn is_resolved(&self, item: &QueueItem) -> anyhow::Result<bool> {
        let now = self.firing_now(LIST_TTL).await?;
        Ok(!now.iter().any(|(id, _)| *id == item.event.external_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(v: serde_json::Value) -> Alert {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_synthetic_check_alert_becomes_an_item() {
        let a = alert(json!({
            "fingerprint": "abc123",
            "labels": {
                "alertname": "ProbeFailedExecutionsTooHigh",
                "instance": "https://shop.example.com",
                "job": "shop homepage",
                "severity": "critical",
                "grafana_folder": "Synthetic Monitoring",
                "__alert_rule_uid__": "r1"
            },
            "annotations": {
                "description": "3 of 5 probes failed",
                "__dashboardUid__": "sm",
                "__panelId__": "4",
                "runbook_url": "https://wiki/runbook"
            },
            "startsAt": "2026-11-15T10:00:00Z",
            "generatorURL": "https://g.example/alerting/grafana/r1/view",
            "status": { "state": "active" }
        }));
        let e = to_event(
            "https://g.example",
            external_id("grafana", &a.fingerprint),
            &a,
        );
        assert_eq!(e.external_id, "grafana:abc123");
        assert_eq!(
            e.title,
            "ProbeFailedExecutionsTooHigh: https://shop.example.com"
        );
        assert_eq!(e.body.as_deref(), Some("3 of 5 probes failed"));
        assert_eq!(e.context.as_deref(), Some("Synthetic Monitoring"));
        assert_eq!(
            e.url.as_deref(),
            Some("https://g.example/alerting/grafana/r1/view")
        );
        let Block::Badges { items } = &e.blocks[0] else {
            panic!("badges first")
        };
        let labels: Vec<_> = items.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["instance", "job", "severity"]);
        let Some(Block::Links { items }) = e.blocks.last() else {
            panic!("links last")
        };
        assert_eq!(items[1].url, "https://g.example/d/sm?viewPanel=4");
        assert_eq!(items[2].label, "Runbook");
        assert_eq!(e.entities[0].key, "grafana:rule:r1");
    }

    #[test]
    fn a_summary_wins_the_title() {
        let a = alert(json!({
            "fingerprint": "f",
            "labels": { "alertname": "X" },
            "annotations": { "summary": "Checkout is down" },
            "startsAt": "2026-11-15T10:00:00Z"
        }));
        assert_eq!(
            to_event("b", "grafana:f".into(), &a).title,
            "Checkout is down"
        );
    }

    #[test]
    fn settings_need_url_and_token() {
        let gw = Arc::new(Gateway::new(Arc::new(
            crate::store::Store::in_memory().unwrap(),
        )));
        let mut cfg = ConnectorConfig::on();
        assert!(matches!(
            GrafanaFactory.build(&cfg, gw.clone()),
            Some(Err(_))
        ));
        cfg.settings.insert("url".into(), json!("g.example"));
        cfg.settings.insert("token".into(), json!("t"));
        assert!(matches!(
            GrafanaFactory.build(&cfg, gw.clone()),
            Some(Err(_))
        ));
        cfg.settings
            .insert("url".into(), json!("https://g.example/"));
        assert!(matches!(GrafanaFactory.build(&cfg, gw), Some(Ok(_))));
    }
}
