//! tab cloud: setting up and listing a person's cloud hubs.
//!
//! The cloud side lives in the private `tab-infra` repo. Its edge Worker
//! (`api.tabqueue.com`) is the only way to reach a cloud hub; hubs have no
//! public address. Every call carries the signed-in person's Supabase access
//! token, and every body is binary Protobuf from [`tab_protocol::cloud`]
//! (`account.proto`), the same types the Worker is built from:
//!
//! - `POST /v1/hub` sets up their personal hub (a Durable Object of their
//!   own) and creates the account and trial. Idempotent: calling again
//!   retries a failed setup and leaves a ready hub alone.
//! - `GET /v1/hub` says where that is.
//! - `GET /v1/hubs` lists every hub they belong to (personal, and teams').

use std::time::Duration;

use prost::Message;
pub use tab_protocol::cloud as pb;

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

/// A failed call: the cloud's [`pb::Error`], or one the app made up for a
/// failure that never reached the cloud (`Unreachable`, `Unauthenticated`).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{message}")]
pub struct CloudError {
    pub code: pb::ErrorCode,
    pub message: String,
}

impl CloudError {
    pub fn new(code: pb::ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn not_signed_in() -> Self {
        Self::new(pb::ErrorCode::Unauthenticated, "Sign in to use tab cloud.")
    }

    pub fn unreachable(detail: impl std::fmt::Display) -> Self {
        Self::new(
            pb::ErrorCode::Unreachable,
            format!("Couldn't reach tab cloud: {detail}"),
        )
    }

    pub fn to_pb(&self) -> pb::Error {
        pb::Error {
            code: self.code as i32,
            message: self.message.clone(),
        }
    }
}

/// Reads one answer from the edge Worker: `M` on a 2xx, else its
/// [`pb::Error`]. A personal hub whose setup failed comes back as a hub in
/// `Failed` (with a 502), not as an error, so the app can offer a retry.
pub fn interpret<M: Message + Default>(status: u16, body: &[u8]) -> Result<M, CloudError> {
    if ((200..300).contains(&status) || status == 502)
        && let Ok(m) = M::decode(body)
    {
        return Ok(m);
    }
    match pb::Error::decode(body) {
        Ok(e) if e.code != 0 => Err(CloudError {
            code: pb::ErrorCode::try_from(e.code).unwrap_or(pb::ErrorCode::Internal),
            message: e.message,
        }),
        _ => Err(CloudError::new(
            pb::ErrorCode::Internal,
            format!("tab cloud answered {status}. Try again in a moment."),
        )),
    }
}

async fn call<M: Message + Default>(
    cfg: &CloudConfig,
    token: &str,
    post: bool,
    path: &str,
) -> Result<M, CloudError> {
    let method = if post {
        reqwest::Method::POST
    } else {
        reqwest::Method::GET
    };
    send(cfg, &format!("Bearer {token}"), method, path, None).await
}

async fn send<M: Message + Default>(
    cfg: &CloudConfig,
    authorization: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<M, CloudError> {
    let client = reqwest::Client::new();
    let url = format!("{}{path}", cfg.api);
    let mut req = client.request(method, url);
    if let Some(b) = body {
        req = req.header("content-type", pb::CONTENT_TYPE).body(b);
    }
    let res = req
        .header("authorization", authorization)
        .header("accept", pb::CONTENT_TYPE)
        .header("user-agent", concat!("tab/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| CloudError::unreachable(e.without_url()))?;
    let status = res.status().as_u16();
    let body = res.bytes().await.map_err(CloudError::unreachable)?;
    interpret(status, &body)
}

fn state(s: &pb::HubStatus) -> pb::HubState {
    s.hub
        .as_ref()
        .and_then(|h| pb::HubState::try_from(h.state).ok())
        .unwrap_or(pb::HubState::None)
}

/// The person's personal hub, and their plan.
pub async fn hub_status(cfg: &CloudConfig, token: &str) -> Result<pb::HubStatus, CloudError> {
    call(cfg, token, false, "/v1/hub").await
}

/// Every hub the person belongs to, personal first.
pub async fn list_hubs(cfg: &CloudConfig, token: &str) -> Result<pb::HubList, CloudError> {
    call(cfg, token, false, "/v1/hubs").await
}

/// Sets up the person's hub, or retries a failed setup, and waits until it's
/// ready or failed. A hub already ready comes straight back.
pub async fn set_up_hub(cfg: &CloudConfig, token: &str) -> Result<pb::HubStatus, CloudError> {
    let mut hub: pb::HubStatus = call(cfg, token, true, "/v1/hub").await?;
    let deadline = tokio::time::Instant::now() + WAIT_FOR;
    // Another request (another device, an earlier click) is setting it up.
    while state(&hub) == pb::HubState::Provisioning && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        hub = hub_status(cfg, token).await?;
    }
    Ok(hub)
}

/// Stores (or replaces) secrets in a hub for its cloud runner, and removes
/// the ones named in `remove`. Returns the names it now keeps.
pub async fn put_secrets(
    cfg: &CloudConfig,
    token: &str,
    hub_id: &str,
    secrets: Vec<(String, String)>,
    remove: Vec<String>,
) -> Result<Vec<String>, CloudError> {
    let body = pb::PutSecrets {
        secrets: secrets
            .into_iter()
            .map(|(name, value)| pb::Secret { name, value })
            .collect(),
        remove,
    };
    let names: pb::SecretNames = send(
        cfg,
        &format!("Bearer {token}"),
        reqwest::Method::PUT,
        &format!("/v1/hubs/{hub_id}/secrets"),
        Some(body.encode_to_vec()),
    )
    .await?;
    Ok(names.names)
}

/// The names of the secrets a hub keeps. Never their values.
pub async fn secret_names(
    cfg: &CloudConfig,
    token: &str,
    hub_id: &str,
) -> Result<Vec<String>, CloudError> {
    let n: pb::SecretNames = call(cfg, token, false, &format!("/v1/hubs/{hub_id}/secrets")).await?;
    Ok(n.names)
}

/// The hub's cloud runner: asleep, starting, running, or not on the plan.
pub async fn runner_status(
    cfg: &CloudConfig,
    token: &str,
    hub_id: &str,
) -> Result<pb::RunnerStatus, CloudError> {
    call(cfg, token, false, &format!("/v1/hubs/{hub_id}/runner")).await
}

/// Wakes the hub's cloud runner (paid plans).
pub async fn wake_runner(
    cfg: &CloudConfig,
    token: &str,
    hub_id: &str,
) -> Result<pb::RunnerStatus, CloudError> {
    call(cfg, token, true, &format!("/v1/hubs/{hub_id}/runner")).await
}

/// For the cloud runner itself: its hub's secrets, with values.
pub async fn runner_secrets(
    cfg: &CloudConfig,
    runner_token: &str,
) -> Result<Vec<(String, String)>, CloudError> {
    let s: pb::Secrets = send(
        cfg,
        &format!("Runner {runner_token}"),
        reqwest::Method::GET,
        "/v1/runner/secrets",
        None,
    )
    .await?;
    Ok(s.secrets.into_iter().map(|s| (s.name, s.value)).collect())
}

/// For the cloud runner itself: replaces secrets it keeps current, which
/// for now is only an MCP connection's cloud sign-in after a refresh
/// (servers rotate refresh tokens). The hub refuses any other name.
pub async fn runner_put_secrets(
    cfg: &CloudConfig,
    runner_token: &str,
    secrets: Vec<(String, String)>,
) -> Result<Vec<String>, CloudError> {
    let body = pb::PutSecrets {
        secrets: secrets
            .into_iter()
            .map(|(name, value)| pb::Secret { name, value })
            .collect(),
        remove: Vec::new(),
    };
    let names: pb::SecretNames = send(
        cfg,
        &format!("Runner {runner_token}"),
        reqwest::Method::PUT,
        "/v1/runner/secrets",
        Some(body.encode_to_vec()),
    )
    .await?;
    Ok(names.names)
}

/// The secret name a connection's field is kept under.
pub fn connection_secret(connection: &str, field: &str) -> String {
    format!("connection/{connection}/{field}")
}

/// What the hub keeps for an MCP connection allowed in tab cloud jobs,
/// under `connection/mcp.<id>/…` (ids can't hold a dot, so these never
/// meet a built-in connection's):
pub mod mcp_secret {
    /// The server, as [`crate::remote_mcp::RemoteMcpConfig::for_cloud`].
    pub const SERVER: &str = "server";
    /// Its sign-in for tab cloud (a [`crate::remote_mcp::SignIn`]), a grant
    /// of its own that the runner refreshes and writes back.
    pub const OAUTH: &str = "oauth";
    /// Its header and environment values ([`crate::remote_mcp::Secrets`]).
    pub const KEYS: &str = "keys";
    /// The connections the person has on a computer but not in tab cloud
    /// ([`crate::remote_mcp::Elsewhere`]), so a job can say it needed one.
    pub const ELSEWHERE: &str = "connection/mcp/elsewhere";

    pub fn name(id: &str, field: &str) -> String {
        format!("connection/mcp.{id}/{field}")
    }

    /// `(id, field)` from a name made by [`name`].
    pub fn parse(name: &str) -> Option<(&str, &str)> {
        let rest = name.strip_prefix("connection/mcp.")?;
        let (id, field) = rest.split_once('/')?;
        (!id.is_empty() && !field.is_empty() && !field.contains('/')).then_some((id, field))
    }
}

/// The secret holding the person's agent setup (JSON `AgentConfig`, with
/// this computer's paths left out) for the cloud runner's harnesses.
pub const AGENT_SETUP_SECRET: &str = "setup/agent";

/// The secret name a harness's environment variable is kept under.
pub fn harness_secret(harness: &str, var: &str) -> String {
    format!("harness/{harness}/{var}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn status(state: pb::HubState) -> pb::HubStatus {
        pb::HubStatus {
            hub: Some(pb::Hub {
                hub_id: "5e7e".into(),
                kind: pb::HubKind::Personal as i32,
                role: pb::Role::Owner as i32,
                state: state as i32,
                region: "wnam".into(),
                ..Default::default()
            }),
            plan: Some(pb::Plan {
                name: "trial".into(),
                ends_at: Some(prost_types::Timestamp {
                    seconds: 1_795_294_325,
                    nanos: 0,
                }),
                active: true,
            }),
        }
    }

    fn err(code: pb::ErrorCode, message: &str) -> Vec<u8> {
        pb::Error {
            code: code as i32,
            message: message.into(),
        }
        .encode_to_vec()
    }

    #[test]
    fn reads_a_hub() {
        let body = status(pb::HubState::Ready).encode_to_vec();
        let hub: pb::HubStatus = interpret(200, &body).unwrap();
        assert_eq!(state(&hub), pb::HubState::Ready);
        assert_eq!(hub.hub.unwrap().region, "wnam");
        assert_eq!(hub.plan.unwrap().name, "trial");
    }

    #[test]
    fn a_failed_setup_is_a_hub_not_an_error() {
        let mut s = status(pb::HubState::Failed);
        s.hub.as_mut().unwrap().error = "tab couldn't finish setting up your hub.".into();
        let hub: pb::HubStatus = interpret(502, &s.encode_to_vec()).unwrap();
        assert_eq!(state(&hub), pb::HubState::Failed);
    }

    #[test]
    fn errors_carry_the_clouds_code_and_message() {
        let e = interpret::<pb::HubStatus>(
            402,
            &err(
                pb::ErrorCode::PlanRequired,
                "Your tab cloud trial has ended.",
            ),
        )
        .unwrap_err();
        assert_eq!(e.code, pb::ErrorCode::PlanRequired);
        assert_eq!(e.to_string(), "Your tab cloud trial has ended.");
        let e = interpret::<pb::HubList>(404, &err(pb::ErrorCode::NotFound, "no")).unwrap_err();
        assert_eq!(e.code, pb::ErrorCode::NotFound);
        // A gateway's HTML page, or an error code from a newer cloud.
        let e = interpret::<pb::HubStatus>(503, b"<html>bad gateway</html>").unwrap_err();
        assert_eq!(e.code, pb::ErrorCode::Internal);
        assert!(e.message.contains("503"));
        let e = interpret::<pb::HubStatus>(
            400,
            &pb::Error {
                code: 999,
                message: "new".into(),
            }
            .encode_to_vec(),
        )
        .unwrap_err();
        assert_eq!(e.code, pb::ErrorCode::Internal);
    }

    #[test]
    fn mcp_secrets_have_names_of_their_own() {
        let n = mcp_secret::name("linear", mcp_secret::OAUTH);
        assert_eq!(n, "connection/mcp.linear/oauth");
        assert_eq!(mcp_secret::parse(&n), Some(("linear", "oauth")));
        // The list of the rest, and built-in connections, aren't one.
        assert_eq!(mcp_secret::parse(mcp_secret::ELSEWHERE), None);
        assert_eq!(mcp_secret::parse("connection/github/token"), None);
        assert_eq!(mcp_secret::parse("connection/mcp./oauth"), None);
        assert_eq!(mcp_secret::parse("connection/mcp.x/a/b"), None);
    }

    #[test]
    fn app_made_errors() {
        assert_eq!(
            CloudError::not_signed_in().code,
            pb::ErrorCode::Unauthenticated
        );
        let e = CloudError::unreachable("dns error");
        assert_eq!(e.code, pb::ErrorCode::Unreachable);
        assert_eq!(e.to_pb().code, pb::ErrorCode::Unreachable as i32);
    }

    /// A stand-in for the edge Worker: the first POST finds another request
    /// mid-setup, and the hub turns ready on the second GET.
    #[tokio::test]
    async fn set_up_waits_for_a_hub_being_set_up() {
        use axum::{Router, extract::State, http::HeaderMap, routing::get};
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let provisioning = status(pb::HubState::Provisioning).encode_to_vec();
        let ready = status(pb::HubState::Ready).encode_to_vec();
        let app = Router::new()
            .route(
                "/v1/hub",
                get({
                    let (p, r) = (provisioning.clone(), ready.clone());
                    move |State(c): State<Arc<Mutex<Vec<String>>>>, h: HeaderMap| async move {
                        let mut c = c.lock().unwrap();
                        c.push(format!(
                            "GET {} {}",
                            h["authorization"].to_str().unwrap(),
                            h["accept"].to_str().unwrap()
                        ));
                        if c.len() < 3 { p } else { r }
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
        assert_eq!(state(&hub), pb::HubState::Ready);
        let accept = "Bearer tok application/x-protobuf";
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "POST".to_string(),
                format!("GET {accept}"),
                format!("GET {accept}")
            ]
        );
    }
}
