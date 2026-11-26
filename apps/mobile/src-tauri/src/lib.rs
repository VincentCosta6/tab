//! tab on a phone.
//!
//! A phone can't run tab's engine (agents, the sandbox, connectors), so this
//! app is a client of the person's hub in tab cloud: it syncs a copy of
//! their work ([`link`]), and hands each chat they start to one of their
//! runners, tab cloud's or a computer's ([`chats`]), then watches and steers
//! it there. The UI is the React app beside this crate (`apps/mobile/src`).

mod chats;
mod link;

use std::sync::Arc;

use tab_core::account::Provider;
use tauri::{Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_opener::OpenerExt;

use link::{AccountState, Phone};

#[tauri::command]
fn account(phone: State<'_, Arc<Phone>>) -> AccountState {
    phone.state()
}

#[tauri::command]
fn sign_in(
    app: tauri::AppHandle,
    phone: State<'_, Arc<Phone>>,
    provider: Provider,
) -> Result<(), String> {
    let url = phone.sign_in(provider).map_err(|e| e.to_string())?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn sign_out(phone: State<'_, Arc<Phone>>) -> Result<(), String> {
    phone.sign_out().await;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tab_core=info".into()),
        )
        .try_init();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            let phone = Phone::new(app.handle())?;
            app.manage(phone.clone());
            let p = phone.clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    p.callback(url.as_str());
                }
            });
            // Opened from the sign-in link while not running.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    phone.callback(url.as_str());
                }
            }
            tauri::async_runtime::spawn(async move { phone.connect().await });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            account,
            sign_in,
            sign_out,
            chats::overview,
            chats::messages,
            chats::session,
            chats::plan_chat,
            chats::start_chat,
            chats::job_session,
            chats::steer,
            chats::stop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tab");
}
