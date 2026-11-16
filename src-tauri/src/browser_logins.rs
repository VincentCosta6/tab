//! Signing in inside the in-app browser: logins imported from the person's
//! other browsers ([`tab_core::browser_cookies`]), and tab's own passkeys
//! ([`tab_core::passkeys`]).
//!
//! Pages reach passkeys through the `passkey` plugin's one command, which
//! `capabilities/browser.json` opens to the browser's webviews (only).
//! The command takes the origin from the webview, not from the page, and
//! asks for Touch ID before every use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tab_core::browser_cookies::{self, Cookie, Site, Source, SourceView};
use tab_core::e2e::Presence;
use tab_core::passkeys::{self, Account, CreateOptions, Credential, GetOptions, Passkeys, Refusal};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

use crate::{CmdResult, err};

/// How long a "which account?" question waits for an answer.
const CHOOSE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Logins {
    passkeys: Passkeys,
    /// "Which account?" questions waiting on the person, by request id.
    choosing: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
}

impl Logins {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            passkeys: Passkeys::new(dir),
            choosing: Mutex::new(HashMap::new()),
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[allow(deprecated)]
fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

fn source(id: &str) -> CmdResult<Source> {
    browser_cookies::sources(&home())
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "that browser profile is gone".to_string())
}

/// Browser profiles on this computer tab can import logins from.
#[tauri::command]
pub async fn browser_login_sources() -> CmdResult<Vec<SourceView>> {
    tokio::task::spawn_blocking(|| {
        browser_cookies::sources(&home())
            .iter()
            .map(SourceView::from)
            .collect()
    })
    .await
    .map_err(err)
}

/// The sites a profile is signed in to (has cookies for).
#[tauri::command]
pub async fn browser_login_sites(source_id: String) -> CmdResult<Vec<Site>> {
    let src = source(&source_id)?;
    tokio::task::spawn_blocking(move || browser_cookies::sites(&src, now()))
        .await
        .map_err(err)?
        .map_err(err)
}

/// Copies `sites`' cookies from a profile into the in-app browser. Returns
/// how many were copied.
#[tauri::command]
pub async fn browser_import_logins(
    app: AppHandle,
    source_id: String,
    sites: Vec<String>,
) -> CmdResult<usize> {
    let src = source(&source_id)?;
    let cookies =
        tokio::task::spawn_blocking(move || browser_cookies::cookies(&src, &sites, now()))
            .await
            .map_err(err)?
            .map_err(err)?;
    put_cookies(&app, cookies).await.map_err(err)
}

/// Into WebKit's default store, which every browser tab shares. Straight
/// through WebKit rather than `Webview::set_cookie`: that drops a domain
/// cookie's leading dot (making `.google.com` host-only) and panics on a
/// cookie WebKit won't build.
#[cfg(target_os = "macos")]
async fn put_cookies(app: &AppHandle, cookies: Vec<Cookie>) -> anyhow::Result<usize> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_web_kit::WKWebsiteDataStore;

    let (tx, rx) = oneshot::channel::<usize>();
    app.run_on_main_thread(move || {
        let mtm = MainThreadMarker::new().expect("on the main thread");
        let made: Vec<_> = cookies.iter().filter_map(ns_cookie).collect();
        let count = made.len();
        if count == 0 {
            let _ = tx.send(0);
            return;
        }
        // SAFETY: on the main thread, as WebKit requires.
        let store = unsafe { WKWebsiteDataStore::defaultDataStore(mtm).httpCookieStore() };
        let left = Arc::new(AtomicUsize::new(count));
        let tx = Arc::new(Mutex::new(Some(tx)));
        for c in made {
            let (left, tx) = (left.clone(), tx.clone());
            let done = RcBlock::new(move || {
                if left.fetch_sub(1, Ordering::SeqCst) == 1
                    && let Some(tx) = tx.lock().unwrap().take()
                {
                    let _ = tx.send(count);
                }
            });
            unsafe { store.setCookie_completionHandler(&c, Some(&done)) };
        }
    })?;
    Ok(tokio::time::timeout(Duration::from_secs(60), rx).await??)
}

#[cfg(target_os = "macos")]
fn ns_cookie(c: &Cookie) -> Option<objc2::rc::Retained<objc2_foundation::NSHTTPCookie>> {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{
        NSDate, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieExpires, NSHTTPCookieName,
        NSHTTPCookiePath, NSHTTPCookiePropertyKey, NSHTTPCookieSecure, NSHTTPCookieValue,
        NSMutableDictionary, NSString, ns_string,
    };
    use tab_core::browser_cookies::SameSite;

    if c.name.is_empty() {
        return None;
    }
    let path = if c.path.is_empty() { "/" } else { &c.path };
    let strings = [
        &*NSString::from_str(&c.name),
        &*NSString::from_str(&c.value),
        &*NSString::from_str(path),
        &*NSString::from_str(&c.domain),
    ];
    // SAFETY: Foundation's constant keys.
    let keys = unsafe {
        [
            NSHTTPCookieName,
            NSHTTPCookieValue,
            NSHTTPCookiePath,
            NSHTTPCookieDomain,
        ]
    };
    let props: objc2::rc::Retained<NSMutableDictionary<NSHTTPCookiePropertyKey, AnyObject>> =
        NSMutableDictionary::from_slices(&keys, &strings.map(|s| s as &AnyObject));
    unsafe {
        if let Some(e) = c.expires {
            props.insert(
                NSHTTPCookieExpires,
                &*NSDate::dateWithTimeIntervalSince1970(e as f64),
            );
        }
        if c.secure {
            props.insert(NSHTTPCookieSecure, ns_string!("TRUE"));
        }
        // Not in Foundation's constants, but NSHTTPCookie reads them.
        if c.http_only {
            props.insert(ns_string!("HttpOnly"), ns_string!("TRUE"));
        }
        match c.same_site {
            Some(SameSite::Lax) => props.insert(ns_string!("SameSite"), ns_string!("lax")),
            Some(SameSite::Strict) => props.insert(ns_string!("SameSite"), ns_string!("strict")),
            _ => {}
        }
        NSHTTPCookie::cookieWithProperties(&props)
    }
}

/// Elsewhere through the webview (Firefox profiles only). The cookie crate
/// drops a leading dot, so domain cookies arrive host-only.
#[cfg(not(target_os = "macos"))]
async fn put_cookies(app: &AppHandle, cookies: Vec<Cookie>) -> anyhow::Result<usize> {
    use tauri::webview::cookie::{self, time::OffsetDateTime};
    let wv = app
        .get_webview("main")
        .ok_or_else(|| anyhow::anyhow!("no main window"))?;
    tokio::task::spawn_blocking(move || {
        let mut n = 0;
        for c in cookies {
            let mut b = cookie::Cookie::build((c.name, c.value))
                .domain(c.domain)
                .path(if c.path.is_empty() {
                    "/".to_string()
                } else {
                    c.path
                })
                .secure(c.secure)
                .http_only(c.http_only);
            if let Some(t) = c
                .expires
                .and_then(|e| OffsetDateTime::from_unix_timestamp(e).ok())
            {
                b = b.expires(t);
            }
            if wv.set_cookie(b.build()).is_ok() {
                n += 1;
            }
        }
        n
    })
    .await
    .map_err(Into::into)
}

/// Asks the dock which passkey to sign in with, or tells it there's none.
#[derive(Serialize, Clone)]
struct PasskeyEvent {
    kind: &'static str,
    request: String,
    /// The browser tab asking.
    tab: String,
    site: String,
    accounts: Vec<Account>,
}

/// The dock's answer to "which account?": a passkey id, or none to cancel.
#[tauri::command]
pub fn browser_passkey_choose(state: State<'_, Logins>, request: String, passkey: Option<String>) {
    if let Some(tx) = state.choosing.lock().unwrap().remove(&request) {
        let _ = tx.send(passkey);
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Refusal> + Send + 'static,
) -> Result<T, Refusal> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Refusal::not_allowed(e.to_string()))?
}

async fn confirm(reason: String) -> Result<(), Refusal> {
    tab_core::e2e::presence::System
        .confirm(&reason)
        .await
        .map_err(|e| Refusal::not_allowed(e.to_string()))
}

/// A page's `navigator.credentials.create` / `.get` for a passkey.
#[tauri::command]
async fn request(
    app: AppHandle,
    webview: tauri::Webview,
    kind: String,
    options: serde_json::Value,
) -> Result<Credential, Refusal> {
    let Some(tab) = webview.label().strip_prefix("browser-").map(str::to_string) else {
        return Err(Refusal::not_allowed(
            "only the in-app browser uses tab's passkeys",
        ));
    };
    let origin = webview
        .url()
        .map_err(|e| Refusal::not_allowed(e.to_string()))?;
    let origin: url::Url = origin
        .as_str()
        .parse()
        .map_err(|_| Refusal::not_allowed("bad page address"))?;
    let bad = |e: serde_json::Error| Refusal {
        name: "TypeError",
        message: e.to_string(),
    };
    let logins = app.state::<Logins>();
    match kind.as_str() {
        "create" => {
            let opts: CreateOptions = serde_json::from_value(options).map_err(bad)?;
            let rp = {
                let (app, origin, opts) = (app.clone(), origin.clone(), opts.clone());
                blocking(move || {
                    app.state::<Logins>()
                        .passkeys
                        .with(|k| passkeys::check_register(k, &origin, &opts))
                })
                .await?
            };
            let who = if opts.user.name.is_empty() {
                &opts.user.display_name
            } else {
                &opts.user.name
            };
            confirm(format!("save a passkey for {rp} as {who}")).await?;
            let app = app.clone();
            blocking(move || {
                app.state::<Logins>()
                    .passkeys
                    .with(|k| passkeys::register(k, &origin, &opts, now()))
            })
            .await
        }
        "get" => {
            let opts: GetOptions = serde_json::from_value(options).map_err(bad)?;
            let (rp, accounts) = {
                let (app, origin, opts) = (app.clone(), origin.clone(), opts.clone());
                blocking(move || {
                    app.state::<Logins>()
                        .passkeys
                        .with(|k| passkeys::candidates(k, &origin, &opts))
                })
                .await?
            };
            let ask = |kind, request: &str, accounts| {
                let _ = app.emit_to(
                    "main",
                    "tab://passkey",
                    PasskeyEvent {
                        kind,
                        request: request.into(),
                        tab: tab.clone(),
                        site: rp.clone(),
                        accounts,
                    },
                );
            };
            let chosen = match accounts.as_slice() {
                [] => {
                    ask("none", "", Vec::new());
                    return Err(Refusal::not_allowed(format!("tab has no passkey for {rp}")));
                }
                [one] => one.clone(),
                _ => {
                    let id = uuid::Uuid::new_v4().to_string();
                    let (tx, rx) = oneshot::channel();
                    logins.choosing.lock().unwrap().insert(id.clone(), tx);
                    ask("choose", &id, accounts.clone());
                    let picked = tokio::time::timeout(CHOOSE_TIMEOUT, rx).await;
                    logins.choosing.lock().unwrap().remove(&id);
                    let picked = picked.ok().and_then(Result::ok).flatten();
                    ask("done", &id, Vec::new());
                    accounts
                        .into_iter()
                        .find(|a| Some(&a.id) == picked.as_ref())
                        .ok_or_else(|| Refusal::not_allowed("cancelled"))?
                }
            };
            confirm(format!("sign in to {rp} as {}", chosen.name)).await?;
            let app = app.clone();
            blocking(move || {
                app.state::<Logins>()
                    .passkeys
                    .with(|k| passkeys::assert(k, &origin, &opts, &chosen.id))
            })
            .await
        }
        _ => Err(Refusal {
            name: "NotSupportedError",
            message: "unknown request".into(),
        }),
    }
}

/// The `passkey` plugin pages call (see `capabilities/browser.json`).
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("passkey")
        .invoke_handler(tauri::generate_handler![request])
        .build()
}

/// Run in each browser tab's top frame. Only on macOS: elsewhere the system
/// webview has passkeys of its own.
pub const SHIM: &str = include_str!("passkey_shim.js");
