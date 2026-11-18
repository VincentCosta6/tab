//! What the gate remembers per chat: its approved plan, what it read on
//! write sites (taint), links it saw, lent logins, whether the person has
//! taken over, and how many actions it took. In memory only; it goes when
//! tab quits or the person presses Stop.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::plan::Plan;
use super::taint::Taint;

/// Actions (clicks, typing, navigations) per chat, at most.
pub const MAX_ACTIONS: usize = 300;

#[derive(Debug, Default, Clone)]
pub struct Chat {
    pub plan: Option<Plan>,
    /// Plan steps done, by index.
    pub done: HashSet<usize>,
    pub taint: Taint,
    pub seen_links: HashSet<String>,
    /// Tabs carrying a login the person lent, by tab id, with the site.
    pub lent: HashMap<String, String>,
    /// Tabs the person took over (by touching them, or a handover), with why.
    pub paused: HashMap<String, String>,
    pub actions: usize,
    /// When the agent last acted in each tab (ms since the epoch), so a
    /// later trusted click or key in the page means the person took over.
    pub last_agent_ms: HashMap<String, u64>,
    /// Write sites the person agreed to lend their login for.
    pub lend_ok: HashSet<String>,
}

static CHATS: LazyLock<Mutex<HashMap<String, Chat>>> = LazyLock::new(Default::default);

/// Runs `f` on a chat's state.
pub fn with<R>(session_id: &str, f: impl FnOnce(&mut Chat) -> R) -> R {
    let mut chats = CHATS.lock().unwrap();
    f(chats.entry(session_id.to_string()).or_default())
}

pub fn get(session_id: &str) -> Chat {
    with(session_id, |c| c.clone())
}

/// Forgets everything about a chat: plan, taint, lent logins (Stop).
pub fn forget(session_id: &str) {
    CHATS.lock().unwrap().remove(session_id);
}

impl Chat {
    /// Counts one action, or fails at [`MAX_ACTIONS`].
    pub fn take_action(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.actions < MAX_ACTIONS,
            "this chat used its {MAX_ACTIONS} browser actions; ask the person whether to go on"
        );
        self.actions += 1;
        Ok(())
    }

    /// Approves `plan`: the values in it are the person's to send.
    pub fn approve(&mut self, plan: Plan) {
        self.taint.allow(plan.values());
        self.done.clear();
        self.plan = Some(plan);
    }
}

/// Which agent tab the person is looking at, as the dock last reported:
/// the active tab while the dock is open, tab is the front window, and the
/// page is visible.
static WATCHED: LazyLock<Mutex<Option<(String, Instant)>>> = LazyLock::new(Default::default);

pub fn set_watched(tab_id: Option<String>) {
    *WATCHED.lock().unwrap() = tab_id.map(|t| (t, Instant::now()));
}

pub fn watching(tab_id: &str) -> bool {
    WATCHED
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(t, _)| t == tab_id)
}

/// Waits up to `limit` for the person to watch `tab_id`.
pub async fn wait_watching(tab_id: &str, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if watching(tab_id) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    watching(tab_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_capped_and_stop_forgets() {
        let id = format!("s-{}", uuid::Uuid::new_v4());
        with(&id, |c| {
            for _ in 0..MAX_ACTIONS {
                c.take_action().unwrap();
            }
            assert!(c.take_action().is_err());
            c.taint.add("secret-value-123");
        });
        forget(&id);
        assert_eq!(get(&id).actions, 0);
        assert!(get(&id).taint.is_empty());
    }

    #[test]
    fn watching_follows_the_dock() {
        set_watched(Some("agent-a".into()));
        assert!(watching("agent-a"));
        assert!(!watching("agent-b"));
        set_watched(None);
        assert!(!watching("agent-a"));
    }
}
