//! Signing in to a remote MCP server, the way the MCP authorization spec
//! describes, as a public client with no setup on our side:
//!
//! 1. The server's `401` names its Protected Resource Metadata (RFC 9728),
//!    which names the authorization server.
//! 2. The authorization server's metadata (RFC 8414 / OpenID discovery).
//! 3. Dynamic Client Registration (RFC 7591) for a client id.
//! 4. Authorization code with PKCE (S256) and `resource` (RFC 8707), in the
//!    person's browser, back to tab's loopback address.
//! 5. Refresh tokens from then on.

use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Where to sign in, found from the server's URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Discovered {
    /// The MCP server as the authorization server knows it (`resource`).
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    /// What to ask for, from the 401 or the resource's metadata.
    pub scope: Option<String>,
}

/// This app's registration with one authorization server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Client {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Tokens {
    /// Expired, or within a minute of it.
    pub fn stale(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .is_some_and(|t| t - Duration::seconds(60) <= now)
    }
}

/// `key="value"` parameters of a `WWW-Authenticate: Bearer …` header.
pub fn challenge_param(header: &str, key: &str) -> Option<String> {
    let rest = header.trim();
    let rest = rest
        .strip_prefix("Bearer")
        .or_else(|| rest.strip_prefix("bearer"))
        .unwrap_or(rest);
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let name: String = chars
            .by_ref()
            .take_while(|c| *c != '=')
            .collect::<String>()
            .trim()
            .to_string();
        if name.is_empty() {
            return None;
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => v.extend(chars.next()),
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            chars
                .by_ref()
                .take_while(|c| *c != ',')
                .collect::<String>()
                .trim()
                .to_string()
        };
        if name.eq_ignore_ascii_case(key) {
            return Some(value);
        }
    }
}

fn origin(u: &Url) -> String {
    u.origin().ascii_serialization()
}

/// Whether `resource` (from a server's resource metadata, RFC 9728) names
/// the server at `server`: the same scheme, host and port. RFC 9728 §3.3
/// has the client check this, or a server could name another's resource
/// and authorization server, get the person to sign in there, and be sent
/// that token. Paths aren't compared: servers differ on whether they name
/// their origin or their endpoint (and some have moved endpoints), and a
/// token for one origin only goes back to that origin.
pub fn resource_matches(resource: &str, server: &str) -> bool {
    let (Ok(r), Ok(s)) = (Url::parse(resource), Url::parse(server)) else {
        return false;
    };
    r.origin().is_tuple() && r.origin() == s.origin()
}

/// Whether a server-supplied OAuth address is safe to send the person or
/// a secret to: https, or plain http only to this computer.
pub fn secure_url(u: &str) -> bool {
    let Ok(u) = Url::parse(u) else {
        return false;
    };
    match u.scheme() {
        "https" => u.host().is_some(),
        "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

/// RFC 8414 / OIDC: where an issuer's metadata may be, in order.
pub fn metadata_urls(issuer: &str) -> Vec<String> {
    let Ok(u) = Url::parse(issuer) else {
        return Vec::new();
    };
    let o = origin(&u);
    let path = u.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            format!("{o}/.well-known/oauth-authorization-server"),
            format!("{o}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{o}/.well-known/oauth-authorization-server{path}"),
            format!("{o}/.well-known/openid-configuration{path}"),
            format!("{o}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// RFC 9728: where a resource's metadata may be when the 401 doesn't say.
pub fn resource_metadata_urls(server: &str) -> Vec<String> {
    let Ok(u) = Url::parse(server) else {
        return Vec::new();
    };
    let o = origin(&u);
    let path = u.path().trim_end_matches('/');
    let mut v = Vec::new();
    if !path.is_empty() {
        v.push(format!("{o}/.well-known/oauth-protected-resource{path}"));
    }
    v.push(format!("{o}/.well-known/oauth-protected-resource"));
    v
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<Value> {
    let r = http
        .get(url)
        .header("accept", "application/json")
        .header("MCP-Protocol-Version", super::PROTOCOL_VERSION)
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json::<Value>().await.ok().filter(|v| v.is_object())
}

/// Finds where to sign in to `server`. `challenge` is the 401's
/// `WWW-Authenticate` header, if there was one.
pub async fn discover(
    http: &reqwest::Client,
    server: &str,
    challenge: Option<&str>,
) -> anyhow::Result<Discovered> {
    let mut candidates = Vec::new();
    if let Some(u) = challenge.and_then(|c| challenge_param(c, "resource_metadata")) {
        candidates.push(u);
    }
    candidates.extend(resource_metadata_urls(server));
    let mut prm = None;
    for u in &candidates {
        if let Some(v) = get_json(http, u).await
            && v["authorization_servers"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        {
            prm = Some(v);
            break;
        }
    }
    let server_url = Url::parse(server)?;
    // Without resource metadata (older servers), the server's own origin
    // is the authorization server.
    let (resource, issuer, scopes) = match &prm {
        Some(p) => (
            p["resource"].as_str().unwrap_or(server).to_string(),
            p["authorization_servers"][0]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            p["scopes_supported"].as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        ),
        None => (server.to_string(), origin(&server_url), None),
    };
    anyhow::ensure!(
        resource_matches(&resource, server),
        "{server} says it's {resource}, a different server, so tab won't sign in to it"
    );
    anyhow::ensure!(
        secure_url(&issuer),
        "{server}'s sign-in server ({issuer}) isn't https, so tab won't sign in to it"
    );
    let scope = challenge
        .and_then(|c| challenge_param(c, "scope"))
        .or(scopes)
        .filter(|s| !s.trim().is_empty());

    let mut meta = None;
    for u in metadata_urls(&issuer) {
        if let Some(v) = get_json(http, &u).await
            && v["authorization_endpoint"].is_string()
            && v["token_endpoint"].is_string()
        {
            meta = Some(v);
            break;
        }
    }
    let d = match meta {
        Some(m) => {
            let methods = m["code_challenge_methods_supported"].as_array();
            // Servers that say nothing get PKCE anyway; ones that list
            // methods without S256 can't be signed in to safely.
            anyhow::ensure!(
                methods.is_none_or(|a| a.iter().any(|x| x == "S256")),
                "{issuer} doesn't support PKCE (S256), so tab can't sign in to it safely"
            );
            Discovered {
                resource,
                issuer: m["issuer"].as_str().unwrap_or(&issuer).to_string(),
                authorization_endpoint: m["authorization_endpoint"].as_str().unwrap().into(),
                token_endpoint: m["token_endpoint"].as_str().unwrap().into(),
                registration_endpoint: m["registration_endpoint"].as_str().map(Into::into),
                scope,
            }
        }
        // The 2025-03-26 spec's defaults.
        None => {
            let o = issuer.trim_end_matches('/');
            Discovered {
                resource,
                issuer: issuer.clone(),
                authorization_endpoint: format!("{o}/authorize"),
                token_endpoint: format!("{o}/token"),
                registration_endpoint: Some(format!("{o}/register")),
                scope,
            }
        }
    };
    for u in [&d.authorization_endpoint, &d.token_endpoint]
        .into_iter()
        .chain(&d.registration_endpoint)
    {
        anyhow::ensure!(
            secure_url(u),
            "{} gave a sign-in address that isn't https ({u}), so tab won't sign in to it",
            d.issuer
        );
    }
    Ok(d)
}

/// Registers tab with the authorization server (RFC 7591).
pub async fn register(
    http: &reqwest::Client,
    d: &Discovered,
    redirect_uri: &str,
) -> anyhow::Result<Client> {
    let Some(endpoint) = &d.registration_endpoint else {
        anyhow::bail!(
            "{} doesn't let apps register themselves, so tab can't sign in to it yet",
            d.issuer
        );
    };
    let mut body = json!({
        "client_name": "tab",
        "client_uri": "https://tab.dev",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(s) = &d.scope {
        body["scope"] = s.clone().into();
    }
    let r = http.post(endpoint).json(&body).send().await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success() && v["client_id"].is_string(),
        "registering with {} failed ({status}): {}",
        d.issuer,
        error_text(&v)
    );
    Ok(Client {
        client_id: v["client_id"].as_str().unwrap().into(),
        client_secret: v["client_secret"].as_str().map(Into::into),
        redirect_uri: redirect_uri.into(),
    })
}

/// A PKCE verifier and its S256 challenge.
pub fn pkce() -> (String, String) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The page the person signs in on.
pub fn authorize_url(
    d: &Discovered,
    c: &Client,
    state: &str,
    challenge: &str,
) -> anyhow::Result<String> {
    let mut u = Url::parse(&d.authorization_endpoint)?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &c.client_id)
            .append_pair("redirect_uri", &c.redirect_uri)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("resource", &d.resource);
        if let Some(s) = &d.scope {
            q.append_pair("scope", s);
        }
    }
    Ok(u.into())
}

fn error_text(v: &Value) -> String {
    match (v["error"].as_str(), v["error_description"].as_str()) {
        (Some(e), Some(d)) => format!("{e}: {d}"),
        (Some(e), None) => e.into(),
        _ => "no details".into(),
    }
}

async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    c: &Client,
    mut form: Vec<(&str, String)>,
    now: DateTime<Utc>,
) -> anyhow::Result<Tokens> {
    form.push(("client_id", c.client_id.clone()));
    if let Some(s) = &c.client_secret {
        form.push(("client_secret", s.clone()));
    }
    let r = http
        .post(endpoint)
        .header("accept", "application/json")
        .form(&form)
        .send()
        .await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success() && v["access_token"].is_string(),
        "{}",
        error_text(&v)
    );
    Ok(Tokens {
        access_token: v["access_token"].as_str().unwrap().into(),
        refresh_token: v["refresh_token"].as_str().map(Into::into),
        expires_at: v["expires_in"]
            .as_i64()
            .or_else(|| v["expires_in"].as_str()?.parse().ok())
            .map(|s| now + Duration::seconds(s)),
        scope: v["scope"].as_str().map(Into::into),
    })
}

/// Trades the code from the browser for tokens.
pub async fn exchange(
    http: &reqwest::Client,
    d: &Discovered,
    c: &Client,
    code: &str,
    verifier: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Tokens> {
    token_request(
        http,
        &d.token_endpoint,
        c,
        vec![
            ("grant_type", "authorization_code".into()),
            ("code", code.into()),
            ("redirect_uri", c.redirect_uri.clone()),
            ("code_verifier", verifier.into()),
            ("resource", d.resource.clone()),
        ],
        now,
    )
    .await
}

/// New tokens from the refresh token. Servers that rotate refresh tokens
/// send a new one; otherwise the old one stays.
pub async fn refresh(
    http: &reqwest::Client,
    d: &Discovered,
    c: &Client,
    old: &Tokens,
    now: DateTime<Utc>,
) -> anyhow::Result<Tokens> {
    let Some(rt) = &old.refresh_token else {
        anyhow::bail!("the sign-in expired");
    };
    let mut t = token_request(
        http,
        &d.token_endpoint,
        c,
        vec![
            ("grant_type", "refresh_token".into()),
            ("refresh_token", rt.clone()),
            ("resource", d.resource.clone()),
        ],
        now,
    )
    .await?;
    if t.refresh_token.is_none() {
        t.refresh_token = old.refresh_token.clone();
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_challenge_params() {
        let h = r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", scope="read write""#;
        assert_eq!(
            challenge_param(h, "resource_metadata").as_deref(),
            Some("https://mcp.linear.app/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(challenge_param(h, "scope").as_deref(), Some("read write"));
        assert_eq!(challenge_param(h, "realm").as_deref(), Some("OAuth"));
        assert_eq!(challenge_param(h, "missing"), None);
        let h = r#"Bearer error="invalid_token", error_description="No \"auth\"", resource_metadata="https://x/.well-known/oauth-protected-resource""#;
        assert_eq!(
            challenge_param(h, "resource_metadata").as_deref(),
            Some("https://x/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            challenge_param("Bearer error=invalid", "error").as_deref(),
            Some("invalid")
        );
    }

    #[test]
    fn builds_metadata_urls_for_issuers_with_paths() {
        assert_eq!(
            metadata_urls("https://access.stripe.com/mcp"),
            [
                "https://access.stripe.com/.well-known/oauth-authorization-server/mcp",
                "https://access.stripe.com/.well-known/openid-configuration/mcp",
                "https://access.stripe.com/mcp/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            metadata_urls("https://mcp.linear.app"),
            [
                "https://mcp.linear.app/.well-known/oauth-authorization-server",
                "https://mcp.linear.app/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            resource_metadata_urls("https://mcp.atlassian.com/v1/mcp"),
            [
                "https://mcp.atlassian.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.atlassian.com/.well-known/oauth-protected-resource",
            ]
        );
    }

    #[test]
    fn resource_must_be_the_server_itself() {
        let s = "https://mcp.linear.app/mcp";
        assert!(resource_matches("https://mcp.linear.app/mcp", s));
        assert!(resource_matches("https://mcp.linear.app/mcp/", s));
        assert!(resource_matches("https://mcp.linear.app", s));
        assert!(resource_matches("https://mcp.linear.app/", s));
        // Another server's resource, or a sibling path.
        assert!(resource_matches("https://mcp.linear.app/sse", s));
        // Another server's resource.
        assert!(!resource_matches(
            "https://mcp.linear.app/mcp",
            "https://evil.example/mcp"
        ));
        assert!(!resource_matches(
            "https://mcp.linear.app.evil.example/mcp",
            s
        ));
        assert!(!resource_matches("http://mcp.linear.app/mcp", s));
        assert!(!resource_matches("https://mcp.linear.app:8443/mcp", s));
        assert!(!resource_matches("not a url", s));
    }

    #[test]
    fn sign_in_addresses_must_be_https() {
        assert!(secure_url("https://mcp.linear.app/authorize"));
        assert!(secure_url("http://127.0.0.1:7777/token"));
        assert!(secure_url("http://localhost/token"));
        assert!(!secure_url("http://mcp.linear.app/authorize"));
        assert!(!secure_url("file:///etc/passwd"));
        assert!(!secure_url("javascript:alert(1)"));
        assert!(!secure_url("ms-settings:"));
    }

    #[test]
    fn pkce_challenge_is_the_verifiers_hash() {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let (v, c) = pkce();
        assert!(v.len() >= 43 && v.len() <= 128);
        assert_eq!(
            c,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes()))
        );
        assert_ne!(pkce().0, v);
    }

    #[test]
    fn authorize_url_carries_pkce_state_and_resource() {
        let d = Discovered {
            resource: "https://mcp.linear.app/mcp".into(),
            issuer: "https://mcp.linear.app".into(),
            authorization_endpoint: "https://mcp.linear.app/authorize".into(),
            token_endpoint: "https://mcp.linear.app/token".into(),
            registration_endpoint: None,
            scope: Some("read write".into()),
        };
        let c = Client {
            client_id: "abc".into(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:7777/oauth/callback".into(),
        };
        let u = Url::parse(&authorize_url(&d, &c, "st", "ch").unwrap()).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "abc");
        assert_eq!(q["code_challenge"], "ch");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st");
        assert_eq!(q["resource"], "https://mcp.linear.app/mcp");
        assert_eq!(q["scope"], "read write");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:7777/oauth/callback");
    }

    #[test]
    fn tokens_go_stale_a_minute_early() {
        let now = Utc::now();
        let t = |secs| Tokens {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(now + Duration::seconds(secs)),
            scope: None,
        };
        assert!(t(30).stale(now));
        assert!(!t(600).stale(now));
        assert!(
            !Tokens {
                expires_at: None,
                ..t(0)
            }
            .stale(now)
        );
    }
}
