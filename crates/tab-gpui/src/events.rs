//! Engine events, delivered to views on GPUI's thread.

use std::sync::Arc;

use futures::StreamExt;
use gpui::{App, AppContext, Entity, EventEmitter, Global};
use tab_core::EngineEvent;
use tokio::sync::broadcast;

/// Views subscribe to this entity for [`EngineEvent`]s.
pub struct Events;

impl EventEmitter<Arc<EngineEvent>> for Events {}

struct GlobalEvents(Entity<Events>);
impl Global for GlobalEvents {}

/// The events entity.
pub fn events(cx: &App) -> Entity<Events> {
    cx.global::<GlobalEvents>().0.clone()
}

/// Forwards the engine's broadcast into GPUI.
pub fn start(tx: &broadcast::Sender<EngineEvent>, cx: &mut App) {
    let entity = cx.new(|_| Events);
    cx.set_global(GlobalEvents(entity.clone()));
    let (fwd, mut rx) = futures::channel::mpsc::unbounded::<Arc<EngineEvent>>();
    let mut sub = tx.subscribe();
    crate::runtime::handle().spawn(async move {
        loop {
            match sub.recv().await {
                Ok(ev) => {
                    if fwd.unbounded_send(Arc::new(ev)).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "UI fell behind on events")
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    cx.spawn(async move |cx| {
        while let Some(ev) = rx.next().await {
            let _ = entity.update(cx, |_, cx| cx.emit(ev));
        }
    })
    .detach();
}
