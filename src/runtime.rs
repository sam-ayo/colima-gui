//! Bridge between the Tokio runtime (needed by bollard) and the GPUI executor.

use std::future::Future;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use gpui::{App, Global};

/// Owns the Tokio runtime. GPUI keeps it as a global for the life of the app.
#[derive(Clone)]
pub struct Backend {
    rt: Arc<tokio::runtime::Runtime>,
}

impl Global for Backend {}

impl Backend {
    /// Starts the runtime. Call this before GPUI starts, so that the first
    /// engine connection can run while GPUI initializes.
    pub fn new() -> Backend {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("docker-io")
            .enable_all()
            .build()
            .expect("failed to start the Tokio runtime");
        Backend { rt: Arc::new(rt) }
    }

    pub fn global(cx: &App) -> Backend {
        cx.global::<Backend>().clone()
    }

    /// Runs `fut` on Tokio. Await the receiver from a GPUI task.
    pub fn spawn<F, T>(&self, fut: F) -> futures::channel::oneshot::Receiver<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx
    }

    /// Drives `stream` on Tokio and forwards each item to a channel.
    /// The Tokio task stops when the receiver is dropped.
    pub fn stream<S, T>(&self, stream: S) -> async_channel::Receiver<T>
    where
        S: Stream<Item = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = async_channel::bounded(4096);
        self.rt.spawn(async move {
            futures::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        rx
    }
}

/// Waits for at least one item, then takes all items that are ready now.
/// Returns `None` when the channel is closed and empty.
pub async fn recv_batch<T>(rx: &async_channel::Receiver<T>, max: usize) -> Option<Vec<T>> {
    let first = rx.recv().await.ok()?;
    let mut batch = Vec::with_capacity(rx.len().min(max) + 1);
    batch.push(first);
    while batch.len() < max {
        match rx.try_recv() {
            Ok(item) => batch.push(item),
            Err(_) => break,
        }
    }
    Some(batch)
}
