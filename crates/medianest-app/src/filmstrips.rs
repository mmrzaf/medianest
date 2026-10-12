//! Hover filmstrips: a few frames of a video, shown while the pointer moves
//! across its tile in the grid.
//!
//! The service makes and caches the strip. At most one is fetched at a time,
//! and only for the tile the pointer last settled on, so sweeping across the
//! grid never queues work behind the library's other tasks.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::{gdk, glib};
use medianest_service::Library;
use medianest_service::core::{AssetId, AssetUuid};
use medianest_service::media::FILMSTRIP_FRAMES;

use crate::textures::{Pixels, decode_jpeg};

/// Strips kept as textures. Each is eight uncompressed frames, up to about
/// 8 MB; the rest come back from the cache on disk in a few milliseconds.
const KEPT: usize = 6;
/// How long a video that failed is left alone before it is tried again, so
/// that a drive that was unplugged or a busy moment does not last all session.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The frames of one video, in playback order.
pub type Frames = Rc<Vec<gdk::Texture>>;

/// Filmstrip textures of one library session.
pub struct Filmstrips {
    library: Library,
    /// Least recently used first.
    strips: RefCell<VecDeque<(AssetUuid, Frames)>>,
    wanted: Cell<Option<(AssetId, AssetUuid)>>,
    fetching: Cell<Option<AssetUuid>>,
    failed: RefCell<HashMap<AssetUuid, Instant>>,
    /// The strip being fetched was invalidated; its result is out of date.
    discard: Cell<bool>,
    listeners: RefCell<Vec<Box<dyn Fn() -> bool>>>,
}

impl std::fmt::Debug for Filmstrips {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filmstrips")
            .field("cached", &self.strips.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Filmstrips {
    pub fn new(library: Library) -> Rc<Self> {
        Rc::new(Self {
            library,
            strips: RefCell::default(),
            wanted: Cell::new(None),
            fetching: Cell::new(None),
            failed: RefCell::default(),
            discard: Cell::new(false),
            listeners: RefCell::default(),
        })
    }

    /// Calls `listener` whenever a strip becomes available, until it
    /// returns `false`.
    pub fn connect_loaded(&self, listener: impl Fn() -> bool + 'static) {
        self.listeners.borrow_mut().push(Box::new(listener));
    }

    fn notify(&self) {
        let mut listeners = self.listeners.take();
        listeners.retain(|listener| listener());
        self.listeners.borrow_mut().append(&mut listeners);
    }

    /// The frames of a video, if its strip is loaded.
    pub fn get(&self, uuid: AssetUuid) -> Option<Frames> {
        let mut strips = self.strips.borrow_mut();
        let position = strips.iter().position(|(known, _)| *known == uuid)?;
        let entry = strips.remove(position)?;
        let frames = Rc::clone(&entry.1);
        strips.push_back(entry);
        Some(frames)
    }

    /// Asks for a video's strip. Replaces any earlier request that has not
    /// started, so only the tile the pointer is on is ever fetched.
    pub fn request(self: &Rc<Self>, id: AssetId, uuid: AssetUuid) {
        self.wanted.set(Some((id, uuid)));
        self.pump();
    }

    /// The pointer left the tile: drop a request that has not started.
    pub fn cancel(&self) {
        self.wanted.set(None);
    }

    /// The video's cached strip was removed or rebuilt.
    pub fn forget(&self, uuid: AssetUuid) {
        self.strips.borrow_mut().retain(|(known, _)| *known != uuid);
        self.failed.borrow_mut().remove(&uuid);
        if self.fetching.get() == Some(uuid) {
            self.discard.set(true);
        }
    }

    fn pump(self: &Rc<Self>) {
        if self.fetching.get().is_some() {
            return;
        }
        let Some((id, uuid)) = self.wanted.take() else {
            return;
        };
        let recently_failed = self
            .failed
            .borrow()
            .get(&uuid)
            .is_some_and(|when| when.elapsed() < RETRY_AFTER);
        if recently_failed || self.strips.borrow().iter().any(|(known, _)| *known == uuid) {
            return;
        }
        self.fetching.set(Some(uuid));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let cells = this
                .library
                .run(move |library| {
                    let path = library.filmstrip(id).map_err(|error| error.to_string())?;
                    split(&path)
                })
                .await;
            this.fetching.set(None);
            match cells {
                _ if this.discard.replace(false) => {}
                Some(Ok(cells)) => {
                    let frames = cells.into_iter().map(Pixels::texture).collect();
                    this.insert(uuid, Rc::new(frames));
                    this.notify();
                }
                Some(Err(error)) => {
                    tracing::info!(asset = %id, %error, "no hover filmstrip");
                    this.failed.borrow_mut().insert(uuid, Instant::now());
                }
                None => {}
            }
            this.pump();
        });
    }

    fn insert(&self, uuid: AssetUuid, frames: Frames) {
        let mut strips = self.strips.borrow_mut();
        strips.retain(|(known, _)| *known != uuid);
        strips.push_back((uuid, frames));
        while strips.len() > KEPT {
            strips.pop_front();
        }
    }
}

/// Cuts a filmstrip image into its frames.
fn split(path: &Path) -> Result<Vec<Pixels>, String> {
    let strip = decode_jpeg(path).map_err(|error| error.to_string())?;
    let count = FILMSTRIP_FRAMES as usize;
    let (strip_width, height) = (strip.width as usize, strip.height as usize);
    let width = strip_width / count;
    if width == 0 || height == 0 || strip_width % count != 0 {
        return Err("the filmstrip has an unexpected size".into());
    }
    let channels = if strip.alpha { 4 } else { 3 };
    Ok((0..count)
        .map(|frame| {
            let mut data = Vec::with_capacity(width * height * channels);
            for row in 0..height {
                let start = (row * strip_width + frame * width) * channels;
                data.extend_from_slice(&strip.data[start..start + width * channels]);
            }
            Pixels {
                width: u32::try_from(width).unwrap_or(u32::MAX),
                height: strip.height,
                data,
                alpha: strip.alpha,
            }
        })
        .collect())
}
