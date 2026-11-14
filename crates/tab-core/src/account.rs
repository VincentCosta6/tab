//! The tab account: signing in to Supabase with Apple or Google.
//!
//! Desktop OAuth with PKCE over a loopback redirect. tab listens on a free
//! port on 127.0.0.1, opens the browser at Supabase's authorize page, and
//! Supabase sends the browser back to `http://127.0.0.1:<port>/auth/callback`
//! with a one-time code. tab trades the code and its verifier for a session.
//! The project's redirect allow list needs `http://127.0.0.1:*/auth/callback`.
//!
//! The session (with its tokens) is kept in `account.json` beside
//! `config.json`, never in the config the UI sees. The UI only ever gets an
//! [`AccountView`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
pub use tokio_util::sync::CancellationToken;

/// How long tab waits for the browser to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const CALLBACK_PATH: &str = "/auth/callback";

/// The Supabase project tab signs in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupabaseConfig {
    /// e.g. `https://abcd.supabase.co`, without a trailing slash.
    pub url: String,
    /// The project's public anon key.
    pub anon_key: String,
}

/// tab's own Supabase project (tab-prod). Neither value is secret: the
/// publishable key is meant to ship inside apps. Every build signs in here
/// unless `TAB_SUPABASE_URL` and `TAB_SUPABASE_ANON_KEY` point elsewhere.
pub const DEFAULT_SUPABASE_URL: &str = "https://keyimpyykjgropteihtg.supabase.co";
pub const DEFAULT_SUPABASE_ANON_KEY: &str = "sb_publishable_4clSvjbo6A0ox9AkMzAiZw_zkLlx2hk";

impl SupabaseConfig {
    /// From `TAB_SUPABASE_URL` and `TAB_SUPABASE_ANON_KEY`: at run time if
    /// set, else baked in at build time, else tab's own project. Setting
    /// either to `off` turns sign-in off.
    pub fn from_env() -> Option<Self> {
        Self::resolve(
            std::env::var("TAB_SUPABASE_URL").ok(),
            option_env!("TAB_SUPABASE_URL"),
            std::env::var("TAB_SUPABASE_ANON_KEY").ok(),
            option_env!("TAB_SUPABASE_ANON_KEY"),
        )
    }

    fn resolve(
        url_run: Option<String>,
        url_built: Option<&str>,
        key_run: Option<String>,
        key_built: Option<&str>,
    ) -> Option<Self> {
        let get = |run: Option<String>, built: Option<&str>, default: &str| {
            let set = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
            let v = run
                .as_deref()
                .and_then(set)
                .or_else(|| built.and_then(set))
                .unwrap_or_else(|| default.to_string());
            (v != "off").then_some(v)
        };
        let url = get(url_run, url_built, DEFAULT_SUPABASE_URL)?;
        let anon_key = get(key_run, key_built, DEFAULT_SUPABASE_ANON_KEY)?;
        Some(Self {
            url: url.trim_end_matches('/').to_string(),
            anon_key,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Apple,
    Google,
}

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Provider::Apple => "apple",
            Provider::Google => "google",
        }
    }
}

/// A signed-in session, as kept on disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub account: AccountView,
}

/// Who's signed in, without the tokens: what the UI sees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountView {
    pub user_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub provider: String,
}

/// For the UI: whether this build can sign in, and who is.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AccountStatus {
    pub configured: bool,
    pub account: Option<AccountView>,
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("sign-in isn't set up in this build of tab")]
    NotConfigured,
    #[error("sign-in was cancelled")]
    Cancelled,
    #[error("the browser didn't come back within 5 minutes, so sign-in stopped. Try again.")]
    TimedOut,
    #[error("{0}")]
    Provider(String),
    #[error("couldn't reach the sign-in server: {0}")]
    Network(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// A PKCE verifier and its S256 challenge.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        // 64 hex characters from two random v4 UUIDs: in the 43-128 range
        // RFC 7636 asks for, and only unreserved characters.
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self::from_verifier(verifier)
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// Supabase's authorize page for `provider`, coming back to `redirect`.
pub fn authorize_url(
    cfg: &SupabaseConfig,
    provider: Provider,
    redirect: &str,
    challenge: &str,
) -> String {
    let mut u = reqwest::Url::parse(&format!("{}/auth/v1/authorize", cfg.url))
        .expect("Supabase URL is a valid base");
    u.query_pairs_mut()
        .append_pair("provider", provider.id())
        .append_pair("redirect_to", redirect)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "s256");
    u.to_string()
}

/// What the browser brought back to the loopback listener.
#[derive(Debug, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    Error(String),
    /// Some other request (a favicon, say): answer it and keep waiting.
    Other,
}

/// Reads the callback out of an HTTP request's first line.
pub fn parse_callback(request_line: &str) -> Callback {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return Callback::Other;
    };
    let Ok(u) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Other;
    };
    if u.path() != CALLBACK_PATH {
        return Callback::Other;
    }
    let mut code = None;
    let mut error = None;
    let mut description = None;
    for (k, v) in u.query_pairs() {
        match &*k {
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => description = Some(v.into_owned()),
            _ => {}
        }
    }
    match (code, error, description) {
        (Some(c), None, _) if !c.is_empty() => Callback::Code(c),
        (_, _, Some(d)) => Callback::Error(d),
        (_, Some(e), None) => Callback::Error(e),
        _ => Callback::Error("the sign-in page came back without a code".into()),
    }
}

/// Signs in with `provider`: opens the browser with `open` and waits for it
/// to come back, until `cancel` fires or five minutes pass.
pub async fn sign_in(
    cfg: &SupabaseConfig,
    provider: Provider,
    open: impl FnOnce(&str),
    cancel: CancellationToken,
) -> Result<Session, SignInError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let redirect = format!(
        "http://127.0.0.1:{}{CALLBACK_PATH}",
        listener.local_addr()?.port()
    );
    let pkce = Pkce::new();
    open(&authorize_url(cfg, provider, &redirect, &pkce.challenge));

    let code = tokio::select! {
        r = wait_for_code(&listener) => r?,
        _ = cancel.cancelled() => return Err(SignInError::Cancelled),
        _ = tokio::time::sleep(SIGN_IN_TIMEOUT) => return Err(SignInError::TimedOut),
    };
    exchange(cfg, &code, &pkce.verifier).await
}

async fn wait_for_code(listener: &TcpListener) -> Result<String, SignInError> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = match tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        let line = text.lines().next().unwrap_or_default();
        let (status, page, result) = match parse_callback(line) {
            Callback::Other => ("404 Not Found", page("Not found", ""), None),
            Callback::Code(c) => (
                "200 OK",
                page(
                    "You're signed in to tab",
                    "You can close this tab and go back to the app.",
                ),
                Some(Ok(c)),
            ),
            Callback::Error(e) => (
                "400 Bad Request",
                page("Sign-in didn't finish", &html_escape(&e)),
                Some(Err(SignInError::Provider(e))),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
        if let Some(r) = result {
            return r;
        }
    }
}

pub(crate) fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>tab</title>\
         <body style=\"font:15px system-ui;display:grid;place-items:center;height:90vh;margin:0\">\
         <div style=\"text-align:center\"><h2>{title}</h2><p style=\"opacity:.7\">{body}</p></div>"
    )
}

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    expires_in: Option<i64>,
    user: User,
}

#[derive(Deserialize)]
struct User {
    id: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    user_metadata: serde_json::Value,
    #[serde(default)]
    app_metadata: serde_json::Value,
}

/// Turns Supabase's token response into a [`Session`].
fn session_from(body: &str, now: i64) -> Result<Session, SignInError> {
    let t: TokenResponse = serde_json::from_str(body).map_err(|e| {
        SignInError::Provider(format!("unexpected answer from the sign-in server: {e}"))
    })?;
    let meta = |k: &str| {
        t.user
            .user_metadata
            .get(k)
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    Ok(Session {
        expires_at: t.expires_at.unwrap_or(now + t.expires_in.unwrap_or(3600)),
        account: AccountView {
            name: meta("full_name").or_else(|| meta("name")),
            avatar_url: meta("avatar_url").or_else(|| meta("picture")),
            email: t
                .user
                .email
                .filter(|e| !e.is_empty())
                .or_else(|| meta("email")),
            provider: t
                .user
                .app_metadata
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            user_id: t.user.id,
        },
        access_token: t.access_token,
        refresh_token: t.refresh_token,
    })
}

async fn exchange(
    cfg: &SupabaseConfig,
    code: &str,
    verifier: &str,
) -> Result<Session, SignInError> {
    let res = reqwest::Client::new()
        .post(format!("{}/auth/v1/token?grant_type=pkce", cfg.url))
        .header("apikey", &cfg.anon_key)
        .json(&serde_json::json!({ "auth_code": code, "code_verifier": verifier }))
        .send()
        .await
        .map_err(|e| SignInError::Network(e.to_string()))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| SignInError::Network(e.to_string()))?;
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                ["error_description", "msg", "message", "error"]
                    .iter()
                    .find_map(|k| v.get(k).and_then(|m| m.as_str()).map(String::from))
            })
            .unwrap_or_else(|| format!("the sign-in server said {status}"));
        return Err(SignInError::Provider(msg));
    }
    session_from(&body, chrono::Utc::now().timestamp())
}

/// A token this close to expiring is refreshed before use, in seconds.
const REFRESH_AHEAD: i64 = 60;

/// One refresh at a time. Each refresh token works once (Supabase rotates
/// it, and can end the whole session when a spent one comes back), and at
/// launch the cloud socket, the cloud runner and the UI all want a token at
/// once, often an expired one after the app was closed for a while.
static REFRESHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn usable(s: &Session) -> bool {
    s.expires_at - chrono::Utc::now().timestamp() > REFRESH_AHEAD
}

/// Trades the refresh token for a new session. Supabase rotates the refresh
/// token, so the result must be saved.
pub async fn refresh(cfg: &SupabaseConfig, session: &Session) -> Result<Session, SignInError> {
    let res = reqwest::Client::new()
        .post(format!(
            "{}/auth/v1/token?grant_type=refresh_token",
            cfg.url
        ))
        .header("apikey", &cfg.anon_key)
        .json(&serde_json::json!({ "refresh_token": session.refresh_token }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| SignInError::Network(e.to_string()))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| SignInError::Network(e.to_string()))?;
    // Only a refused refresh token means signing in again. A busy or
    // failing server (429, 5xx) is tried again on the next call.
    if status.as_u16() == 429 || status.is_server_error() {
        return Err(SignInError::Network(format!(
            "the sign-in server said {status}"
        )));
    }
    if !status.is_success() {
        return Err(SignInError::Provider(
            "your sign-in has expired. Sign in again.".into(),
        ));
    }
    session_from(&body, chrono::Utc::now().timestamp())
}

/// The saved session's access token, refreshed (and saved) first if it's
/// about to expire. `None` when nobody's signed in.
pub async fn access_token(
    cfg: &SupabaseConfig,
    path: &Path,
) -> Result<Option<String>, SignInError> {
    match load(path) {
        None => return Ok(None),
        Some(s) if usable(&s) => return Ok(Some(s.access_token)),
        Some(_) => {}
    }
    let _turn = REFRESHING.lock().await;
    // Whoever refreshed while this waited saved the new session.
    let Some(session) = load(path) else {
        return Ok(None);
    };
    if usable(&session) {
        return Ok(Some(session.access_token));
    }
    let fresh = match refresh(cfg, &session).await {
        Ok(fresh) => fresh,
        Err(e) => match load(path) {
            // Another tab (the one an update just restarted) spent the
            // token first and saved what it got: carry on from that.
            Some(now) if now.refresh_token != session.refresh_token => {
                if usable(&now) {
                    return Ok(Some(now.access_token));
                }
                refresh(cfg, &now).await?
            }
            _ => return Err(e),
        },
    };
    save(path, &fresh)?;
    Ok(Some(fresh.access_token))
}

/// Ends the session on Supabase too. Best effort: the local session goes
/// either way.
pub async fn revoke(cfg: &SupabaseConfig, session: &Session) {
    let _ = reqwest::Client::new()
        .post(format!("{}/auth/v1/logout", cfg.url))
        .header("apikey", &cfg.anon_key)
        .bearer_auth(&session.access_token)
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

/// Where the session is kept: `account.json` beside `config.json`.
pub fn session_path(config_dir: &Path) -> PathBuf {
    config_dir.join("account.json")
}

pub fn load(path: &Path) -> Option<Session> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes the session readable by this user only.
pub fn save(path: &Path, session: &Session) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A temporary file of its own, so two saves at once can't write into
    // each other's and leave a broken session behind.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}-{n}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(session).map_err(std::io::Error::other)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        std::io::Write::write_all(&mut f, &bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)
}

pub fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_tab_prod() {
        let c = SupabaseConfig::resolve(None, None, None, None).unwrap();
        assert_eq!(c.url, DEFAULT_SUPABASE_URL);
        assert_eq!(c.anon_key, DEFAULT_SUPABASE_ANON_KEY);
        let c = SupabaseConfig::resolve(
            Some("https://x.supabase.co/".into()),
            Some("https://built.supabase.co"),
            Some(" ".into()),
            Some("built-key"),
        )
        .unwrap();
        assert_eq!(c.url, "https://x.supabase.co");
        assert_eq!(c.anon_key, "built-key");
        assert!(SupabaseConfig::resolve(Some("off".into()), None, None, None).is_none());
    }

    fn cfg() -> SupabaseConfig {
        SupabaseConfig {
            url: "https://proj.supabase.co".into(),
            anon_key: "anon".into(),
        }
    }

    #[test]
    fn pkce_matches_rfc_7636_example() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let fresh = Pkce::new();
        assert_eq!(fresh.verifier.len(), 64);
        assert_ne!(fresh.verifier, Pkce::new().verifier);
    }

    #[test]
    fn authorize_url_carries_provider_redirect_and_challenge() {
        let u = authorize_url(
            &cfg(),
            Provider::Apple,
            "http://127.0.0.1:5000/auth/callback",
            "abc",
        );
        let u = reqwest::Url::parse(&u).unwrap();
        assert_eq!(u.path(), "/auth/v1/authorize");
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["provider"], "apple");
        assert_eq!(q["redirect_to"], "http://127.0.0.1:5000/auth/callback");
        assert_eq!(q["code_challenge"], "abc");
        assert_eq!(q["code_challenge_method"], "s256");
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(
            parse_callback("GET /auth/callback?code=xyz HTTP/1.1"),
            Callback::Code("xyz".into())
        );
        assert_eq!(
            parse_callback(
                "GET /auth/callback?error=access_denied&error_description=User+cancelled HTTP/1.1"
            ),
            Callback::Error("User cancelled".into())
        );
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1"), Callback::Other);
        assert_eq!(
            parse_callback("POST /auth/callback?code=x HTTP/1.1"),
            Callback::Other
        );
        assert!(matches!(
            parse_callback("GET /auth/callback HTTP/1.1"),
            Callback::Error(_)
        ));
    }

    #[test]
    fn session_from_supabase_answer() {
        let body = r#"{"access_token":"a","refresh_token":"r","expires_in":3600,"token_type":"bearer",
            "user":{"id":"u1","email":"me@example.com","app_metadata":{"provider":"google"},
            "user_metadata":{"full_name":"Me","avatar_url":"https://x/p.png"}}}"#;
        let s = session_from(body, 1000).unwrap();
        assert_eq!(s.expires_at, 4600);
        assert_eq!(s.account.provider, "google");
        assert_eq!(s.account.email.as_deref(), Some("me@example.com"));
        assert_eq!(s.account.name.as_deref(), Some("Me"));
        // Apple can hide the email and send no name.
        let apple = r#"{"access_token":"a","refresh_token":"r","expires_at":99,
            "user":{"id":"u2","email":"","app_metadata":{"provider":"apple"},"user_metadata":{}}}"#;
        let s = session_from(apple, 0).unwrap();
        assert_eq!(
            (s.expires_at, s.account.email, s.account.name),
            (99, None, None)
        );
    }

    #[test]
    fn session_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("tab-account-{}", uuid::Uuid::new_v4()));
        let path = session_path(&dir);
        assert!(load(&path).is_none());
        let s = session_from(
            r#"{"access_token":"a","refresh_token":"r","user":{"id":"u"}}"#,
            0,
        )
        .unwrap();
        save(&path, &s).unwrap();
        assert_eq!(load(&path), Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(load(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn session(refresh: &str, expires_at: i64) -> Session {
        Session {
            access_token: format!("access-{refresh}"),
            refresh_token: refresh.into(),
            expires_at,
            account: session_from(
                r#"{"access_token":"a","refresh_token":"r","user":{"id":"u"}}"#,
                0,
            )
            .unwrap()
            .account,
        }
    }

    /// A stand-in for Supabase's token endpoint: each refresh token works
    /// once, and `on_spent` decides what a spent one gets.
    async fn token_server(
        on_spent: impl Fn(&str) -> u16 + Clone + Send + Sync + 'static,
    ) -> (
        SupabaseConfig,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::{Json, Router, http::StatusCode, routing::post};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let app = Router::new().route(
            "/auth/v1/token",
            post({
                let seen = seen.clone();
                move |Json(body): Json<serde_json::Value>| async move {
                    let token = body["refresh_token"].as_str().unwrap().to_string();
                    let mut seen = seen.lock().unwrap();
                    seen.push(format!("asked {token}"));
                    if seen.contains(&token) {
                        let status = StatusCode::from_u16(on_spent(&token)).unwrap();
                        return (status, r#"{"error":"invalid_grant"}"#.to_string());
                    }
                    seen.push(token.clone());
                    let next = format!("{token}+");
                    (
                        StatusCode::OK,
                        format!(
                            r#"{{"access_token":"access-{next}","refresh_token":"{next}","expires_in":3600,"user":{{"id":"u"}}}}"#
                        ),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let cfg = SupabaseConfig {
            url: format!("http://{addr}"),
            anon_key: "anon".into(),
        };
        (cfg, seen)
    }

    fn temp_session(s: &Session) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tab-account-{}", uuid::Uuid::new_v4()));
        let path = session_path(&dir);
        save(&path, s).unwrap();
        path
    }

    /// At launch everything wants a token at once; only one refresh goes
    /// out, so the session survives.
    #[tokio::test]
    async fn callers_at_once_share_one_refresh() {
        let (cfg, seen) = token_server(|_| 400).await;
        let path = temp_session(&session("r1", 0));
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let (cfg, path) = (cfg.clone(), path.clone());
                tokio::spawn(async move { access_token(&cfg, &path).await })
            })
            .collect();
        for c in calls {
            assert_eq!(c.await.unwrap().unwrap().as_deref(), Some("access-r1+"));
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["asked r1".to_string(), "r1".to_string()]
        );
        assert_eq!(load(&path).unwrap().refresh_token, "r1+");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The tab an update replaced refreshed and saved just before this one
    /// tried the same token: this one carries on with what it saved.
    #[tokio::test]
    async fn a_token_another_tab_spent_is_picked_up() {
        let path = temp_session(&session("r1", 0));
        let (cfg, seen) = token_server({
            let path = path.clone();
            move |_| {
                save(
                    &path,
                    &session("r1+", chrono::Utc::now().timestamp() + 3600),
                )
                .unwrap();
                400
            }
        })
        .await;
        seen.lock().unwrap().push("r1".into());
        let got = access_token(&cfg, &path).await.unwrap();
        assert_eq!(got.as_deref(), Some("access-r1+"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A busy sign-in server isn't an expired sign-in.
    #[tokio::test]
    async fn a_failing_server_is_not_an_expired_sign_in() {
        let (cfg, seen) = token_server(|_| 503).await;
        seen.lock().unwrap().push("r1".into());
        let path = temp_session(&session("r1", 0));
        let e = access_token(&cfg, &path).await.unwrap_err();
        assert!(matches!(e, SignInError::Network(_)), "{e:?}");
        // Refused outright, it is.
        let (cfg, seen) = token_server(|_| 400).await;
        seen.lock().unwrap().push("r1".into());
        let e = access_token(&cfg, &path).await.unwrap_err();
        assert!(matches!(e, SignInError::Provider(_)), "{e:?}");
        assert_eq!(load(&path).unwrap().refresh_token, "r1");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn loopback_hands_back_the_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = tokio::spawn(async move {
            let get = |path: &'static str| async move {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).await.unwrap();
                out
            };
            assert!(get("/favicon.ico").await.starts_with("HTTP/1.1 404"));
            get("/auth/callback?code=c0de").await
        });
        assert_eq!(wait_for_code(&listener).await.unwrap(), "c0de");
        assert!(browser.await.unwrap().contains("signed in"));
    }
}
