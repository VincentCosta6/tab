//! The desktop side of agents in the browser (`docs/agent-browser.md`):
//! running tab's page script in an agent tab's isolated JavaScript world,
//! screenshots, and lending a login into an agent tab's own data store.
//!
//! Only agent tabs (`tab_core::browser_agent::agent_tab`) are ever touched
//! here; the person's own tabs never are. Live pages need an isolated world
//! (rule 9), which only macOS's WebKit gives tab today: elsewhere every
//! function here refuses, and agents fall back to reading pages as text.

use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use std::time::Duration;

use serde_json::Value;
use tab_core::browser_agent::{self, page::Op};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Manager;

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

#[cfg(target_os = "macos")]
fn agent_webview(tab_id: &str) -> anyhow::Result<tauri::Webview> {
    anyhow::ensure!(
        browser_agent::agent_tab(tab_id).is_some(),
        "{tab_id} isn't an agent tab"
    );
    let app = APP
        .get()
        .ok_or_else(|| anyhow::anyhow!("the app isn't ready"))?;
    app.get_webview(&format!("browser-{tab_id}"))
        .ok_or_else(|| anyhow::anyhow!("the tab isn't showing yet"))
}

/// Waits for the dock to create the tab's webview (it does when it renders).
#[cfg(target_os = "macos")]
async fn webview_soon(tab_id: &str) -> anyhow::Result<tauri::Webview> {
    for _ in 0..40 {
        if let Ok(wv) = agent_webview(tab_id) {
            return Ok(wv);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    agent_webview(tab_id)
}

/// Runs one op of tab's page script in the tab's isolated world and
/// returns what it reported.
#[cfg(target_os = "macos")]
pub async fn run_op(tab_id: &str, op: &Op) -> anyhow::Result<Value> {
    use block2::RcBlock;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSError, NSString, ns_string};
    use objc2_web_kit::{WKContentWorld, WKWebView};

    let wv = webview_soon(tab_id).await?;
    let script = browser_agent::page::script(op);
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let tx = std::sync::Mutex::new(Some(tx));
    wv.with_webview(move |pv| {
        // SAFETY: on the main thread (with_webview runs there); `inner` is
        // this webview's WKWebView.
        unsafe {
            let wk: &WKWebView = &*pv.inner().cast();
            // A world of tab's own: the page can't see or change it.
            let mtm = objc2::MainThreadMarker::new().expect("on the main thread");
            let world = WKContentWorld::worldWithName(ns_string!("tab-agent"), mtm);
            let done = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                let out = if !error.is_null() {
                    Err((*error).localizedDescription().to_string())
                } else if let Some(s) = result.as_ref().and_then(|r| r.downcast_ref::<NSString>()) {
                    Ok(s.to_string())
                } else {
                    Err("the page script returned nothing".to_string())
                };
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(out);
                }
            });
            wk.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(&script),
                None,
                &world,
                Some(&done),
            );
        }
    })?;
    let text = tokio::time::timeout(Duration::from_secs(20), rx)
        .await
        .map_err(|_| anyhow::anyhow!("the page didn't answer"))??
        .map_err(|e| anyhow::anyhow!(e))?;
    let v: Value = serde_json::from_str(&text)?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        anyhow::bail!("{e}");
    }
    Ok(v)
}

#[cfg(not(target_os = "macos"))]
pub async fn run_op(_tab_id: &str, _op: &Op) -> anyhow::Result<Value> {
    anyhow::bail!(
        "live pages need an isolated script world, which tab only has on macOS so far; read pages as text instead"
    )
}

/// A PNG of the tab's view, `width` points wide.
#[cfg(target_os = "macos")]
pub async fn screenshot(tab_id: &str, width: f64) -> anyhow::Result<Vec<u8>> {
    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError, NSNumber};
    use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

    let wv = webview_soon(tab_id).await?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Vec<u8>, String>>();
    let tx = std::sync::Mutex::new(Some(tx));
    wv.with_webview(move |pv| unsafe {
        // SAFETY: as in `run_op`.
        let wk: &WKWebView = &*pv.inner().cast();
        let mtm = objc2::MainThreadMarker::new().expect("on the main thread");
        let cfg = WKSnapshotConfiguration::new(mtm);
        cfg.setSnapshotWidth(Some(&NSNumber::new_f64(width)));
        let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let out = if let Some(img) = image.as_ref() {
                img.TIFFRepresentation()
                    .and_then(|tiff| {
                        NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
                    })
                    .and_then(|rep| {
                        rep.representationUsingType_properties(
                            NSBitmapImageFileType::PNG,
                            &NSDictionary::new(),
                        )
                    })
                    .map(|png| png.to_vec())
                    .ok_or_else(|| "couldn't encode the screenshot".to_string())
            } else if let Some(e) = error.as_ref() {
                Err(e.localizedDescription().to_string())
            } else {
                Err("no screenshot".to_string())
            };
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(out);
            }
        });
        wk.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &done);
    })?;
    tokio::time::timeout(Duration::from_secs(20), rx)
        .await
        .map_err(|_| anyhow::anyhow!("the screenshot didn't come"))??
        .map_err(|e| anyhow::anyhow!(e))
}

#[cfg(not(target_os = "macos"))]
pub async fn screenshot(_tab_id: &str, _width: f64) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("screenshots of agent tabs need macOS for now")
}

/// Copies the person's cookies for `site` from the first of their browsers
/// signed in to it, into the agent tab's own data store (not the dock's
/// shared one). Needs "Import logins" on; the person approved lending.
#[cfg(target_os = "macos")]
pub async fn lend(tab_id: &str, site: &str) -> anyhow::Result<usize> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use block2::RcBlock;
    use objc2_web_kit::WKWebView;
    use tab_core::browser_cookies;

    let wv = webview_soon(tab_id).await?;
    let site = site.to_string();
    let cookies = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        #[allow(deprecated)]
        let home = std::env::home_dir().unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        for src in browser_cookies::sources(&home) {
            let signed_in = browser_cookies::sites(&src, now)
                .map(|s| s.iter().any(|x| x.site == site))
                .unwrap_or(false);
            if signed_in {
                return browser_cookies::cookies(&src, std::slice::from_ref(&site), now);
            }
        }
        anyhow::bail!("none of your browsers is signed in to {site}")
    })
    .await??;
    let (tx, rx) = tokio::sync::oneshot::channel::<usize>();
    wv.with_webview(move |pv| unsafe {
        // SAFETY: as in `run_op`. The tab's own store: an incognito
        // webview has a non-persistent one of its own.
        let wk: &WKWebView = &*pv.inner().cast();
        let store = wk.configuration().websiteDataStore().httpCookieStore();
        let made: Vec<_> = cookies
            .iter()
            .filter_map(crate::browser_logins::ns_cookie)
            .collect();
        let count = made.len();
        if count == 0 {
            let _ = tx.send(0);
            return;
        }
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
            store.setCookie_completionHandler(&c, Some(&done));
        }
    })?;
    Ok(tokio::time::timeout(Duration::from_secs(30), rx).await??)
}

#[cfg(not(target_os = "macos"))]
pub async fn lend(_tab_id: &str, _site: &str) -> anyhow::Result<usize> {
    anyhow::bail!("lending a login needs macOS for now; the person can sign in in the tab instead")
}
