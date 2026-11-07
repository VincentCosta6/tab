//! tab cloud: setting up a person's private hub.
//!
//! The cloud side lives in the private `tab-infra` repo. Its edge Worker
//! (`api.tabqueue.com`) answers two calls this module makes, both with the
//! signed-in person's Supabase access token:
//!
//! - `POST /v1/hub` creates their account (and trial) and sets up their hub,
//!   a Durable Object of their own. Idempotent: calling again retries a
//!   failed setup and leaves a ready hub alone.
//! - `GET /v1/hub` says where that is.
//!
//! The JSON mirrors `tab-infra`'s `crates/accounts/src/api.rs`. Fields are
//! only ever added there, and unknown ones are ignored here.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long [`set_up_hub`] waits for a hub another request is setting up.
const WAIT_FOR: Duration = Duration::from_secs(30);
const POLL_EVERY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudConfig {
    /// e.g. `https://api.tabqueue.com`, without a trailing slash.
    pub api: String,
}

impl CloudConfig {
    /// `TAB_CLOUD_URL` at run time, else at build time, else tab cloud.
    pub fn from_env() -> Self {
        let url = std::env::var("TAB_CLOUD_URL")
            .ok()
            .or(option_env!("TAB_CLOUD_URL").map(String::from))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://api.tabqueue.com".into());
        Self {
            api: url.trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudHubState {
    None,
    Provisioning,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudPlan {
    /// `trial`, `pro` or `none`.
    pub plan: String,
    /// Unix seconds.
    pub ends_at: Option<i64>,
    pub active: bool,
}

/// The person's cloud hub, as the edge Worker reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudHub {
    pub state: CloudHubState,
    pub hub_id: Option<String>,
    /// Cloudflare's region hint, e.g. `wnam`.
    pub region: Option<String>,
    pub ready_at: Option<i64>,
    pub error: Option<String>,
    pub plan: CloudPlan,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CloudError {
    #[error("sign in to use tab cloud")]
    NotSignedIn,
    /// The trial or plan has ended. Carries the cloud's message.
    #[error("{0}")]
    PlanRequired(String),
    #[error("{0}")]
    Locked(String),
    #[error("tab cloud is busy. Try again in a minute.")]
    RateLimited,
    #[error("couldn't reach tab cloud: {0}")]
    Network(String),
    #[error("{0}")]
    Server(String),
}

impl CloudError {
    /// A stable name for the UI to branch on.
    pub fn code(&self) -> &'static str {
        match self {
            CloudError::NotSignedIn => "not_signed_in",
            CloudError::PlanRequired(_) => "plan_required",
            CloudError::Locked(_) => "locked",
            CloudError::RateLimited => "rate_limited",
            CloudError::Network(_) => "network",
            CloudError::Server(_) => "server",
        }
    }
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

/// Reads one answer from the edge Worker. A failed setup comes back as a
/// hub in `failed` (with a 502), not as an error, so the UI can offer a retry.
pub fn interpret(status: u16, body: &str) -> Result<CloudHub, CloudError> {
    if let Ok(hub) = serde_json::from_str::<CloudHub>(body) {
        return Ok(hub);
    }
    let err = serde_json::from_str::<ApiError>(body).ok();
    let message = err.as_ref().map(|e| e.message.clone());
    Err(match (status, err.as_ref().map(|e| e.code.as_str())) {
        (401, _) => CloudError::NotSignedIn,
        (402, _) | (_, Some("plan_required")) => {
            CloudError::PlanRequired(message.unwrap_or_else(|| "tab cloud needs a plan.".into()))
        }
        (403, _) => CloudError::Locked(message.unwrap_or_else(|| "This account is locked.".into())),
        (429, _) => CloudError::RateLimited,
        _ => CloudError::Server(
            message
                .unwrap_or_else(|| format!("tab cloud answered {status}. Try again in a moment.")),
        ),
    })
}

async fn call(cfg: &CloudConfig, token: &str, post: bool) -> Result<CloudHub, CloudError> {
    let client = reqwest::Client::new();
    let url = format!("{}/v1/hub", cfg.api);
    let req = if post {
        client.post(url)
    } else {
        client.get(url)
    };
    let res = req
        .bearer_auth(token)
        .header("user-agent", concat!("tab/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| CloudError::Network(e.without_url().to_string()))?;
    let status = res.status().as_u16();
    let body = res
        .text()
        .await
        .map_err(|e| CloudError::Network(e.to_string()))?;
    interpret(status, &body)
}

/// Where the person's cloud hub is.
pub async fn hub_status(cfg: &CloudConfig, token: &str) -> Result<CloudHub, CloudError> {
    call(cfg, token, false).await
}

/// Sets up the person's hub, or retries a failed setup, and waits until it's
/// ready or failed. A hub already ready comes straight back.
pub async fn set_up_hub(cfg: &CloudConfig, token: &str) -> Result<CloudHub, CloudError> {
    let mut hub = call(cfg, token, true).await?;
    let deadline = tokio::time::Instant::now() + WAIT_FOR;
    // Another request (another device, an earlier click) is setting it up.
    while hub.state == CloudHubState::Provisioning && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        hub = call(cfg, token, false).await?;
    }
    Ok(hub)
}

/// How the app names a region hint.
pub fn region_name(hint: &str) -> &'static str {
    match hint {
        "wnam" => "Western North America",
        "enam" => "Eastern North America",
        "sam" => "South America",
        "weur" => "Western Europe",
        "eeur" => "Eastern Europe",
        "afr" => "Africa",
        "me" => "Middle East",
        "apac" => "Asia-Pacific",
        "oc" => "Oceania",
        _ => "the nearest region",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const READY: &str = r#"{"state":"ready","hub_id":"5e7e","region":"wnam","ready_at":1794084725,"error":null,"plan":{"plan":"trial","ends_at":1795294325,"active":true},"later_field":1}"#;

    #[test]
    fn reads_a_hub_and_ignores_new_fields() {
        let hub = interpret(200, READY).unwrap();
        assert_eq!(hub.state, CloudHubState::Ready);
        assert_eq!(hub.region.as_deref(), Some("wnam"));
        assert_eq!(hub.plan.plan, "trial");
        assert!(hub.plan.active);
    }

    #[test]
    fn a_failed_setup_is_a_hub_not_an_error() {
        let body = r#"{"state":"failed","hub_id":"h","region":null,"ready_at":null,"error":"tab couldn't finish setting up your hub. Try again in a moment.","plan":{"plan":"trial","ends_at":1,"active":true}}"#;
        let hub = interpret(502, body).unwrap();
        assert_eq!(hub.state, CloudHubState::Failed);
        assert!(hub.error.unwrap().contains("Try again"));
    }

    #[test]
    fn errors_map_to_what_the_ui_does() {
        let e = |s, b: &str| interpret(s, b).unwrap_err();
        assert_eq!(
            e(
                401,
                r#"{"code":"unauthenticated","message":"the token has expired"}"#
            ),
            CloudError::NotSignedIn
        );
        let plan = e(
            402,
            r#"{"code":"plan_required","message":"Your tab cloud trial has ended."}"#,
        );
        assert_eq!(plan.code(), "plan_required");
        assert_eq!(plan.to_string(), "Your tab cloud trial has ended.");
        assert_eq!(
            e(
                403,
                r#"{"code":"locked","message":"This account is locked. Contact support."}"#
            )
            .code(),
            "locked"
        );
        assert_eq!(
            e(429, r#"{"code":"rate_limited","message":"x"}"#),
            CloudError::RateLimited
        );
        assert_eq!(
            e(
                500,
                r#"{"code":"internal","message":"Something went wrong on our side."}"#
            ),
            CloudError::Server("Something went wrong on our side.".into())
        );
        assert!(
            matches!(e(503, "<html>bad gateway</html>"), CloudError::Server(m) if m.contains("503"))
        );
    }

    #[test]
    fn config_default_and_trailing_slash() {
        // Not touching the process env (tests run in parallel): just the rules.
        let c = CloudConfig {
            api: "https://api.tabqueue.com".into(),
        };
        assert_eq!(c.api, "https://api.tabqueue.com");
        assert_eq!(region_name("weur"), "Western Europe");
        assert_eq!(region_name("??"), "the nearest region");
    }

    /// A stand-in for the edge Worker: the first POST finds another request
    /// mid-setup, and the hub turns ready on the second GET.
    #[tokio::test]
    async fn set_up_waits_for_a_hub_being_set_up() {
        use axum::{Router, extract::State, http::HeaderMap, routing::get};
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let provisioning = READY.replace(r#""state":"ready""#, r#""state":"provisioning""#);
        let app = Router::new()
            .route(
                "/v1/hub",
                get({
                    let p = provisioning.clone();
                    move |State(c): State<Arc<Mutex<Vec<String>>>>, h: HeaderMap| async move {
                        let mut c = c.lock().unwrap();
                        c.push(format!("GET {}", h["authorization"].to_str().unwrap()));
                        if c.len() < 3 { p } else { READY.to_string() }
                    }
                })
                .post({
                    let p = provisioning.clone();
                    move |State(c): State<Arc<Mutex<Vec<String>>>>| async move {
                        c.lock().unwrap().push("POST".into());
                        p
                    }
                }),
            )
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let cfg = CloudConfig {
            api: format!("http://{addr}"),
        };
        let hub = set_up_hub(&cfg, "tok").await.unwrap();
        assert_eq!(hub.state, CloudHubState::Ready);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["POST", "GET Bearer tok", "GET Bearer tok"]
        );
    }
}
