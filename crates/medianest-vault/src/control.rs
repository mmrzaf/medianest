//! Cancellation, pausing and progress for long-running work.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A step of progress reported by vault operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Work on an item started; `label` is shown to the user.
    Item { label: String },
    /// Bytes were read or written.
    Bytes(u64),
    /// An item finished.
    Done,
}

type Sink = Arc<dyn Fn(Progress) + Send + Sync>;

/// Shared handle to stop, pause or observe an operation.
#[derive(Clone, Default)]
pub struct Control {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    sink: Option<Sink>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control")
            .field("cancelled", &self.is_cancelled())
            .field("paused", &self.is_paused())
            .finish_non_exhaustive()
    }
}

impl Control {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same cancellation and pause state, reporting progress to `sink`.
    #[must_use]
    pub fn with_progress(mut self, sink: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.sink = Some(Arc::new(sink));
        self
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Waits while paused; fails with [`crate::VaultError::Cancelled`] once cancelled.
    pub fn checkpoint(&self) -> crate::Result<()> {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.is_cancelled() {
            return Err(crate::VaultError::Cancelled);
        }
        Ok(())
    }

    pub fn report(&self, progress: Progress) {
        if let Some(sink) = &self.sink {
            sink(progress);
        }
    }
}
