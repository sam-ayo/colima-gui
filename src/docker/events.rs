//! The `/events` stream. The engine pushes changes, so the app does not poll.

use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::EventsOptions;
use futures::StreamExt;

#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// A container changed. `action` is the Docker action, for example `start`.
    Container { id: String, action: String },
    /// The stream (re)connected. The store must reload the full list.
    Resync,
    /// The engine does not answer. The stream tries again with backoff.
    Disconnected,
}

impl EngineEvent {
    /// Actions that do not change the list row. They are skipped.
    fn is_noise(action: &str) -> bool {
        action.starts_with("exec_")
            || matches!(
                action,
                "attach" | "resize" | "top" | "archive-path" | "extract-to-dir" | "copy" | "export"
                    | "commit" | "detach"
            )
    }
}

const MIN_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// Reads container events until `tx` closes. If the stream ends, it connects
/// again with backoff (0.5 s to 2 s) and sends `Resync`.
pub async fn pump(docker: Docker, tx: async_channel::Sender<EngineEvent>) {
    let mut backoff = MIN_BACKOFF;
    let mut connected = true;
    loop {
        if docker.ping().await.is_err() {
            if connected {
                connected = false;
                if tx.send(EngineEvent::Disconnected).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
            if tx.is_closed() {
                return;
            }
            continue;
        }
        backoff = MIN_BACKOFF;
        connected = true;
        // The HTTP request starts only at the first poll, after `Resync`.
        // `since` makes the engine also send the events of that gap. An event
        // that comes two times is harmless, because a refresh is idempotent.
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().saturating_sub(1))
            .unwrap_or(0);
        let opts = EventsOptions {
            since: Some(since.to_string()),
            filters: Some(HashMap::from([(
                "type".to_string(),
                vec!["container".to_string()],
            )])),
            ..Default::default()
        };
        let mut stream = docker.events(Some(opts));
        if tx.send(EngineEvent::Resync).await.is_err() {
            return;
        }
        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { break };
            let action = msg.action.unwrap_or_default();
            let Some(id) = msg.actor.and_then(|a| a.id) else {
                continue;
            };
            if EngineEvent::is_noise(&action) {
                continue;
            }
            if tx.send(EngineEvent::Container { id, action }).await.is_err() {
                return;
            }
        }
        tracing::info!("event stream ended; reconnecting");
        tokio::time::sleep(MIN_BACKOFF).await;
    }
}
