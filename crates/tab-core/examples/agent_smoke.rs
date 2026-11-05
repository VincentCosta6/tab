//! End-to-end check of an agent turn against tab's MCP server.
//! `cargo run -p tab-core --example agent_smoke -- claude_code|opencode "prompt"`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tab_core::agents::{AgentHub, Decision, McpEndpoint};
use tab_core::config::AgentConfig;
use tab_core::mcp::{EngineAccess, McpServer};
use tab_core::model::*;
use tab_core::{Engine, EngineEvent, EngineHandle, Gateway, Store, TabConfig};

struct Live(Arc<EngineHandle>);
#[async_trait]
impl EngineAccess for Live {
    async fn create_task(&self, t: NewTask) -> anyhow::Result<QueueItem> {
        self.0.create_task(t)
    }
    async fn preview(&self, id: &str) -> anyhow::Result<Preview> {
        self.0.preview(id).await
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let kind = AgentKind::parse(args.get(1).map(String::as_str).unwrap_or("claude_code"));
    let prompt = args.get(2).cloned().unwrap_or_else(|| {
        "Use the tab_search tool to search for 'checkout'. Then call tab_add_found for the most relevant result with a one-line summary. Reply with one sentence.".into()
    });
    let store = Arc::new(Store::in_memory()?);
    let (events, mut rx) = tokio::sync::broadcast::channel(1024);
    let cfg = TabConfig::default();
    let engine = Arc::new(
        Engine::from_config(
            &cfg,
            store.clone(),
            Arc::new(Gateway::new(store.clone())),
            events.clone(),
        )
        .start(),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let task = engine.create_task(NewTask {
        title: "Investigate checkout errors".into(),
        created_by: "user".into(),
        ..Default::default()
    })?;

    let dir = std::env::temp_dir().join("tab-smoke");
    let hub = AgentHub::new(
        store.clone(),
        events.clone(),
        AgentConfig {
            kind: Some(kind),
            workdir: Some(dir.display().to_string()),
            ..Default::default()
        },
        dir.clone(),
    );
    std::fs::create_dir_all(&dir)?;
    let url = tab_core::mcp::serve(
        Arc::new(McpServer {
            store: store.clone(),
            hub: hub.clone(),
            engine: Arc::new(Live(engine.clone())),
            events: events.clone(),
            token: "t0k".into(),
        }),
        47950,
    )
    .await?;
    hub.set_mcp(McpEndpoint {
        url,
        token: "t0k".into(),
    });

    let session = hub.session_for_item(&task.id)?;
    hub.send(&session.id, &prompt)?;
    let mut worked = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        let ev = tokio::select! { e = rx.recv() => e?, _ = tokio::time::sleep_until(deadline) => { println!("TIMEOUT"); break } };
        match ev {
            EngineEvent::SessionMessage { message } => println!(
                "[{} {}] {}",
                message.role,
                message.kind,
                message.content.chars().take(200).collect::<String>()
            ),
            EngineEvent::ApprovalRequested { approval } => {
                println!("APPROVAL {}: auto-denying", approval.summary);
                hub.resolve_approval(&approval.id, Decision::Deny("smoke test".into()));
            }
            EngineEvent::SessionChanged { session } => {
                println!(
                    "status {:?} agent_session {:?}",
                    session.status, session.agent_session_id
                );
                if session.status == SessionStatus::Working {
                    worked = true
                }
                if worked
                    && session.agent_session_id.is_some()
                    && matches!(session.status, SessionStatus::Idle | SessionStatus::Failed)
                {
                    break;
                }
            }
            EngineEvent::ContextChanged { item_id } => println!("CONTEXT CHANGED on {item_id}"),
            _ => {}
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!(
        "context: {:?}",
        store.item_context(&task.id)?.map(|c| c
            .entries
            .iter()
            .map(|e| (e.section, e.title.clone()))
            .collect::<Vec<_>>())
    );
    hub.shutdown();
    Ok(())
}
