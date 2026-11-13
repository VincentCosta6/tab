//! The tokio runtime tab-core runs on, beside GPUI's own executor.
//!
//! tab-core is async on tokio (sockets, processes, timers). GPUI runs views
//! on its own main-thread executor. A view starts backend work with
//! [`spawn`], which runs it on tokio and hands back a future GPUI can
//! await.

use std::future::Future;
use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn init() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("tab-core")
            .build()
            .expect("tokio runtime")
    })
}

pub fn handle() -> &'static tokio::runtime::Runtime {
    RUNTIME.get().expect("runtime::init first")
}

/// Runs `f` on tokio. Awaiting the result (on any executor) gives its
/// output; a panic there comes back as an error string.
pub fn spawn<F>(f: F) -> impl Future<Output = Result<F::Output, String>> + Send + 'static
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = handle().spawn(f);
    async move { task.await.map_err(|e| e.to_string()) }
}
