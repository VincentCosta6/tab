//! "Sign in with Google / Microsoft" for the email connection: OAuth 2.0
//! with PKCE in the person's browser, then IMAP with XOAUTH2, so Gmail needs
//! no app password and Outlook works at all (Microsoft turned off password
//! sign-in for IMAP).
//!
//! Neither provider lets apps register themselves, so each needs a client
//! registered once for tab: a Google "Desktop app" OAuth client and a
//! Microsoft Entra public client with the redirect
//! `http://127.0.0.1/oauth/callback`. Their ids come from the build
//! (`TAB_GOOGLE_CLIENT_ID`, `TAB_GOOGLE_CLIENT_SECRET`,
//! `TAB_MICROSOFT_CLIENT_ID`) or, for development, the same variables at
//! run time. A provider without one isn't offered.
//!
//! The browser comes back to `/oauth/callback` on tab's MCP server, like an
//! MCP sign-in ([`crate::remote_mcp`]). The sign-in (refresh token included)
//! is kept in the vault ([`crate::secrets::vault`]), never in `config.json`;
//! the email connection's settings only say which provider it uses
//! (`auth`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::remote_mcp::oauth::{self, Client, Discovered, Tokens};

/// The vault entry for the email connection's sign-in.
const ENTRY: &str = "email-oauth";

/// Sign-ins started this long ago are dropped.
const PENDING_FOR: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Google, Provider::Microsoft];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == s.trim())
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }

    /// Where its mail is read.
    pub fn imap_host(self) -> &'static str {
        match self {
            Provider::Google => "imap.gmail.com",
            Provider::Microsoft => "outlook.office365.com",
        }
    }

    fn endpoints(self) -> Discovered {
        let (issuer, auth, token, scope) = match self {
            Provider::Google => (
                "https://accounts.google.com",
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                "https://mail.google.com/ openid email",
            ),
            Provider::Microsoft => (
                "https://login.microsoftonline.com/common/v2.0",
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                "https://outlook.office.com/IMAP.AccessAsUser.All offline_access openid email",
            ),
        };
        Discovered {
            // Neither takes RFC 8707's `resource`; Microsoft refuses it.
            resource: String::new(),
            issuer: issuer.into(),
            authorization_endpoint: auth.into(),
            token_endpoint: token.into(),
            registration_endpoint: None,
            scope: Some(scope.into()),
        }
    }

    /// Asks for a refresh token (Google only sends one with consent).
    fn extra_params(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Provider::Google => &[("access_type", "offline"), ("prompt", "consent")],
            Provider::Microsoft => &[("prompt", "select_account")],
        }
    }

    /// tab's client with the provider, if this build has one.
    fn client(self, redirect_uri: &str) -> Option<Client> {
        let var = |name: &str, built: Option<&'static str>| {
            std::env::var(name)
                .ok()
                .or(built.map(Into::into))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let (id, secret) = match self {
            // Google's desktop clients have a secret that isn't one (it
            // ships in every copy of the app), and its token endpoint wants it.
            Provider::Google => (
                var("TAB_GOOGLE_CLIENT_ID", option_env!("TAB_GOOGLE_CLIENT_ID")),
                var(
                    "TAB_GOOGLE_CLIENT_SECRET",
                    option_env!("TAB_GOOGLE_CLIENT_SECRET"),
                ),
            ),
            Provider::Microsoft => (
                var(
                    "TAB_MICROSOFT_CLIENT_ID",
                    option_env!("TAB_MICROSOFT_CLIENT_ID"),
                ),
                None,
            ),
        };
        Some(Client {
            client_id: id?,
            client_secret: secret,
            redirect_uri: redirect_uri.into(),
        })
    }

    pub fn available(self) -> bool {
        self.client("").is_some()
    }
}

/// One provider as the Connections tab offers it.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// This build has a client for it.
    pub available: bool,
}

pub fn providers() -> Vec<ProviderInfo> {
    Provider::ALL
        .into_iter()
        .map(|p| ProviderInfo {
            id: p.id(),
            name: p.name(),
            available: p.available(),
        })
        .collect()
}

/// What's kept in the vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailSignIn {
    pub provider: Provider,
    /// The address signed in, from the ID token: the IMAP user name.
    pub address: String,
    pub client: Client,
    pub tokens: Tokens,
    pub signed_in_at: DateTime<Utc>,
}

/// Who's signed in, for the Connections tab. No tokens.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SignedIn {
    pub provider: Provider,
    pub address: String,
    pub signed_in_at: DateTime<Utc>,
}

struct Pending {
    provider: Provider,
    verifier: String,
    client: Client,
    started: Instant,
}

static PENDING: Mutex<Option<HashMap<String, Pending>>> = Mutex::new(None);
static REFRESHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn vault() -> anyhow::Result<crate::vault::Vault> {
    crate::secrets::vault().ok_or_else(|| anyhow::anyhow!("tab's vault isn't set up yet"))
}

fn http() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("tab/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client")
    })
}

/// Starts signing in. `loopback` is tab's MCP server (`http://127.0.0.1:port`).
/// Returns the page to open in the browser.
pub fn begin(provider: Provider, loopback: &str) -> anyhow::Result<String> {
    let redirect = format!("{}/oauth/callback", loopback.trim_end_matches('/'));
    let client = provider.client(&redirect).ok_or_else(|| {
        anyhow::anyhow!(
            "This build of tab can't sign in with {} yet. Use an app password instead.",
            provider.name()
        )
    })?;
    let (verifier, challenge) = oauth::pkce();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut url = reqwest::Url::parse(&oauth::authorize_url(
        &provider.endpoints(),
        &client,
        &state,
        &challenge,
    )?)?;
    url.query_pairs_mut().extend_pairs(provider.extra_params());
    let mut pending = PENDING.lock().unwrap();
    let pending = pending.get_or_insert_with(HashMap::new);
    pending.retain(|_, p| p.started.elapsed() < PENDING_FOR && p.provider != provider);
    pending.insert(
        state,
        Pending {
            provider,
            verifier,
            client,
            started: Instant::now(),
        },
    );
    Ok(url.into())
}

/// Whether `state` is an email sign-in this app started (rather than an
/// MCP one).
pub fn is_pending(state: &str) -> bool {
    PENDING
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|p| p.contains_key(state))
}

/// The address an ID token names.
fn address_of(tokens: &Tokens) -> Option<String> {
    let claims = oauth::id_claims(tokens.id_token.as_deref()?)?;
    ["email", "preferred_username", "upn"]
        .iter()
        .find_map(|k| claims[*k].as_str().filter(|s| s.contains('@')))
        .map(str::to_string)
}

/// The browser came back: trades the code for tokens and keeps the sign-in.
pub async fn finish(
    state: &str,
    code: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<SignedIn> {
    let p = PENDING
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|m| m.remove(state))
        .filter(|p| p.started.elapsed() < PENDING_FOR)
        .ok_or_else(|| {
            anyhow::anyhow!("This sign-in link expired or was already used. Start again in tab.")
        })?;
    let name = p.provider.name();
    if let Some(e) = error {
        anyhow::bail!("{name} said: {e}");
    }
    let code = code.ok_or_else(|| anyhow::anyhow!("{name} sent no code back"))?;
    let tokens = oauth::exchange(
        http(),
        &p.provider.endpoints(),
        &p.client,
        code,
        &p.verifier,
        Utc::now(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{name} didn't accept the sign-in: {e}"))?;
    anyhow::ensure!(
        tokens.refresh_token.is_some(),
        "{name} didn't let tab stay signed in (no refresh token). Try again."
    );
    let address = address_of(&tokens)
        .ok_or_else(|| anyhow::anyhow!("{name} didn't say which address signed in"))?;
    let record = MailSignIn {
        provider: p.provider,
        address,
        client: p.client,
        tokens,
        signed_in_at: Utc::now(),
    };
    save(&record)?;
    Ok(SignedIn {
        provider: record.provider,
        address: record.address,
        signed_in_at: record.signed_in_at,
    })
}

fn save(record: &MailSignIn) -> anyhow::Result<()> {
    vault()?.set(ENTRY, &serde_json::to_string(record)?)
}

fn record() -> Option<MailSignIn> {
    serde_json::from_str(&vault().ok()?.get(ENTRY)?).ok()
}

/// Who's signed in, if anyone.
pub fn signed_in() -> Option<SignedIn> {
    record().map(|r| SignedIn {
        provider: r.provider,
        address: r.address,
        signed_in_at: r.signed_in_at,
    })
}

/// Forgets the sign-in.
pub fn sign_out() {
    if let Ok(v) = vault() {
        v.remove(ENTRY);
    }
}

/// The IMAP user and a fresh access token for `provider`, refreshed when
/// it's stale.
pub async fn access_token(provider: Provider) -> anyhow::Result<(String, String)> {
    let again = || {
        anyhow::anyhow!(
            "Sign in with {} again in Connections → Email.",
            provider.name()
        )
    };
    let r = record()
        .filter(|r| r.provider == provider)
        .ok_or_else(again)?;
    if !r.tokens.stale(Utc::now()) {
        return Ok((r.address, r.tokens.access_token));
    }
    let _one_at_a_time = REFRESHING.lock().await;
    // Another connection may have refreshed it while this one waited.
    let mut r = record()
        .filter(|r| r.provider == provider)
        .ok_or_else(again)?;
    if !r.tokens.stale(Utc::now()) {
        return Ok((r.address, r.tokens.access_token));
    }
    let Some(rt) = r.tokens.refresh_token.clone() else {
        return Err(again());
    };
    let d = provider.endpoints();
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", rt.clone()),
    ];
    if provider == Provider::Microsoft {
        form.push(("scope", d.scope.clone().unwrap_or_default()));
    }
    let mut t = oauth::token_request(http(), &d.token_endpoint, &r.client, form, Utc::now())
        .await
        .map_err(|e| {
            tracing::warn!(provider = provider.id(), error = %e, "email sign-in refresh failed");
            again()
        })?;
    // Microsoft sends a new refresh token each time; Google keeps the old.
    if t.refresh_token.is_none() {
        t.refresh_token = Some(rt);
    }
    r.tokens = t;
    save(&r)?;
    Ok((r.address, r.tokens.access_token))
}

/// SASL XOAUTH2 for IMAP `AUTHENTICATE` (Google's and Microsoft's format).
pub struct XOAuth2 {
    pub user: String,
    pub token: String,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_token(claims: serde_json::Value) -> String {
        use base64::Engine;
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            b.encode(r#"{"alg":"RS256"}"#),
            b.encode(claims.to_string())
        )
    }

    fn tokens(id: Option<String>) -> Tokens {
        Tokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: None,
            scope: None,
            id_token: id,
        }
    }

    #[test]
    fn reads_the_address_from_the_id_token() {
        let t = tokens(Some(id_token(serde_json::json!({"email": "me@gmail.com"}))));
        assert_eq!(address_of(&t).as_deref(), Some("me@gmail.com"));
        // Microsoft personal accounts may only have preferred_username.
        let t = tokens(Some(id_token(
            serde_json::json!({"preferred_username": "me@outlook.com"}),
        )));
        assert_eq!(address_of(&t).as_deref(), Some("me@outlook.com"));
        let t = tokens(Some(id_token(
            serde_json::json!({"preferred_username": "me"}),
        )));
        assert_eq!(address_of(&t), None);
        assert_eq!(address_of(&tokens(None)), None);
    }

    #[test]
    fn microsoft_is_never_sent_resource() {
        let d = Provider::Microsoft.endpoints();
        let c = Client {
            client_id: "id".into(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:47821/oauth/callback".into(),
        };
        let u = oauth::authorize_url(&d, &c, "st", "ch").unwrap();
        let u = reqwest::Url::parse(&u).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert!(!q.contains_key("resource"));
        assert!(q["scope"].contains("IMAP.AccessAsUser.All"));
        assert!(q["scope"].contains("offline_access"));
    }

    #[test]
    fn xoauth2_is_the_sasl_string_imap_wants() {
        use async_imap::Authenticator;
        let mut a = XOAuth2 {
            user: "me@gmail.com".into(),
            token: "ya29".into(),
        };
        assert_eq!(
            a.process(b""),
            "user=me@gmail.com\x01auth=Bearer ya29\x01\x01"
        );
    }

    #[test]
    fn a_provider_without_a_client_isnt_offered() {
        // SAFETY: tests in this module don't read these from other threads.
        unsafe { std::env::remove_var("TAB_MICROSOFT_CLIENT_ID") };
        if option_env!("TAB_MICROSOFT_CLIENT_ID").is_none() {
            assert!(!Provider::Microsoft.available());
            assert!(begin(Provider::Microsoft, "http://127.0.0.1:1").is_err());
        }
        unsafe { std::env::set_var("TAB_MICROSOFT_CLIENT_ID", "abc") };
        assert!(Provider::Microsoft.available());
        let url = begin(Provider::Microsoft, "http://127.0.0.1:1").unwrap();
        let state = reqwest::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        assert!(is_pending(&state));
        assert!(url.contains("prompt=select_account"));
        unsafe { std::env::remove_var("TAB_MICROSOFT_CLIENT_ID") };
    }
}
