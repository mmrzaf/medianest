//! Reads everything MediaNest needs to know about a file.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Local};
use medianest_core::{AssetMetadata, CaptureTime, Confidence, DateSource, Format, MediaKind};

use crate::decode::Decoder;
use crate::exif::{self, ExifData};
use crate::ffmpeg::VideoTools;
use crate::{bmff, dates, geocode, jpeg, sniff};

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("not a supported photo or video format")]
    Unsupported,
    #[error("could not read the file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the file is damaged: {0}")]
    Corrupt(String),
}

/// Probing and decoding facilities, shared by every worker.
#[derive(Clone, Debug)]
pub struct MediaToolkit {
    pub decoder: Decoder,
    pub video: VideoTools,
}

impl MediaToolkit {
    /// Uses helper processes for risky decoders and finds video tools on `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: Decoder::isolated(),
            video: VideoTools::detect(),
        }
    }

    /// Decodes everything in this process. For tests.
    #[must_use]
    pub fn in_process() -> Self {
        Self {
            decoder: Decoder::in_process(),
            video: VideoTools::detect(),
        }
    }

    /// Reads a file's metadata. Never modifies the file.
    pub fn probe(&self, path: &Path) -> Result<AssetMetadata, ProbeError> {
        let format = sniff::detect(path).ok_or(ProbeError::Unsupported)?;
        let file_metadata = fs::metadata(path)?;
        if !file_metadata.is_file() {
            return Err(ProbeError::Unsupported);
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut metadata = AssetMetadata::minimal(format);
        metadata.is_screenshot = is_screenshot_name(&name);
        match format.kind() {
            MediaKind::Photo => probe_photo(path, format, &mut metadata)?,
            MediaKind::Video => self.probe_video(path, &mut metadata),
        }
        if format == Format::Jxl {
            if let Ok(info) = self.video.probe(path) {
                metadata.width = info.width;
                metadata.height = info.height;
            }
        }
        if !metadata.taken.is_known() {
            metadata.taken = dates::date_from_file_name(&name)
                .map(|local| CaptureTime::floating(local, DateSource::FileName, Confidence::Medium))
                .or_else(|| {
                    file_metadata.modified().ok().map(|modified| {
                        CaptureTime::from_instant(
                            DateTime::<Local>::from(modified),
                            DateSource::FileModified,
                            Confidence::Low,
                        )
                    })
                })
                .unwrap_or(CaptureTime::UNKNOWN);
        }
        if let Some(location) = metadata.location {
            metadata.place = geocode::reverse(location);
        }
        Ok(metadata)
    }

    fn probe_video(&self, path: &Path, metadata: &mut AssetMetadata) {
        let Ok(info) = self.video.probe(path) else {
            return;
        };
        metadata.duration_ms = info.duration_ms;
        metadata.width = info.width;
        metadata.height = info.height;
        if let Some(taken) = info.taken {
            metadata.taken = taken;
        }
        metadata.location = info.location;
        metadata.camera_make = info.make;
        metadata.camera_model = info.model;
        metadata.content_identifier = info.content_identifier;
    }
}

impl Default for MediaToolkit {
    fn default() -> Self {
        Self::new()
    }
}

fn is_screenshot_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "screenshot",
        "screen shot",
        "screen_shot",
        "scrnshot",
        "bildschirmfoto",
        "capture d’écran",
        "capture d'écran",
        "schermafbeelding",
        "captura de pantalla",
        "スクリーンショット",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn read_prefix(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut data)?;
    Ok(data)
}

fn apply_exif(metadata: &mut AssetMetadata, exif: ExifData) {
    if let Some(local) = exif.taken {
        metadata.taken = match exif.taken_offset_minutes {
            Some(offset) if exif.taken_is_original => {
                CaptureTime::zoned(local, offset, DateSource::ExifWithOffset, Confidence::High)
            }
            _ => CaptureTime::floating(local, DateSource::Exif, Confidence::Medium),
        };
        if let (Some(offset), false) = (exif.taken_offset_minutes, exif.taken_is_original) {
            metadata.taken.offset_minutes = Some(offset);
        }
    }
    metadata.camera_make = exif.make;
    metadata.camera_model = exif.model;
    metadata.lens = exif.lens;
    metadata.aperture = exif.aperture;
    metadata.exposure_seconds = exif.exposure_seconds;
    metadata.iso = exif.iso;
    metadata.focal_length_mm = exif.focal_length_mm;
    metadata.focal_length_35mm = exif.focal_length_35mm;
    metadata.location = exif.location;
    if let Some(orientation) = exif.orientation {
        metadata.orientation = orientation;
    }
    if metadata.width.is_none() {
        metadata.width = exif.width;
        metadata.height = exif.height;
    }
    if exif
        .software
        .as_deref()
        .is_some_and(|software| software.to_lowercase().contains("screenshot"))
    {
        metadata.is_screenshot = true;
    }
}

fn probe_photo(
    path: &Path,
    format: Format,
    metadata: &mut AssetMetadata,
) -> Result<(), ProbeError> {
    let exif = exif::read(path);
    match format {
        Format::Jpeg => {
            let prefix = read_prefix(path, 2 * 1024 * 1024)?;
            let header = jpeg::read_header(&prefix);
            let (width, height) = match &header {
                Some(header) => (header.width, header.height),
                None => image::image_dimensions(path)
                    .map_err(|error| ProbeError::Corrupt(error.to_string()))?,
            };
            metadata.width = Some(width);
            metadata.height = Some(height);
            let xmp = header.and_then(|header| header.xmp);
            if xmp
                .as_deref()
                .is_some_and(|xmp| xmp.contains("MotionPhoto") || xmp.contains("MicroVideo"))
            {
                let data = fs::read(path)?;
                metadata.motion_video_offset = jpeg::motion_video_length(xmp.as_deref(), &data);
            }
        }
        Format::Png | Format::Webp | Format::Gif | Format::Tiff => {
            let (width, height) = image::image_dimensions(path)
                .map_err(|error| ProbeError::Corrupt(error.to_string()))?;
            metadata.width = Some(width);
            metadata.height = Some(height);
        }
        Format::Heic | Format::Avif => {
            let info = bmff::heif_info(path)
                .ok_or_else(|| ProbeError::Corrupt("no primary image in HEIF container".into()))?;
            let (width, height) = info.display_size();
            metadata.width = Some(width);
            metadata.height = Some(height);
        }
        Format::Jxl => {
            // Sized by ffprobe in `MediaToolkit::probe`, which can read JPEG XL.
        }
        raw if raw.is_raw() => {
            let data = fs::read(path)?;
            if let Some(preview) = jpeg::largest_embedded(&data) {
                metadata.width = Some(preview.width);
                metadata.height = Some(preview.height);
                if exif.is_none() {
                    let embedded = &data[preview.offset..];
                    if let Some(payload) = jpeg::exif_payload(embedded) {
                        if let Some(found) = exif::read_raw(payload.to_vec()) {
                            apply_exif(
                                metadata,
                                ExifData {
                                    width: None,
                                    height: None,
                                    ..found
                                },
                            );
                        }
                    }
                }
            }
        }
        _ => return Err(ProbeError::Unsupported),
    }
    if let Some(found) = exif {
        // Sizes from the file structure are more reliable than EXIF tags.
        apply_exif(
            metadata,
            ExifData {
                width: None,
                height: None,
                ..found
            },
        );
    }
    if matches!(format, Format::Heic | Format::Avif | Format::Jxl) {
        // These decoders orient pixels themselves.
        metadata.orientation = 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_names() {
        assert!(is_screenshot_name("Screenshot_20230701-101500.png"));
        assert!(is_screenshot_name("Screen Shot 2019-01-01 at 10.00.00.png"));
        assert!(!is_screenshot_name("IMG_0001.JPG"));
    }

    #[test]
    fn png_without_exif_falls_back_to_file_name_date() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("Screenshot_20230701-101500.png");
        image::RgbImage::new(30, 20).save(&path).expect("save");
        let metadata = MediaToolkit::in_process().probe(&path).expect("probe");
        assert_eq!(metadata.format, Format::Png);
        assert_eq!((metadata.width, metadata.height), (Some(30), Some(20)));
        assert!(metadata.is_screenshot);
        assert_eq!(metadata.taken.source, DateSource::FileName);
        assert_eq!(metadata.taken.confidence, Confidence::Medium);
    }

    #[test]
    fn files_without_any_date_use_modified_time_with_low_confidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("photo.png");
        image::RgbImage::new(4, 4).save(&path).expect("save");
        let metadata = MediaToolkit::in_process().probe(&path).expect("probe");
        assert_eq!(metadata.taken.source, DateSource::FileModified);
        assert!(!metadata.taken.is_trustworthy());
    }

    #[test]
    fn unsupported_and_corrupt_files_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = dir.path().join("notes.txt");
        fs::write(&text, b"hello").expect("write");
        assert!(matches!(
            MediaToolkit::in_process().probe(&text),
            Err(ProbeError::Unsupported)
        ));
        let broken = dir.path().join("broken.png");
        fs::write(&broken, b"\x89PNG\r\n\x1a\n").expect("write");
        assert!(matches!(
            MediaToolkit::in_process().probe(&broken),
            Err(ProbeError::Corrupt(_))
        ));
    }
}
