//! The connector contract.
//!
//! A connector is anything that can notice work for a human: a Slack
//! workspace, a GitHub account, a mailbox, a meeting recorder, a fleet of
//! agent sessions. Each one runs as its own task, pushes [`IncomingEvent`]s
//! through its [`ConnectorContext`], and may react when the person finishes
//! an item (for example, marking the GitHub notification read).

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use std::sync::Arc;
use std::time::Duration;

use crate::autofix::FixReport;
use crate::config::ConnectorConfig;
use crate::gateway::Gateway;
use crate::model::{
    Block, ConnectorDescriptor, ConnectorStatus, ContextStatus, DoneGuard, IncomingEvent,
    ItemContext, QueueItem,
};

/// Describes a connector type and builds instances of it from settings.
///
/// The registry of factories is what the Connections tab lists. Adding a
/// connector means adding a factory; nothing else in the app changes.
pub trait ConnectorFactory: Send + Sync {
    fn descriptor(&self) -> ConnectorDescriptor;
    /// `None` for connectors that are described but not built yet.
    fn build(
        &self,
        cfg: &ConnectorConfig,
        gateway: Arc<Gateway>,
    ) -> Option<anyhow::Result<Arc<dyn Connector>>>;
    /// Something changed the things behind these pages (an agent merged a
    /// pull request, a webhook said an issue closed): drop whatever this
    /// connector cached about them, so the app's next look is fresh. The
    /// engine then re-fetches their previews and tells the app.
    ///
    /// On the factory, not the connector, so it works while the
    /// connection isn't running.
    fn forget_pages(&self, _pages: &[String]) {}
}

#[async_trait]
pub trait Connector: Send + Sync + 'static {
    /// Stable id, also used as `IncomingEvent::source`.
    fn id(&self) -> &str;

    /// Runs until `ctx.cancelled()` fires. Returning an error marks the
    /// connector as failed in the UI, and the engine starts it again after
    /// a pause that grows with each failure in a row ([`restart_delay`]).
    /// Return a [`NeedsReconnect`] for an error only the person can fix
    /// (a rejected token): that one stays failed until they reconnect.
    /// Returning `Ok` means the connector is finished; it isn't restarted.
    async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()>;

    /// Called after the person marks one of this connector's items done.
    async fn on_done(&self, _item: &QueueItem) -> anyhow::Result<()> {
        Ok(())
    }

    /// Reasons the person shouldn't mark an item done yet. Asked of every
    /// connector, not just the item's own, so a connector can speak for its
    /// links in another item's Context (GitHub: "the PR for this task is
    /// still open"). `ctx` is the item with its Context.
    ///
    /// Keep it quick and cached; the engine gives up on slow answers.
    async fn done_guards(&self, _ctx: &ItemContext) -> anyhow::Result<Vec<DoneGuard>> {
        Ok(Vec::new())
    }

    /// Where the things this connector knows about in an item's Context
    /// stand (GitHub: "Changes requested" on the task's PR). Like
    /// [`Self::done_guards`], asked of every connector with the item's
    /// Context; connectors with nothing to say keep the default.
    ///
    /// Keep it quick and cached; the engine gives up on slow answers.
    async fn context_status(&self, _ctx: &ItemContext) -> anyhow::Result<Vec<ContextStatus>> {
        Ok(Vec::new())
    }

    /// What's wrong with one link this connector knows (failing checks,
    /// open review threads), for autofix to hand to the task's agent. `None`
    /// when the link isn't this connector's; set [`ContextStatus::autofix`]
    /// on the links that have one.
    ///
    /// Asked about once a minute while autofix is on, so it should read
    /// fresh state rather than a long cache.
    async fn fix_report(&self, _url: &str) -> anyhow::Result<Option<FixReport>> {
        Ok(None)
    }

    /// True when the thing behind an item is finished upstream, e.g. its pull
    /// request closed or merged. The engine asks once per start for every
    /// open item, and marks finished ones done.
    async fn is_resolved(&self, _item: &QueueItem) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// Richer content for the detail pane, fetched on demand. Implementations
    /// should go through the gateway with a cache TTL so previews are cheap.
    async fn preview(&self, _item: &QueueItem) -> anyhow::Result<Vec<Block>> {
        Ok(Vec::new())
    }

    /// Runs one of the actions in this connector's descriptor for a
    /// workflow. `input` holds the step's inputs, templates already filled
    /// in. Returns the step's output, which later steps can read.
    ///
    /// API calls go through the gateway like everything else; the workflow
    /// runtime runs actions at background priority, so a tight budget defers
    /// them instead of spending what interactive use needs.
    async fn act(
        &self,
        action: &str,
        _input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("{} has no action {action}", self.id())
    }

    /// How long a cached preview counts as fresh. Older previews still
    /// render instantly; the engine's refresher replaces them in the
    /// background when this connector's rate budget has room.
    fn preview_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(15 * 60)
    }
}

/// An error a connector can't recover from by trying again, such as a
/// rejected token: the person has to reconnect. A connector's `run` returns
/// it (`Err(NeedsReconnect(..).into())`) so the engine doesn't restart it.
#[derive(Debug)]
pub struct NeedsReconnect(pub String);

impl std::fmt::Display for NeedsReconnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NeedsReconnect {}

/// The pause before the first restart of a failed connector.
pub const RESTART_FIRST: Duration = Duration::from_secs(5);
/// The longest pause between restarts.
pub const RESTART_CAP: Duration = Duration::from_secs(5 * 60);
/// A connector that ran this long before failing was healthy: its next
/// failure counts as the first again.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long to wait before restarting a connector that has now failed
/// `failures` times in a row: [`RESTART_FIRST`], doubling with each
/// failure, up to [`RESTART_CAP`].
pub fn restart_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    RESTART_FIRST
        .saturating_mul(1 << doublings)
        .min(RESTART_CAP)
}

/// Failures in a row after one more, given the count so far and how long
/// this run lasted: a run of [`HEALTHY_AFTER`] or more starts the count over.
pub fn failures_after(so_far: u32, ran_for: Duration) -> u32 {
    if ran_for >= HEALTHY_AFTER {
        1
    } else {
        so_far.saturating_add(1)
    }
}

/// Runs a connector, starting it again after an error with
/// [`restart_delay`] between tries, until it finishes, the engine shuts
/// down, or it fails with [`NeedsReconnect`]. While it waits to restart,
/// its status is the error, with when it tries again.
pub(crate) async fn supervise(c: Arc<dyn Connector>, ctx: ConnectorContext) {
    let mut failures = 0;
    loop {
        let started = tokio::time::Instant::now();
        let e = match c.run(ctx.clone()).await {
            Ok(()) => {
                ctx.set_status(ConnectorStatus::Stopped).await;
                return;
            }
            Err(e) => e,
        };
        let message = e.to_string();
        if ctx.is_cancelled() || e.downcast_ref::<NeedsReconnect>().is_some() {
            tracing::error!(connector = c.id(), error = %e, "connector failed");
            ctx.set_status(ConnectorStatus::Error { message }).await;
            return;
        }
        failures = failures_after(failures, started.elapsed());
        let wait = restart_delay(failures);
        tracing::error!(connector = c.id(), error = %e, failures, retry_in = ?wait, "connector failed; restarting");
        ctx.set_status(ConnectorStatus::Error {
            message: format!("{message} (trying again in {}s)", wait.as_secs()),
        })
        .await;
        if !ctx.sleep(wait).await {
            return;
        }
    }
}

pub(crate) enum EngineMsg {
    Event(Box<IncomingEvent>),
    /// The item with this external id is finished upstream; mark it done.
    Resolved(String, String),
    Status(String, ConnectorStatus),
    /// These pages changed upstream; see [`ConnectorContext::changed`].
    Changed(String, Vec<String>),
}

#[derive(Clone)]
pub struct ConnectorContext {
    id: String,
    tx: mpsc::Sender<EngineMsg>,
    cancel: CancellationToken,
}

impl ConnectorContext {
    pub(crate) fn new(id: String, tx: mpsc::Sender<EngineMsg>, cancel: CancellationToken) -> Self {
        Self { id, tx, cancel }
    }

    /// Hands an event to the engine. Fails only when the engine is shutting down.
    pub async fn emit(&self, event: IncomingEvent) -> anyhow::Result<()> {
        self.tx
            .send(EngineMsg::Event(Box::new(event)))
            .await
            .map_err(|_| anyhow::anyhow!("engine stopped"))
    }

    /// Says the thing behind an item finished upstream (a PR closed or
    /// merged). If it's in the queue, it's marked done; if not, nothing happens.
    pub async fn resolve(&self, external_id: &str) {
        let _ = self
            .tx
            .send(EngineMsg::Resolved(
                self.id.clone(),
                external_id.to_string(),
            ))
            .await;
    }

    /// Says the things behind these pages changed upstream, so tab drops
    /// what it cached about them and the app refreshes what shows them.
    /// Cheaper than waiting for the next poll.
    pub async fn changed(&self, pages: Vec<String>) {
        let _ = self
            .tx
            .send(EngineMsg::Changed(self.id.clone(), pages))
            .await;
    }

    pub async fn synced(&self) {
        self.set_status(ConnectorStatus::Running {
            last_sync: Some(Utc::now()),
        })
        .await;
    }

    pub async fn set_status(&self, status: ConnectorStatus) {
        let _ = self
            .tx
            .send(EngineMsg::Status(self.id.clone(), status))
            .await;
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Sleeps for `d`, returning `false` early if the engine is shutting down.
    pub async fn sleep(&self, d: std::time::Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.cancel.cancelled() => false,
        }
    }
}

/// A context outside the engine, for examples and smoke tests: every event and
/// status change arrives on the receiver as one line of text.
pub fn test_context(id: &str) -> (ConnectorContext, mpsc::Receiver<String>, CancellationToken) {
    let (tx, mut rx) = mpsc::channel::<EngineMsg>(64);
    let (out_tx, out_rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let line = match m {
                EngineMsg::Event(e) => format!(
                    "event {} | {} | {} | from {} | ai={} | {}",
                    e.kind,
                    e.external_id,
                    e.title,
                    e.actor.unwrap_or_default(),
                    e.metadata["ai"],
                    e.body.unwrap_or_default().replace('\n', " ")
                ),
                EngineMsg::Status(id, s) => format!("status {id}: {s:?}"),
                EngineMsg::Resolved(_, ext) => format!("resolved {ext}"),
                EngineMsg::Changed(_, pages) => format!("changed {}", pages.join(" ")),
            };
            if out_tx.send(line).await.is_err() {
                break;
            }
        }
    });
    (
        ConnectorContext::new(id.into(), tx, cancel.clone()),
        out_rx,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn restart_delay_doubles_from_five_seconds_up_to_five_minutes() {
        let secs: Vec<u64> = (1..=9).map(|n| restart_delay(n).as_secs()).collect();
        assert_eq!(secs, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(restart_delay(0), RESTART_FIRST);
        assert_eq!(restart_delay(u32::MAX), RESTART_CAP);
    }

    #[test]
    fn a_healthy_run_starts_the_count_over() {
        assert_eq!(failures_after(0, Duration::ZERO), 1);
        assert_eq!(failures_after(4, Duration::from_secs(30)), 5);
        assert_eq!(failures_after(4, HEALTHY_AFTER - Duration::from_secs(1)), 5);
        assert_eq!(failures_after(4, HEALTHY_AFTER), 1);
        assert_eq!(failures_after(u32::MAX, Duration::ZERO), u32::MAX);
    }

    /// Fails `fail` times, then runs until cancelled.
    struct Flaky {
        runs: AtomicU32,
        fail: u32,
        reconnect: bool,
    }

    #[async_trait]
    impl Connector for Flaky {
        fn id(&self) -> &str {
            "flaky"
        }
        async fn run(&self, ctx: ConnectorContext) -> anyhow::Result<()> {
            let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            if self.reconnect {
                return Err(NeedsReconnect("token rejected".into()).into());
            }
            if n <= self.fail {
                anyhow::bail!("down {n}");
            }
            ctx.synced().await;
            ctx.cancelled().await;
            Ok(())
        }
    }

    fn flaky(fail: u32, reconnect: bool) -> Arc<Flaky> {
        Arc::new(Flaky {
            runs: AtomicU32::new(0),
            fail,
            reconnect,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_connector_is_restarted_with_backoff() {
        let c = flaky(2, false);
        let (ctx, mut rx, cancel) = test_context("flaky");
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(supervise(c.clone(), ctx));
        let mut lines = Vec::new();
        while lines.len() < 3 {
            lines.push(rx.recv().await.unwrap());
        }
        assert!(
            lines[0].contains("down 1 (trying again in 5s)"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("down 2 (trying again in 10s)"),
            "{lines:?}"
        );
        assert!(lines[2].contains("Running"), "{lines:?}");
        assert_eq!(c.runs.load(Ordering::SeqCst), 3);
        // Paused time: the waits are virtual, 5s then 10s.
        assert!(started.elapsed() >= Duration::from_secs(15));

        cancel.cancel();
        task.await.unwrap();
        assert!(rx.recv().await.unwrap().contains("Stopped"));
        assert_eq!(c.runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connector_that_needs_reconnecting_stays_failed() {
        let c = flaky(0, true);
        let (ctx, mut rx, _cancel) = test_context("flaky");
        supervise(c.clone(), ctx).await;
        let line = rx.recv().await.unwrap();
        assert!(line.contains("token rejected"), "{line}");
        assert!(!line.contains("trying again"), "{line}");
        assert_eq!(c.runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn shutting_down_during_the_wait_stops_restarts() {
        let c = flaky(u32::MAX, false);
        let (ctx, mut rx, cancel) = test_context("flaky");
        let task = tokio::spawn(supervise(c.clone(), ctx));
        assert!(rx.recv().await.unwrap().contains("trying again in 5s"));
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(c.runs.load(Ordering::SeqCst), 1);
    }
}
