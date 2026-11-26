//! Signing in, and the phone's connection to the person's hub in tab cloud.
//!
//! The phone syncs like any other device: a `tab.db` of its own (in a
//! folder per hub), kept in step with the cloud hub through
//! [`LocalHub::use_cloud`], the same way the cloud runner does. So chats,
//! jobs and runners arrive as rows it can read, a job it writes goes out
//! through the outbox, and [`LocalHub`] watches chats other devices run
//! mid-turn and turns their live stream into [`EngineEvent`]s, which go to
//! the webview as `tab://event`.
//!
//! Sign-in is Supabase with PKCE, like the desktop's, except the page comes
//! back to the app's own link (`tabqueue://auth/callback`) instead of a
//! loopback listener, which iOS would suspend while the browser is up.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tab_core::account::{self, Callback, Pkce, Provider, SupabaseConfig};
use tab_core::cloud::{self, CloudConfig, pb};
use tab_core::cloud_link::{self, CloudLink, TokenSource};
use tab_core::delegate::Cloud;
use tab_core::engine::EngineEvent;
use tab_core::local_hub::LocalHub;
use tab_core::store::Store;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;

/// The link scheme sign-in comes back on. Supabase must list
/// `tabqueue://auth/callback` among its redirect URLs.
pub const SCHEME: &str = "tabqueue";

/// The app's state: who's signed in, and the hub connection once there is
/// one.
pub struct Phone {
    app: AppHandle,
    dir: PathBuf,
    supabase: Option<SupabaseConfig>,
    pub cloud: CloudConfig,
    /// The verifier of the sign-in that's waiting for its link.
    pkce: Mutex<Option<Pkce>>,
    conn: Mutex<Option<Conn>>,
    error: Mutex<Option<String>>,
}

/// A connection to one cloud hub.
#[derive(Clone)]
pub struct Conn {
    pub hub_id: String,
    pub store: Arc<Store>,
    pub hub: Arc<LocalHub>,
}

/// What the UI shows before anything else.
#[derive(serde::Serialize)]
pub struct AccountState {
    pub configured: bool,
    pub account: Option<account::AccountView>,
    /// Linked to the hub (it may still be catching up).
    pub connected: bool,
    pub error: Option<String>,
}

impl Phone {
    pub fn new(app: &AppHandle) -> anyhow::Result<Arc<Self>> {
        let dir = app.path().app_data_dir()?;
        std::fs::create_dir_all(&dir)?;
        Ok(Arc::new(Self {
            app: app.clone(),
            dir,
            supabase: SupabaseConfig::from_env(),
            cloud: CloudConfig::from_env(),
            pkce: Mutex::new(None),
            conn: Mutex::new(None),
            error: Mutex::new(None),
        }))
    }

    fn session_path(&self) -> PathBuf {
        account::session_path(&self.dir)
    }

    pub fn conn(&self) -> Option<Conn> {
        self.conn.lock().unwrap().clone()
    }

    pub fn state(&self) -> AccountState {
        AccountState {
            configured: self.supabase.is_some(),
            account: account::load(&self.session_path()).map(|s| s.account),
            connected: self.conn().is_some(),
            error: self.error.lock().unwrap().clone(),
        }
    }

    fn set_error(&self, e: Option<String>) {
        *self.error.lock().unwrap() = e;
        let _ = self.app.emit("tab://account", ());
    }

    /// A current access token, refreshed first if it's about to expire.
    pub async fn token(&self) -> anyhow::Result<String> {
        let cfg = self
            .supabase
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("sign-in isn't set up in this build"))?;
        account::access_token(cfg, &self.session_path())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("not signed in"))
    }

    fn token_source(self: &Arc<Self>) -> TokenSource {
        let me = self.clone();
        Arc::new(move || {
            let me = me.clone();
            Box::pin(async move { me.token().await })
        })
    }

    /// Opens the sign-in page; [`Phone::callback`] finishes when it comes
    /// back.
    pub fn sign_in(&self, provider: Provider) -> anyhow::Result<String> {
        let cfg = self
            .supabase
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("sign-in isn't set up in this build"))?;
        let pkce = Pkce::new();
        let url = account::authorize_url(
            cfg,
            provider,
            &account::mobile_redirect(SCHEME),
            &pkce.challenge,
        );
        *self.pkce.lock().unwrap() = Some(pkce);
        Ok(url)
    }

    /// The app was opened with `url`: a sign-in coming back, or not ours.
    pub fn callback(self: &Arc<Self>, url: &str) {
        let code = match account::parse_mobile_callback(url) {
            Callback::Code(c) => c,
            Callback::Error(e) => return self.set_error(Some(e)),
            Callback::Other => return,
        };
        let Some(pkce) = self.pkce.lock().unwrap().take() else {
            return self.set_error(Some("That sign-in link is stale. Sign in again.".into()));
        };
        let Some(cfg) = self.supabase.clone() else {
            return;
        };
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            match account::exchange(&cfg, &code, &pkce.verifier).await {
                Ok(session) => {
                    if let Err(e) = account::save(&me.session_path(), &session) {
                        return me.set_error(Some(format!("couldn't keep the sign-in: {e}")));
                    }
                    me.set_error(None);
                    me.connect().await;
                }
                Err(e) => me.set_error(Some(e.to_string())),
            }
        });
    }

    pub async fn sign_out(&self) {
        if let (Some(cfg), Some(s)) = (&self.supabase, account::load(&self.session_path())) {
            account::revoke(cfg, &s).await;
        }
        let _ = std::fs::remove_file(self.session_path());
        // The synced copy stays on disk for the next sign-in; the link
        // can't get a token any more, so it stops.
        *self.conn.lock().unwrap() = None;
        self.set_error(None);
    }

    /// Links to the person's own hub and starts syncing with it. Does
    /// nothing if already linked or not signed in.
    pub async fn connect(self: &Arc<Self>) {
        if self.conn().is_some() || account::load(&self.session_path()).is_none() {
            return;
        }
        match self.link().await {
            Ok(()) => self.set_error(None),
            Err(e) => self.set_error(Some(format!("{e:#}"))),
        }
    }

    async fn link(self: &Arc<Self>) -> anyhow::Result<()> {
        let token = self.token().await?;
        let hubs = cloud::list_hubs(&self.cloud, &token).await?;
        let hub_id = hubs
            .hubs
            .iter()
            .find(|h| {
                h.kind == pb::HubKind::Personal as i32 && h.state == pb::HubState::Ready as i32
            })
            .map(|h| h.hub_id.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Your hub isn't in tab cloud yet. Move it there from tab on your computer (Settings → Hub)."
                )
            })?;
        let dir = self.dir.join("hubs").join(&hub_id);
        std::fs::create_dir_all(&dir)?;
        let store = Arc::new(Store::open(&dir.join("tab.db"))?);
        let hub = LocalHub::open(&dir.join("hub.db"))?;
        // Everything goes to the cloud hub, never this phone's hub.db.
        hub.expect_cloud(&hub_id);
        let link = CloudLink::start(
            cloud_link::sync_url(&self.cloud.api, &hub_id),
            hub_id.clone(),
            self.token_source(),
        );
        let (events, mut rx) = broadcast::channel::<EngineEvent>(1024);
        hub.follow(store.clone(), events);
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let _ = app.emit("tab://event", ev);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Too far behind to replay: have the UI read again.
                        let _ = app.emit("tab://changed", ());
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let (h, s, me) = (hub.clone(), store.clone(), self.clone());
        tauri::async_runtime::spawn(async move {
            // Until it connects (offline, a token to refresh), writes wait.
            let mut wait = 2;
            loop {
                let (h, s, l) = (h.clone(), s.clone(), link.clone());
                match tauri::async_runtime::spawn_blocking(move || h.use_cloud(l, &s)).await {
                    Ok(Ok(())) => {
                        tracing::info!("syncing with tab cloud");
                        let _ = me.app.emit("tab://changed", ());
                        break;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, retry_in = wait, "tab cloud isn't reachable yet");
                    }
                    Err(e) => tracing::error!(error = %e, "connecting to tab cloud panicked"),
                }
                tokio::time::sleep(Duration::from_secs(wait)).await;
                wait = (wait * 2).min(60);
            }
        });
        *self.conn.lock().unwrap() = Some(Conn { hub_id, store, hub });
        Ok(())
    }

    /// The cloud runner's state, for deciding where a chat runs. Offline,
    /// it counts as not there, so the chat goes to a computer.
    pub async fn cloud_runner(&self, hub_id: &str) -> Cloud {
        let Ok(token) = self.token().await else {
            return Cloud::NotOnPlan;
        };
        match cloud::runner_status(&self.cloud, &token, hub_id).await {
            Ok(s) => cloud_of(s.state),
            Err(e) => {
                tracing::debug!(error = %e, "couldn't read the cloud runner's state");
                Cloud::NotOnPlan
            }
        }
    }

    /// Wakes the cloud runner, so a chat that's waiting for it starts.
    pub async fn wake_cloud(&self, hub_id: &str) -> anyhow::Result<()> {
        let token = self.token().await?;
        cloud::wake_runner(&self.cloud, &token, hub_id).await?;
        Ok(())
    }
}

/// tab cloud's runner states, as [`tab_core::delegate`] reads them.
pub fn cloud_of(state: i32) -> Cloud {
    match pb::RunnerState::try_from(state) {
        Ok(pb::RunnerState::Running | pb::RunnerState::Starting) => Cloud::Awake,
        Ok(pb::RunnerState::Asleep) => Cloud::Asleep,
        _ => Cloud::NotOnPlan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_runner_states_that_can_take_work_count() {
        assert_eq!(cloud_of(pb::RunnerState::Running as i32), Cloud::Awake);
        assert_eq!(cloud_of(pb::RunnerState::Starting as i32), Cloud::Awake);
        assert_eq!(cloud_of(pb::RunnerState::Asleep as i32), Cloud::Asleep);
        assert_eq!(
            cloud_of(pb::RunnerState::NotOnPlan as i32),
            Cloud::NotOnPlan
        );
        assert_eq!(cloud_of(0), Cloud::NotOnPlan);
        assert_eq!(cloud_of(99), Cloud::NotOnPlan);
    }
}
