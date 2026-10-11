//! Generates thumbnails: what is on screen first, then everything else.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use medianest_catalog::{ThumbnailWork, Writer};
use medianest_core::{AssetId, ThumbState};
use medianest_media::{MediaToolkit, ThumbnailCache, ThumbnailSource};

use crate::events::{Event, EventBus};
use crate::readers::ReaderPool;

#[derive(Default)]
struct Queue {
    /// Requested by the interface, most urgent first.
    visible: VecDeque<AssetId>,
    /// Pending work from the catalog, filled in batches.
    background: VecDeque<ThumbnailWork>,
    in_flight: HashSet<AssetId>,
    /// The catalog had nothing left to do at the last look.
    exhausted: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    stopping: AtomicBool,
    readers: Arc<ReaderPool>,
    writer: Arc<Mutex<Writer>>,
    cache: ThumbnailCache,
    toolkit: MediaToolkit,
    events: Arc<EventBus>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Background thumbnail generation.
pub struct Thumbnailer {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for Thumbnailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnailer").finish_non_exhaustive()
    }
}

enum Work {
    Visible(AssetId),
    Background(ThumbnailWork),
}

impl Thumbnailer {
    pub(crate) fn new(
        readers: Arc<ReaderPool>,
        writer: Arc<Mutex<Writer>>,
        cache: ThumbnailCache,
        toolkit: MediaToolkit,
        events: Arc<EventBus>,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            stopping: AtomicBool::new(false),
            readers,
            writer,
            cache,
            toolkit,
            events,
        });
        let workers = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 6));
        let threads = (0..workers)
            .filter_map(|index| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("thumbnails-{index}"))
                    .spawn(move || worker(&shared))
                    .ok()
            })
            .collect();
        Self {
            shared,
            threads: Mutex::new(threads),
        }
    }

    /// Makes these assets the most urgent, replacing the previous request.
    pub fn request(&self, ids: &[AssetId]) {
        let mut queue = lock(&self.shared.queue);
        queue.visible = ids
            .iter()
            .copied()
            .filter(|id| !queue.in_flight.contains(id))
            .collect();
        drop(queue);
        self.shared.ready.notify_all();
    }

    /// New assets may need thumbnails: look in the catalog again.
    pub fn poke(&self) {
        lock(&self.shared.queue).exhausted = false;
        self.shared.ready.notify_all();
    }

    pub(crate) fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.ready.notify_all();
        let threads: Vec<_> = lock(&self.threads).drain(..).collect();
        for thread in threads {
            let _ = thread.join();
        }
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn next_work(shared: &Shared) -> Option<Work> {
    let mut queue = lock(&shared.queue);
    loop {
        if shared.stopping.load(Ordering::SeqCst) {
            return None;
        }
        while let Some(id) = queue.visible.pop_front() {
            if queue.in_flight.insert(id) {
                return Some(Work::Visible(id));
            }
        }
        {
            while let Some(work) = queue.background.pop_front() {
                if queue.in_flight.insert(work.id) {
                    return Some(Work::Background(work));
                }
            }
            if !queue.exhausted {
                drop(queue);
                let batch = shared
                    .readers
                    .with(|db| db.thumbnails_pending(96))
                    .unwrap_or_default();
                queue = lock(&shared.queue);
                if batch.is_empty() {
                    queue.exhausted = true;
                } else {
                    let in_flight = queue.in_flight.clone();
                    queue.background.extend(
                        batch
                            .into_iter()
                            .filter(|work| !in_flight.contains(&work.id)),
                    );
                    if queue.background.is_empty() {
                        // Everything pending is already being worked on.
                        queue.exhausted = true;
                    }
                }
                continue;
            }
        }
        queue = shared
            .ready
            .wait_timeout(queue, Duration::from_secs(60))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn worker(shared: &Shared) {
    while let Some(work) = next_work(shared) {
        let (id, work) = match work {
            Work::Background(work) => (work.id, Some(work)),
            Work::Visible(id) => (
                id,
                shared
                    .readers
                    .with(|db| db.thumbnail_work(id))
                    .ok()
                    .flatten(),
            ),
        };
        if let Some(work) = work {
            generate(shared, &work);
        }
        lock(&shared.queue).in_flight.remove(&id);
    }
}

fn generate(shared: &Shared, work: &ThumbnailWork) {
    let duration = shared
        .readers
        .with(|db| db.asset(work.id).map(|asset| asset.duration_ms))
        .ok()
        .flatten();
    let source = ThumbnailSource {
        path: &work.path,
        format: work.format,
        orientation: work.orientation,
        motion_video_length: work.motion_video_offset,
        duration_ms: duration,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shared.cache.generate(
            work.uuid,
            &source,
            &shared.toolkit.decoder,
            &shared.toolkit.video,
        )
    }));
    match result {
        Ok(Ok(facts)) => {
            let saved = lock(&shared.writer).set_thumbnail(
                work.id,
                ThumbState::Ready,
                Some(facts.color),
                Some(facts.phash),
            );
            if let Err(error) = saved {
                tracing::warn!(asset = %work.id, %error, "could not record thumbnail");
            }
            shared
                .events
                .emit(Event::ThumbnailReady(work.id, work.uuid));
        }
        Ok(Err(error)) => {
            tracing::info!(asset = %work.id, path = %work.path.display(), %error, "no thumbnail");
            let _ = lock(&shared.writer).set_thumbnail(work.id, ThumbState::Failed, None, None);
            shared.events.emit(Event::ThumbnailFailed(work.id));
        }
        Err(_) => {
            tracing::error!(asset = %work.id, path = %work.path.display(), "thumbnail generation panicked");
            let _ = lock(&shared.writer).set_thumbnail(work.id, ThumbState::Failed, None, None);
            shared.events.emit(Event::ThumbnailFailed(work.id));
        }
    }
}
