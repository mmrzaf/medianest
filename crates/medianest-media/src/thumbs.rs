//! The thumbnail and preview cache.
//!
//! Cached images live outside the library, under the user's cache directory,
//! and can be deleted at any time: they are regenerated on demand.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
use medianest_core::{AssetUuid, Format, LibraryUuid, MediaKind};

use crate::decode::{DecodeError, Decoder, Size, decode_jpeg, shrink_long_side, shrink_short_side};
use crate::ffmpeg::{ToolError, VideoTools};

/// Shorter side of grid thumbnails, in pixels.
pub const THUMB_SHORT_SIDE: u32 = 360;
/// Longer side of viewer previews, in pixels.
pub const PREVIEW_LONG_SIDE: u32 = 2048;

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Video(#[from] ToolError),
    #[error("could not write to the cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not encode the thumbnail: {0}")]
    Encode(String),
}

/// What to make a thumbnail of.
#[derive(Clone, Copy, Debug)]
pub struct ThumbnailSource<'a> {
    pub path: &'a Path,
    pub format: Format,
    pub orientation: u8,
    /// Length of an embedded motion video, for motion photos.
    pub motion_video_length: Option<u64>,
    pub duration_ms: Option<u64>,
}

/// Facts computed while generating a thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailFacts {
    /// Average color as `0xRRGGBB`.
    pub color: u32,
    /// 64-bit difference hash for finding similar photos.
    pub phash: u64,
}

/// Cache of thumbnails and previews for one library.
#[derive(Clone, Debug)]
pub struct ThumbnailCache {
    root: PathBuf,
}

/// Base directory for MediaNest caches, following the XDG specification.
#[must_use]
pub fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("medianest")
}

impl ThumbnailCache {
    #[must_use]
    pub fn for_library(library: LibraryUuid) -> Self {
        Self::with_root(cache_home().join(library.simple()))
    }

    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sharded(&self, kind: &str, uuid: AssetUuid) -> PathBuf {
        let name = uuid.simple();
        self.root
            .join(kind)
            .join(name.get(..2).unwrap_or("00"))
            .join(format!("{name}.jpg"))
    }

    #[must_use]
    pub fn thumbnail_path(&self, uuid: AssetUuid) -> PathBuf {
        self.sharded("thumbnails", uuid)
    }

    #[must_use]
    pub fn preview_path(&self, uuid: AssetUuid) -> PathBuf {
        self.sharded("previews", uuid)
    }

    /// Creates the grid thumbnail and returns its color and similarity hash.
    pub fn generate(
        &self,
        uuid: AssetUuid,
        source: &ThumbnailSource<'_>,
        decoder: &Decoder,
        tools: &VideoTools,
    ) -> Result<ThumbnailFacts, ThumbnailError> {
        let image = render(source, decoder, tools, Size::ShortSide(THUMB_SHORT_SIDE))?;
        let thumbnail = shrink_short_side(&image, THUMB_SHORT_SIDE);
        let flat = flatten(&thumbnail);
        write_jpeg(&self.thumbnail_path(uuid), &flat, 82)?;
        Ok(ThumbnailFacts {
            color: average_color(&flat),
            phash: difference_hash(&flat),
        })
    }

    /// Returns a cached viewer preview, creating it if needed.
    pub fn preview(
        &self,
        uuid: AssetUuid,
        source: &ThumbnailSource<'_>,
        decoder: &Decoder,
        tools: &VideoTools,
    ) -> Result<PathBuf, ThumbnailError> {
        let path = self.preview_path(uuid);
        if path.is_file() {
            // Mark as recently used for cache trimming.
            let _ = fs::File::options()
                .append(true)
                .open(&path)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()));
            return Ok(path);
        }
        let image = render(source, decoder, tools, Size::LongSide(PREVIEW_LONG_SIDE))?;
        let preview = shrink_long_side(&image, PREVIEW_LONG_SIDE);
        write_jpeg(&path, &flatten(&preview), 86)?;
        Ok(path)
    }

    /// Deletes every cached image of an asset.
    pub fn remove(&self, uuid: AssetUuid) {
        let _ = fs::remove_file(self.thumbnail_path(uuid));
        let _ = fs::remove_file(self.preview_path(uuid));
    }

    /// Deletes the whole cache for this library.
    pub fn clear(&self) -> std::io::Result<()> {
        match fs::remove_dir_all(&self.root) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Bytes used by thumbnails and by previews.
    #[must_use]
    pub fn usage(&self) -> (u64, u64) {
        (
            dir_size(&self.root.join("thumbnails")),
            dir_size(&self.root.join("previews")),
        )
    }

    /// Deletes least recently used previews until they take at most `limit` bytes.
    pub fn trim_previews(&self, limit: u64) -> u64 {
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        walk(&self.root.join("previews"), &mut |path, meta| {
            files.push((
                meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                meta.len(),
                path.to_path_buf(),
            ));
        });
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        files.sort_by_key(|(modified, _, _)| *modified);
        let mut freed = 0;
        for (_, size, path) in files {
            if total <= limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
                freed += size;
            }
        }
        freed
    }
}

fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, &fs::Metadata)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, visit);
        } else {
            visit(&path, &meta);
        }
    }
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    walk(dir, &mut |_, meta| total += meta.len());
    total
}

/// Produces pixels for any supported asset, oriented, at least `size` large where possible.
pub fn render(
    source: &ThumbnailSource<'_>,
    decoder: &Decoder,
    tools: &VideoTools,
    size: Size,
) -> Result<RgbaImage, ThumbnailError> {
    if source.format.kind() == MediaKind::Video {
        let edge = match size {
            Size::Full => 3840,
            Size::ShortSide(value) => value.saturating_mul(2).max(720),
            Size::LongSide(value) => value,
        };
        let duration = source.duration_ms.unwrap_or(0);
        #[allow(clippy::cast_precision_loss)]
        let at = if duration > 3_000 {
            (duration as f64 / 1000.0 / 3.0).min(3.0)
        } else {
            0.0
        };
        let bytes = tools
            .frame(source.path, at, edge)
            .or_else(|_| tools.frame(source.path, 0.0, edge))?;
        return Ok(decode_jpeg(bytes.as_slice(), Size::Full)?.into_rgba8());
    }
    Ok(decoder.decode(source.path, source.format, source.orientation, size)?)
}

/// Drops transparency by compositing over white.
fn flatten(image: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let pixel = image.get_pixel(x, y).0;
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            u8::try_from((u16::from(channel) * alpha + 255 * (255 - alpha)) / 255)
                .unwrap_or(u8::MAX)
        };
        image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    })
}

fn write_jpeg(path: &Path, image: &RgbImage, quality: u8) -> Result<(), ThumbnailError> {
    let parent = path
        .parent()
        .ok_or_else(|| ThumbnailError::Encode("no parent directory".into()))?;
    fs::create_dir_all(parent)?;
    let mut encoded = Vec::with_capacity(64 * 1024);
    JpegEncoder::new_with_quality(&mut encoded, quality)
        .encode_image(image)
        .map_err(|error| ThumbnailError::Encode(error.to_string()))?;
    let mut temporary = tempfile_in(parent)?;
    temporary.1.write_all(&encoded)?;
    temporary.1.sync_data()?;
    fs::rename(&temporary.0, path)?;
    Ok(())
}

fn tempfile_in(dir: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    for attempt in 0..100u32 {
        let name = format!(
            ".tmp-{}-{}-{attempt}",
            std::process::id(),
            medianest_core::now_ms()
        );
        let path = dir.join(name);
        match fs::File::options().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("could not create a temporary file"))
}

/// Average color as `0xRRGGBB`.
#[must_use]
pub fn average_color(image: &RgbImage) -> u32 {
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for pixel in image.pixels() {
        r += u64::from(pixel[0]);
        g += u64::from(pixel[1]);
        b += u64::from(pixel[2]);
    }
    let count = u64::from(image.width()) * u64::from(image.height());
    if count == 0 {
        return 0;
    }
    let channel = |sum: u64| u32::try_from(sum / count).unwrap_or(0);
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// 64-bit difference hash: robust to scaling, compression and small edits.
#[must_use]
pub fn difference_hash(image: &RgbImage) -> u64 {
    let gray: GrayImage = DynamicImage::ImageRgb8(image.clone()).into_luma8();
    let small = image::imageops::resize(&gray, 9, 8, FilterType::Triangle);
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                hash |= 1;
            }
        }
    }
    hash
}

/// Number of differing bits between two similarity hashes.
#[must_use]
pub const fn hash_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                u8::try_from(x * 255 / width.max(1)).unwrap_or(0),
                u8::try_from(y * 255 / height.max(1)).unwrap_or(0),
                90,
            ])
        })
    }

    #[test]
    fn generates_oriented_thumbnails_with_facts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let photo = dir.path().join("photo.jpg");
        gradient(1200, 800).save(&photo).expect("save");
        let cache = ThumbnailCache::with_root(dir.path().join("cache"));
        let uuid = AssetUuid::new();
        let source = ThumbnailSource {
            path: &photo,
            format: Format::Jpeg,
            orientation: 6,
            motion_video_length: None,
            duration_ms: None,
        };
        let facts = cache
            .generate(
                uuid,
                &source,
                &Decoder::in_process(),
                &VideoTools::default(),
            )
            .expect("thumbnail");
        let thumb = image::open(cache.thumbnail_path(uuid)).expect("open");
        assert_eq!(thumb.width(), THUMB_SHORT_SIDE);
        assert!(thumb.height() > thumb.width(), "rotated to portrait");
        assert_ne!(facts.color, 0);
        let preview = cache
            .preview(
                uuid,
                &source,
                &Decoder::in_process(),
                &VideoTools::default(),
            )
            .expect("preview");
        assert!(preview.is_file());
        assert!(cache.usage().0 > 0);
        cache.remove(uuid);
        assert!(!cache.thumbnail_path(uuid).exists());
    }

    #[test]
    fn similar_images_have_close_hashes() {
        let base = gradient(400, 300);
        let resized = image::imageops::resize(&base, 200, 150, FilterType::Lanczos3);
        let mut different = gradient(400, 300);
        image::imageops::flip_horizontal_in_place(&mut different);
        let a = difference_hash(&base);
        assert!(hash_distance(a, difference_hash(&resized)) <= 4);
        assert!(hash_distance(a, difference_hash(&different)) > 20);
    }

    #[test]
    fn trimming_removes_oldest_previews_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = ThumbnailCache::with_root(dir.path().to_path_buf());
        let older = cache.preview_path(AssetUuid::new());
        let newer = cache.preview_path(AssetUuid::new());
        for path in [&older, &newer] {
            fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            fs::write(path, vec![0u8; 1000]).expect("write");
        }
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .append(true)
            .open(&older)
            .expect("open")
            .set_modified(old_time)
            .expect("touch");
        assert_eq!(cache.trim_previews(1000), 1000);
        assert!(!older.exists() && newer.exists());
    }
}
