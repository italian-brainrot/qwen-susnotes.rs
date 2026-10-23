//! Change notifications: the vault is the single source of truth, and the
//! desktop app, the local API and the CLI all observe it.

use std::sync::{Arc, Mutex, mpsc};

use serde::{Deserialize, Serialize};

/// What happened inside a vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// A note appeared.
    NoteCreated {
        /// Display name of the note.
        name: String,
    },
    /// A note's content was written.
    NoteUpdated {
        /// Display name of the note.
        name: String,
    },
    /// A note was deleted.
    NoteDeleted {
        /// Display name of the note.
        name: String,
    },
    /// A note was renamed, optionally with its links rewritten.
    NoteRenamed {
        /// Name before the rename.
        old: String,
        /// Name after the rename.
        new: String,
        /// How many other notes had their links rewritten.
        links_rewritten: usize,
    },
    /// An asset was added.
    AssetAdded {
        /// Vault relative path.
        path: String,
    },
    /// An asset was removed.
    AssetDeleted {
        /// Vault relative path.
        path: String,
    },
    /// The vault was opened and unlocked.
    Unlocked,
    /// The vault was locked again (password forgotten from memory).
    Locked,
    /// Settings were written, so any window should re-read them.
    SettingsUpdated,
    /// The listener missed events; it should reload what it is showing.
    Resync,
}

/// Queue depth per subscriber. A consumer that falls this far behind gets a
/// [`Change::Resync`] instead of more events, which tells it to reload rather
/// than to keep trusting a partial history.
const QUEUE_DEPTH: usize = 128;

/// A tiny broadcast bus.
///
/// Subscribers get their own queue, so a slow consumer can never block a write,
/// and a dropped consumer simply stops receiving changes.
#[derive(Debug, Clone, Default)]
pub struct ChangeBus {
    subscribers: Arc<Mutex<Vec<mpsc::SyncSender<Change>>>>,
}

impl ChangeBus {
    /// A new, empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener. The receiver yields changes until it is dropped.
    pub fn subscribe(&self) -> mpsc::Receiver<Change> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.push(sender);
        receiver
    }

    /// Publish a change to every current subscriber.
    pub fn emit(&self, change: Change) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        subscribers.retain(|subscriber| match subscriber.try_send(change.clone()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                // The consumer is behind; tell it to reload instead of growing
                // the queue without bound.
                subscriber.try_send(Change::Resync).is_ok()
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    /// How many listeners are attached, for logging.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribers_receive_changes() {
        let bus = ChangeBus::new();
        let first = bus.subscribe();
        let second = bus.subscribe();
        bus.emit(Change::NoteCreated {
            name: "Inbox".into(),
        });

        assert_eq!(
            first.recv().unwrap(),
            Change::NoteCreated {
                name: "Inbox".into()
            }
        );
        assert_eq!(
            second.recv().unwrap(),
            Change::NoteCreated {
                name: "Inbox".into()
            }
        );
    }

    #[test]
    fn dropped_subscribers_are_removed() {
        let bus = ChangeBus::new();
        let receiver = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        drop(receiver);
        bus.emit(Change::Locked);
        assert_eq!(bus.subscriber_count(), 0);
    }
}
