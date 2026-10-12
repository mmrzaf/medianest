//! The running library.
//!
//! A [`Library`] owns an open MediaNest library for the length of a session:
//! the lock, one catalog writer, a pool of readers, background jobs,
//! thumbnail generation, folder watching and scheduled maintenance. The
//! desktop application and the command-line tool both use it.
//!
//! Methods are blocking. User interfaces call them through [`Library::run`],
//! which executes on a worker thread and returns an awaitable [`Task`].

mod edits;
mod error;
mod events;
mod export;
mod health;
mod import;
mod jobs;
mod maintenance;
mod pool;
mod readers;
mod review;
mod similar;
mod sources;
mod thumbnailer;
mod watcher;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use medianest_catalog::{Db, Writer};
use medianest_core::{AssetId, AssetUuid, MediaKind, PathBytes};
use medianest_media::{MediaToolkit, ThumbnailCache, ThumbnailSource};
use medianest_vault::{Layout, LibraryLock, Manifest, OpenedLibrary, Vault};

pub use edits::AssetDetail;
pub use error::{Error, Result};
pub use events::{Event, EventBus};
pub use export::{ExportOptions, MetadataImportReport};
pub use health::{CacheUsage, HealthReport, Issue, group_digits};
pub use import::{ImportItem, ImportItemState, ImportOptions, ImportPlan, SpaceCheck};
pub use jobs::{JobContext, JobId, JobSnapshot, JobState, Lane};
pub use maintenance::Settings;
pub use pool::Task;
pub use review::ReviewAction;
pub use similar::SimilarGroup;
pub use sources::FolderNode;

pub use medianest_catalog as catalog;
pub use medianest_core as core;
pub use medianest_media as media;
pub use medianest_vault as vault;

pub(crate) struct Inner {
    layout: Layout,
    manifest: Manifest,
    writer: Arc<Mutex<Writer>>,
    readers: Arc<readers::ReaderPool>,
    lock: Mutex<Option<LibraryLock>>,
    toolkit: MediaToolkit,
    cache: ThumbnailCache,
    events: Arc<EventBus>,
    jobs: jobs::JobManager,
    thumbnailer: thumbnailer::Thumbnailer,
    pool: pool::Pool,
    watcher: Mutex<Option<watcher::Watcher>>,
    maintenance: Mutex<Option<maintenance::Scheduler>>,
    settings: Mutex<Settings>,
    closed: AtomicBool,
}

/// Advanced options for opening a library.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    pub settings: Settings,
    /// Probing and decoding facilities; by default decoders run isolated.
    pub toolkit: Option<MediaToolkit>,
    /// Where thumbnails are cached; by default under the user's cache folder.
    pub cache_root: Option<PathBuf>,
}

/// An open library. Cheap to clone; all clones share one session.
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("root", &self.inner.layout.root())
            .finish_non_exhaustive()
    }
}

const SETTINGS_KEY: &str = "settings";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Library {
    /// Creates a new library in `root` and opens it.
    pub fn create(root: &Path, settings: Settings) -> Result<Self> {
        Self::create_with(
            root,
            OpenOptions {
                settings,
                ..OpenOptions::default()
            },
        )
    }

    pub fn create_with(root: &Path, options: OpenOptions) -> Result<Self> {
        let opened = medianest_vault::create(root)?;
        Self::start(opened, options)
    }

    /// Opens an existing library. Interrupted operations are reconciled
    /// before this returns, so nothing else can change files first.
    pub fn open(root: &Path, settings: Settings) -> Result<Self> {
        Self::open_with(
            root,
            OpenOptions {
                settings,
                ..OpenOptions::default()
            },
        )
    }

    pub fn open_with(root: &Path, options: OpenOptions) -> Result<Self> {
        let opened = medianest_vault::open(root)?;
        Self::start(opened, options)
    }

    fn start(opened: OpenedLibrary, options: OpenOptions) -> Result<Self> {
        let OpenOptions {
            settings,
            toolkit,
            cache_root,
        } = options;
        let OpenedLibrary {
            layout,
            manifest,
            mut writer,
            lock: library_lock,
        } = opened;
        writer.abandon_running_jobs()?;
        let writer = Arc::new(Mutex::new(writer));
        let toolkit = toolkit.unwrap_or_default();
        {
            let vault = Vault::new(&layout, &writer);
            let probe = |path: &Path| toolkit.probe(path);
            let report = vault.recover(&probe)?;
            if !report.is_empty() {
                tracing::info!(?report, "interrupted operations reconciled at startup");
            }
        }
        let readers = Arc::new(readers::ReaderPool::new(layout.catalog()));
        // Settings saved in the library win over the defaults passed in.
        let settings = match readers.with(|db| db.meta(SETTINGS_KEY)) {
            Ok(Some(saved)) => serde_json::from_str(&saved).unwrap_or(settings),
            _ => settings,
        };
        let events = Arc::new(EventBus::default());
        let cache = match cache_root {
            Some(root) => ThumbnailCache::with_root(root),
            None => ThumbnailCache::for_library(manifest.uuid),
        };
        let jobs = jobs::JobManager::new(Arc::clone(&events), Arc::clone(&writer));
        let thumbnailer = thumbnailer::Thumbnailer::new(
            Arc::clone(&readers),
            Arc::clone(&writer),
            cache.clone(),
            toolkit.clone(),
            Arc::clone(&events),
        );
        let library = Self {
            inner: Arc::new(Inner {
                layout,
                manifest,
                writer,
                readers,
                lock: Mutex::new(Some(library_lock)),
                toolkit,
                cache,
                events,
                jobs,
                thumbnailer,
                pool: pool::Pool::new("library", 4),
                watcher: Mutex::new(None),
                maintenance: Mutex::new(None),
                settings: Mutex::new(settings),
                closed: AtomicBool::new(false),
            }),
        };
        library.start_watching();
        *lock(&library.inner.maintenance) = Some(maintenance::Scheduler::start(&library));
        tracing::info!(root = %library.root().display(), "library open");
        Ok(library)
    }

    /// Stops background work and releases the library. Further calls fail.
    pub fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(scheduler) = lock(&self.inner.maintenance).take() {
            scheduler.stop();
        }
        lock(&self.inner.watcher).take();
        self.inner.jobs.shutdown();
        self.inner.thumbnailer.shutdown();
        self.inner.pool.shutdown();
        if let Err(error) = lock(&self.inner.writer).checkpoint() {
            tracing::warn!(%error, "could not checkpoint the catalog");
        }
        self.inner.readers.clear();
        self.inner.events.close();
        lock(&self.inner.lock).take();
        tracing::info!(root = %self.root().display(), "library closed");
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    fn check_open(&self) -> Result<()> {
        if self.is_closed() {
            return Err(Error::Closed);
        }
        Ok(())
    }

    /// The library folder.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.inner.layout.root()
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.inner.layout
    }

    #[must_use]
    pub fn uuid(&self) -> medianest_core::LibraryUuid {
        self.inner.manifest.uuid
    }

    #[must_use]
    pub fn toolkit(&self) -> &MediaToolkit {
        &self.inner.toolkit
    }

    #[must_use]
    pub fn thumbnail_cache(&self) -> &ThumbnailCache {
        &self.inner.cache
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        lock(&self.inner.settings).clone()
    }

    /// Applies new settings and saves them in the library. Folder watching
    /// starts or stops accordingly.
    pub fn set_settings(&self, settings: Settings) {
        let watch_changed = lock(&self.inner.settings).watch_folders != settings.watch_folders;
        if let Ok(json) = serde_json::to_string(&settings) {
            if let Err(error) = self.write(|writer| writer.set_meta(SETTINGS_KEY, &json)) {
                tracing::warn!(%error, "could not save the library settings");
            }
        }
        *lock(&self.inner.settings) = settings;
        if watch_changed {
            self.start_watching();
        }
    }

    /// A stream of change notifications.
    #[must_use]
    pub fn subscribe(&self) -> async_channel::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub(crate) fn emit(&self, event: Event) {
        self.inner.events.emit(event);
    }

    /// Runs blocking work on a library worker thread.
    pub fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Library) -> T + Send + 'static,
    ) -> Task<T> {
        let library = self.clone();
        self.inner.pool.spawn(move || work(&library))
    }

    /// Runs a read-only query on a pooled connection.
    pub fn read<R>(&self, query: impl FnOnce(&Db) -> medianest_catalog::Result<R>) -> Result<R> {
        self.check_open()?;
        self.inner.readers.with(query)
    }

    pub(crate) fn write<R>(
        &self,
        change: impl FnOnce(&mut Writer) -> medianest_catalog::Result<R>,
    ) -> Result<R> {
        self.check_open()?;
        let mut writer = lock(&self.inner.writer);
        Ok(change(&mut writer)?)
    }

    pub(crate) fn vault(&self) -> Vault<'_> {
        Vault::new(&self.inner.layout, &self.inner.writer)
    }

    pub(crate) fn probe(
        &self,
        path: &Path,
    ) -> std::result::Result<medianest_core::AssetMetadata, medianest_media::ProbeError> {
        self.inner.toolkit.probe(path)
    }

    // ----- jobs ---------------------------------------------------------------

    #[must_use]
    pub fn jobs(&self) -> Vec<JobSnapshot> {
        self.inner.jobs.snapshots()
    }

    #[must_use]
    pub fn job(&self, id: JobId) -> Option<JobSnapshot> {
        self.inner.jobs.snapshot(id)
    }

    /// Whether any job is queued or running; worth asking before quitting.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.inner.jobs.busy()
    }

    pub fn cancel_job(&self, id: JobId) {
        self.inner.jobs.cancel(id);
    }

    pub fn pause_job(&self, id: JobId, paused: bool) {
        self.inner.jobs.set_paused(id, paused);
    }

    pub(crate) fn submit(
        &self,
        kind: &'static str,
        title: impl Into<String>,
        lane: Lane,
        run: impl FnOnce(&Library, &JobContext) -> Result<String> + Send + 'static,
    ) -> JobId {
        let library = self.clone();
        self.inner
            .jobs
            .submit(kind, title, lane, move |context| run(&library, context))
    }

    /// Blocks until a job finishes and returns its final state.
    pub fn wait_for_job(&self, id: JobId) -> Option<JobSnapshot> {
        let events = self.subscribe();
        loop {
            let snapshot = self.job(id)?;
            if snapshot.state.is_finished() {
                return Some(snapshot);
            }
            match events.recv_blocking() {
                Ok(_) => {}
                Err(_) => return self.job(id),
            }
        }
    }

    // ----- thumbnails, previews and originals ---------------------------------

    /// Where an asset's grid thumbnail is cached. It may not exist yet.
    #[must_use]
    pub fn thumbnail_path(&self, uuid: AssetUuid) -> PathBuf {
        self.inner.cache.thumbnail_path(uuid)
    }

    /// Asks for thumbnails of these assets before any others.
    pub fn request_thumbnails(&self, ids: &[AssetId]) {
        self.inner.thumbnailer.request(ids);
    }

    pub(crate) fn poke_thumbnails(&self) {
        self.inner.thumbnailer.poke();
    }

    /// A large, display-ready rendition of a photo, created on first use.
    pub fn preview(&self, id: AssetId) -> Result<PathBuf> {
        let work = self
            .read(|db| db.thumbnail_work(id))?
            .ok_or_else(|| Error::Invalid("this photo's file is not available".into()))?;
        let duration = self.read(|db| db.asset(id))?.duration_ms;
        let source = ThumbnailSource {
            path: &work.path,
            format: work.format,
            orientation: work.orientation,
            motion_video_length: work.motion_video_offset,
            duration_ms: duration,
        };
        Ok(self.inner.cache.preview(
            work.uuid,
            &source,
            &self.inner.toolkit.decoder,
            &self.inner.toolkit.video,
        )?)
    }

    /// The hover filmstrip of a video: [`medianest_media::FILMSTRIP_FRAMES`]
    /// frames side by side in one image, created on first use.
    pub fn filmstrip(&self, id: AssetId) -> Result<PathBuf> {
        let work = self
            .read(|db| db.thumbnail_work(id))?
            .ok_or_else(|| Error::Invalid("this video's file is not available".into()))?;
        if work.format.kind() != MediaKind::Video {
            return Err(Error::Invalid("only videos have a filmstrip".into()));
        }
        let duration = self.read(|db| db.asset(id))?.duration_ms;
        let source = ThumbnailSource {
            path: &work.path,
            format: work.format,
            orientation: work.orientation,
            motion_video_length: work.motion_video_offset,
            duration_ms: duration,
        };
        Ok(self
            .inner
            .cache
            .filmstrip(work.uuid, &source, &self.inner.toolkit.video)?)
    }

    /// A photo at full resolution with its orientation applied, as RGBA
    /// pixels: `(width, height, pixels)`. Used when zooming past the preview.
    pub fn full_image(&self, id: AssetId) -> Result<(u32, u32, Vec<u8>)> {
        let work = self
            .read(|db| db.thumbnail_work(id))?
            .ok_or_else(|| Error::Invalid("this photo's file is not available".into()))?;
        let image = self.inner.toolkit.decoder.decode(
            &work.path,
            work.format,
            work.orientation,
            medianest_media::Size::Full,
        )?;
        let (width, height) = image.dimensions();
        Ok((width, height, image.into_raw()))
    }

    /// The best available file of an asset: a present managed copy first.
    pub fn original_path(&self, id: AssetId) -> Result<PathBuf> {
        self.read(|db| db.best_file(id))?
            .map(|location| location.path())
            .ok_or_else(|| {
                Error::Invalid("the file is missing; reconnect its drive or folder".into())
            })
    }

    /// A playable video for a Live Photo or motion photo, if it has one.
    pub fn motion_video(&self, id: AssetId) -> Result<Option<PathBuf>> {
        let asset = self.read(|db| db.asset(id))?;
        if let Some(video) = asset.live_video {
            return self.original_path(video).map(Some);
        }
        let Some(length) = asset.motion_video_offset else {
            return Ok(None);
        };
        let source = self.original_path(id)?;
        let target = self
            .inner
            .cache
            .root()
            .join("motion")
            .join(format!("{}.mp4", asset.uuid.simple()));
        if !target.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(Error::io(parent))?;
            }
            medianest_media::VideoTools::extract_tail(&source, length, &target)
                .map_err(Error::io(&target))?;
        }
        Ok(Some(target))
    }

    /// Display path of a file location, for the info panel.
    #[must_use]
    pub fn display_path(path: &PathBytes) -> String {
        path.display()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.jobs.shutdown();
            self.thumbnailer.shutdown();
            self.pool.shutdown();
            let _ = lock(&self.writer).checkpoint();
        }
    }
}
