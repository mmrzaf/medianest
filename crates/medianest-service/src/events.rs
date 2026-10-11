//! Change notifications for the user interface.

use std::sync::Mutex;

use medianest_core::{AssetId, AssetUuid};

use crate::jobs::JobSnapshot;

/// Something in the library changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Metadata of these assets changed; their position in views may not have.
    AssetsChanged(Vec<AssetId>),
    /// Assets were added, removed, hidden, trashed, merged or re-dated:
    /// views should reload.
    ContentChanged,
    /// A thumbnail was written to the cache.
    ThumbnailReady(AssetId, AssetUuid),
    /// A thumbnail could not be made.
    ThumbnailFailed(AssetId),
    SourcesChanged,
    TagsChanged,
    AlbumsChanged,
    ReviewChanged,
    /// A job started, progressed, paused or finished.
    Job(JobSnapshot),
}

/// Fans events out to every subscriber.
#[derive(Debug, Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<async_channel::Sender<Event>>>,
}

impl EventBus {
    /// A new stream of events. Dropping the receiver unsubscribes.
    pub fn subscribe(&self) -> async_channel::Receiver<Event> {
        let (sender, receiver) = async_channel::unbounded();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }

    pub fn emit(&self, event: Event) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.retain(|sender| sender.try_send(event.clone()).is_ok());
        }
    }

    pub(crate) fn close(&self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            for sender in subscribers.drain(..) {
                sender.close();
            }
        }
    }
}
