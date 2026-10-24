//! The change stream: the core change bus, rendered as server-sent events.
//!
//! A client that wants to stay current subscribes once and reacts to events
//! instead of polling the note list.
//!
//! Two details matter:
//!
//! - The subscription is opened *before* the `snapshot` event is emitted, so a
//!   change that lands in between is delivered rather than lost in the gap.
//! - Events missed while disconnected are gone for good. On reconnect, do one
//!   full refresh; the `snapshot` event marks the point where it is complete.
//!   The bus also has a bounded queue per subscriber, and sends
//!   [`susnotes_core::Change::Resync`] rather than dropping events silently if a
//!   client falls too far behind.

use std::time::Duration;

use axum::{
    extract::State,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures_util::stream::{self, StreamExt};
use susnotes_core::{Change, api_types::EventEnvelope};

use crate::AppState;

/// How often a stream checks the change bus for something new.
///
/// The core bus is a plain synchronous subscription so the desktop app can use
/// it without an async runtime. Bridging it with a short poll keeps that boundary
/// simple and keeps the core free of tokio; one wake-up every 100 ms per open
/// stream costs nothing worth measuring.
const POLL: Duration = Duration::from_millis(100);

pub async fn stream(State(state): State<AppState>) -> impl IntoResponse {
    let receiver = state.vault.changes().subscribe();

    let snapshot = EventEnvelope {
        id: 0,
        change: Change::Resync,
    };

    let live = stream::unfold((receiver, 0u64), |(receiver, seq)| async move {
        loop {
            match receiver.try_recv() {
                Ok(change) => {
                    let next = seq + 1;
                    return Some((EventEnvelope { id: next, change }, (receiver, next)));
                }
                // The bus sender lives as long as the vault, so this only
                // happens once the vault itself is gone.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
                Err(std::sync::mpsc::TryRecvError::Empty) => tokio::time::sleep(POLL).await,
            }
        }
    });

    let encoded = stream::once(async move { snapshot })
        .chain(live)
        .map(|event| {
            let kind = event.change.kind();
            tracing::trace!(kind, id = event.id, "emitting an API event");
            let data = serde_json::to_string(&event).unwrap_or_else(|err| {
                tracing::error!(error = %err, "an event could not be encoded");
                "{}".to_owned()
            });
            Ok::<_, std::convert::Infallible>(Event::default().event(kind).data(data))
        });

    // Pings keep intermediaries and half-dead sockets honest; the stream itself
    // has no timeout, because a vault can be quiet for days.
    Sse::new(encoded).keep_alive(KeepAlive::default().text("ping"))
}
