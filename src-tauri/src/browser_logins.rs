//! Logins imported from the person's other browsers into the in-app browser
//! ([`tab_core::browser_cookies`]). Off until they turn it on in Settings →
//! Browser; every command checks.

use std::path::PathBuf;
use std::time::Duration;

use tab_core::browser_cookies::{self, Cookie, Site, Source, SourceView};
use tauri::{AppHandle, State};

use crate::{AppState, CmdResult, err};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[allow(deprecated)]
fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

async fn allowed(state: &AppState) -> CmdResult<()> {
    if state.config.lock().await.browser.import_logins {
        Ok(())
    } else {
        Err("importing logins is off; turn it on in Settings → Browser".into())
    }
}

fn source(id: &str) -> CmdResult<Source> {
    browser_cookies::sources(&home())
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "that browser profile is gone".to_string())
}

/// Browser profiles on this computer tab can import logins from.
#[tauri::command]
pub async fn browser_login_sources(state: State<'_, AppState>) -> CmdResult<Vec<SourceView>> {
    allowed(&state).await?;
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
pub async fn browser_login_sites(
    state: State<'_, AppState>,
    source_id: String,
) -> CmdResult<Vec<Site>> {
    allowed(&state).await?;
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
    state: State<'_, AppState>,
    source_id: String,
    sites: Vec<String>,
) -> CmdResult<usize> {
    allowed(&state).await?;
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

    let (tx, rx) = tokio::sync::oneshot::channel::<usize>();
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
        let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
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
    use tauri::Manager;
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
