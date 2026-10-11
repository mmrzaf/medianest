//! Loading thumbnails and previews into GPU textures.
//!
//! Decoding happens on worker threads; textures are created and cached on
//! the main thread. The queue holds only what is currently wanted, so fast
//! scrolling never builds a backlog.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use gtk::{gdk, glib};
use medianest_service::Library;
use medianest_service::core::{AssetId, AssetUuid};

const WORKERS: usize = 3;
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Decoded pixels ready to become a texture.
#[derive(Debug)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Packed RGB, or RGBA when `alpha` is set.
    pub data: Vec<u8>,
    pub alpha: bool,
}

impl Pixels {
    #[must_use]
    pub fn texture(self) -> gdk::Texture {
        let (format, channels) = if self.alpha {
            (gdk::MemoryFormat::R8g8b8a8, 4)
        } else {
            (gdk::MemoryFormat::R8g8b8, 3)
        };
        let stride = self.width as usize * channels;
        gdk::MemoryTexture::new(
            i32::try_from(self.width).unwrap_or(i32::MAX),
            i32::try_from(self.height).unwrap_or(i32::MAX),
            format,
            &glib::Bytes::from_owned(self.data),
            stride,
        )
        .upcast()
    }
}

use gtk::prelude::*;

/// Decodes a JPEG written by the thumbnail cache.
pub fn decode_jpeg(path: &Path) -> std::io::Result<Pixels> {
    let file = std::fs::File::open(path)?;
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
    let data = decoder.decode().map_err(std::io::Error::other)?;
    let info = decoder
        .info()
        .ok_or_else(|| std::io::Error::other("no image information"))?;
    let data = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => data,
        jpeg_decoder::PixelFormat::L8 => data.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::L16 => data
            .chunks_exact(2)
            .flat_map(|v| [v[0], v[0], v[0]])
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => data
            .chunks_exact(4)
            .flat_map(|p| {
                let k = 255 - u16::from(p[3]);
                let channel = |c: u8| u8::try_from((255 - u16::from(c)) * k / 255).unwrap_or(0);
                [channel(p[0]), channel(p[1]), channel(p[2])]
            })
            .collect(),
    };
    Ok(Pixels {
        width: u32::from(info.width),
        height: u32::from(info.height),
        data,
        alpha: false,
    })
}

#[derive(Default)]
struct Queue {
    wanted: VecDeque<(AssetUuid, PathBuf)>,
    /// Taken by a worker and not yet received on the main thread.
    taken: HashSet<AssetUuid>,
    stop: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

enum Loaded {
    Ready(AssetUuid, Pixels),
    Missing(AssetUuid),
}

struct Entry {
    texture: gdk::Texture,
    bytes: usize,
    used: u64,
}

/// Thumbnail textures of one library session.
pub struct ThumbnailLoader {
    library: Library,
    shared: Arc<Shared>,
    entries: RefCell<HashMap<AssetUuid, Entry>>,
    total_bytes: Cell<usize>,
    clock: Cell<u64>,
    /// Thumbnails that do not exist yet, by asset.
    missing: RefCell<HashMap<AssetUuid, AssetId>>,
    failed: RefCell<HashSet<AssetUuid>>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
}

impl std::fmt::Debug for ThumbnailLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailLoader")
            .field("cached", &self.entries.borrow().len())
            .finish_non_exhaustive()
    }
}

impl ThumbnailLoader {
    pub fn new(library: Library) -> Rc<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let (sender, receiver) = async_channel::unbounded::<Loaded>();
        for index in 0..WORKERS {
            let shared = Arc::clone(&shared);
            let sender = sender.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("thumbnail-loader-{index}"))
                .spawn(move || worker(&shared, &sender));
            if let Err(error) = spawned {
                tracing::error!(%error, "could not start a thumbnail loader");
            }
        }
        let loader = Rc::new(Self {
            library,
            shared,
            entries: RefCell::default(),
            total_bytes: Cell::new(0),
            clock: Cell::new(0),
            missing: RefCell::default(),
            failed: RefCell::default(),
            listeners: RefCell::default(),
        });
        let weak = Rc::downgrade(&loader);
        glib::spawn_future_local(async move {
            while let Ok(loaded) = receiver.recv().await {
                let Some(loader) = weak.upgrade() else {
                    break;
                };
                loader.receive(loaded);
                // Drain whatever else is ready before redrawing once.
                while let Ok(loaded) = receiver.try_recv() {
                    loader.receive(loaded);
                }
                loader.notify();
            }
        });
        loader
    }

    /// Calls `listener` whenever new textures are available.
    pub fn connect_loaded(&self, listener: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(listener));
    }

    fn notify(&self) {
        for listener in self.listeners.borrow().iter() {
            listener();
        }
    }

    fn receive(&self, loaded: Loaded) {
        let uuid = match &loaded {
            Loaded::Ready(uuid, _) | Loaded::Missing(uuid) => *uuid,
        };
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .taken
            .remove(&uuid);
        match loaded {
            Loaded::Ready(uuid, pixels) => {
                let bytes = pixels.data.len();
                let texture = pixels.texture();
                self.insert(uuid, texture, bytes);
            }
            Loaded::Missing(uuid) => {
                self.missing.borrow_mut().entry(uuid).or_insert(AssetId(0));
            }
        }
    }

    fn insert(&self, uuid: AssetUuid, texture: gdk::Texture, bytes: usize) {
        let used = self.tick();
        let mut entries = self.entries.borrow_mut();
        if let Some(old) = entries.insert(
            uuid,
            Entry {
                texture,
                bytes,
                used,
            },
        ) {
            self.total_bytes.set(self.total_bytes.get() - old.bytes);
        }
        self.total_bytes.set(self.total_bytes.get() + bytes);
        if self.total_bytes.get() > BUDGET_BYTES {
            // Evict the least recently used quarter in one pass.
            let mut by_age: Vec<(u64, AssetUuid)> = entries
                .iter()
                .map(|(uuid, entry)| (entry.used, *uuid))
                .collect();
            by_age.sort_unstable();
            let target = BUDGET_BYTES * 3 / 4;
            for (_, uuid) in by_age {
                if self.total_bytes.get() <= target {
                    break;
                }
                if let Some(entry) = entries.remove(&uuid) {
                    self.total_bytes.set(self.total_bytes.get() - entry.bytes);
                }
            }
        }
    }

    fn tick(&self) -> u64 {
        let now = self.clock.get() + 1;
        self.clock.set(now);
        now
    }

    /// The cached texture of an asset, if loaded.
    pub fn get(&self, uuid: AssetUuid) -> Option<gdk::Texture> {
        let used = self.tick();
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(&uuid)?;
        entry.used = used;
        Some(entry.texture.clone())
    }

    /// Whether the thumbnail could not be made.
    pub fn is_failed(&self, uuid: AssetUuid) -> bool {
        self.failed.borrow().contains(&uuid)
    }

    /// Replaces the queue with these assets, most important first.
    pub fn want(&self, wanted: &[(AssetId, AssetUuid)]) {
        let mut generate = Vec::new();
        let mut candidates = Vec::new();
        {
            let entries = self.entries.borrow();
            let failed = self.failed.borrow();
            let mut missing = self.missing.borrow_mut();
            for &(id, uuid) in wanted {
                if entries.contains_key(&uuid) || failed.contains(&uuid) {
                    continue;
                }
                if let Some(known) = missing.get_mut(&uuid) {
                    // Ask for generation once; the service remembers it.
                    if known.0 == 0 {
                        *known = id;
                        generate.push(id);
                    }
                    continue;
                }
                candidates.push(uuid);
            }
        }
        if !generate.is_empty() {
            self.library.request_thumbnails(&generate);
        }
        let mut shared = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let queue: VecDeque<_> = candidates
            .into_iter()
            .filter(|uuid| !shared.taken.contains(uuid))
            .map(|uuid| (uuid, self.library.thumbnail_path(uuid)))
            .collect();
        shared.wanted = queue;
        drop(shared);
        self.shared.wake.notify_all();
    }

    /// A thumbnail was written: load it next time it is wanted.
    pub fn thumbnail_ready(&self, uuid: AssetUuid) {
        self.missing.borrow_mut().remove(&uuid);
        self.failed.borrow_mut().remove(&uuid);
        // A rebuilt thumbnail replaces the old one.
        if let Some(entry) = self.entries.borrow_mut().remove(&uuid) {
            self.total_bytes.set(self.total_bytes.get() - entry.bytes);
        }
        self.notify();
    }

    pub fn thumbnail_failed(&self, id: AssetId) {
        let uuid = self
            .missing
            .borrow()
            .iter()
            .find_map(|(uuid, asset)| (*asset == id).then_some(*uuid));
        if let Some(uuid) = uuid {
            self.missing.borrow_mut().remove(&uuid);
            self.failed.borrow_mut().insert(uuid);
            self.notify();
        }
    }

    /// Forgets everything, after thumbnails were rebuilt.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.total_bytes.set(0);
        self.missing.borrow_mut().clear();
        self.failed.borrow_mut().clear();
        self.notify();
    }
}

impl Drop for ThumbnailLoader {
    fn drop(&mut self) {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.stop = true;
        queue.wanted.clear();
        drop(queue);
        self.shared.wake.notify_all();
    }
}

fn worker(shared: &Shared, sender: &async_channel::Sender<Loaded>) {
    loop {
        let next = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queue.stop {
                    return;
                }
                if let Some(next) = queue.wanted.pop_front() {
                    queue.taken.insert(next.0);
                    break next;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let (uuid, path) = next;
        let loaded = match decode_jpeg(&path) {
            Ok(pixels) => Loaded::Ready(uuid, pixels),
            Err(_) => Loaded::Missing(uuid),
        };
        if sender.send_blocking(loaded).is_err() {
            return;
        }
    }
}
