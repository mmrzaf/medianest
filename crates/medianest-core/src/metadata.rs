use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::{CaptureTime, Format, MediaKind};

/// A point on Earth in WGS84 degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
}

impl GeoPoint {
    /// Rejects coordinates outside the valid range and the common `0,0` placeholder.
    #[must_use]
    pub fn new(latitude: f64, longitude: f64, altitude: Option<f64>) -> Option<Self> {
        let valid = latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude)
            && !(latitude.abs() < 1e-6 && longitude.abs() < 1e-6);
        valid.then_some(Self {
            latitude,
            longitude,
            altitude: altitude.filter(|value| value.is_finite()),
        })
    }
}

/// A named place, resolved offline from coordinates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub city: String,
    pub region: Option<String>,
    pub country: String,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: String,
}

impl Place {
    /// `City, Country` for compact display.
    #[must_use]
    pub fn short(&self) -> String {
        if self.city.is_empty() {
            self.country.clone()
        } else {
            format!("{}, {}", self.city, self.country)
        }
    }
}

/// Everything MediaNest learns about a file by reading it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetMetadata {
    pub kind: MediaKind,
    pub format: Format,
    /// Pixel size as stored, before orientation is applied.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// EXIF orientation (1-8). Videos use 1 with rotation folded into the size.
    pub orientation: u8,
    pub taken: CaptureTime,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub aperture: Option<f32>,
    pub exposure_seconds: Option<f32>,
    pub iso: Option<u32>,
    pub location: Option<GeoPoint>,
    pub place: Option<Place>,
    /// Apple Live Photo pairing identifier shared by the still and its video.
    pub content_identifier: Option<String>,
    /// Byte offset from the end of the file where an embedded motion video starts.
    pub motion_video_offset: Option<u64>,
    pub is_screenshot: bool,
}

impl AssetMetadata {
    /// Metadata known only from the file name, used when a file cannot be read.
    #[must_use]
    pub fn minimal(format: Format) -> Self {
        Self {
            kind: format.kind(),
            format,
            width: None,
            height: None,
            duration_ms: None,
            orientation: 1,
            taken: CaptureTime::UNKNOWN,
            camera_make: None,
            camera_model: None,
            lens: None,
            focal_length_mm: None,
            focal_length_35mm: None,
            aperture: None,
            exposure_seconds: None,
            iso: None,
            location: None,
            place: None,
            content_identifier: None,
            motion_video_offset: None,
            is_screenshot: false,
        }
    }

    /// Display width and height after EXIF orientation.
    #[must_use]
    pub fn oriented_size(&self) -> Option<(u32, u32)> {
        let (width, height) = (self.width?, self.height?);
        Some(if orientation_swaps_axes(self.orientation) {
            (height, width)
        } else {
            (width, height)
        })
    }

    /// `Make Model`, without repeating the make when the model already contains it.
    #[must_use]
    pub fn camera_name(&self) -> Option<String> {
        camera_name(self.camera_make.as_deref(), self.camera_model.as_deref())
    }
}

/// Whether an EXIF orientation value rotates the image by 90 or 270 degrees.
#[must_use]
pub const fn orientation_swaps_axes(orientation: u8) -> bool {
    matches!(orientation, 5..=8)
}

/// Combines EXIF make and model into one readable name.
#[must_use]
pub fn camera_name(make: Option<&str>, model: Option<&str>) -> Option<String> {
    let make = make.map(str::trim).filter(|value| !value.is_empty());
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    match (make, model) {
        (Some(make), Some(model)) => {
            let first_word = make.split_whitespace().next().unwrap_or(make);
            if model.to_lowercase().starts_with(&first_word.to_lowercase()) {
                Some(model.to_owned())
            } else {
                Some(format!("{} {model}", tidy_make(make)))
            }
        }
        (Some(make), None) => Some(tidy_make(make)),
        (None, Some(model)) => Some(model.to_owned()),
        (None, None) => None,
    }
}

fn tidy_make(make: &str) -> String {
    let lower = make.to_lowercase();
    let known = [
        ("nikon", "Nikon"),
        ("canon", "Canon"),
        ("olympus", "Olympus"),
        ("om digital", "OM System"),
        ("fujifilm", "Fujifilm"),
        ("samsung", "Samsung"),
        ("sony", "Sony"),
        ("panasonic", "Panasonic"),
        ("pentax", "Pentax"),
        ("ricoh", "Ricoh"),
        ("google", "Google"),
        ("apple", "Apple"),
        ("huawei", "Huawei"),
        ("xiaomi", "Xiaomi"),
        ("oneplus", "OnePlus"),
        ("motorola", "Motorola"),
        ("leica", "Leica"),
        ("hasselblad", "Hasselblad"),
        ("gopro", "GoPro"),
        ("dji", "DJI"),
    ];
    known
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map_or_else(|| make.to_owned(), |(_, name)| (*name).to_owned())
}

/// Identity and modification stamp of a file, used to notice replacement or edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    /// Birth time, when the filesystem records it.
    pub btime_ns: Option<i64>,
    pub device: u64,
    pub inode: u64,
}

impl FileStamp {
    #[must_use]
    pub fn from_metadata(metadata: &Metadata) -> Self {
        let btime_ns = metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_nanos()).ok());
        Self {
            size: metadata.len(),
            mtime_ns: metadata.mtime().saturating_mul(1_000_000_000) + metadata.mtime_nsec(),
            ctime_ns: metadata.ctime().saturating_mul(1_000_000_000) + metadata.ctime_nsec(),
            btime_ns,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    /// The same file object, untouched since the stamp was taken.
    ///
    /// Inode numbers are reused immediately on many filesystems, so the change
    /// time and, where available, the birth time are compared as well. A file
    /// deleted and recreated with identical bytes therefore does not match.
    #[must_use]
    pub fn same_file_unchanged(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.size == other.size
            && self.mtime_ns == other.mtime_ns
            && self.ctime_ns == other.ctime_ns
            && match (self.btime_ns, other.btime_ns) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }

    /// Cheap check used by rescans: same size and modification time.
    #[must_use]
    pub fn looks_unmodified(&self, other: &Self) -> bool {
        self.size == other.size && self.mtime_ns == other.mtime_ns
    }
}

/// Canonical form used to compare tag names: trimmed, single-spaced, lowercase.
#[must_use]
pub fn normalize_tag(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A tag name as entered, with whitespace tidied but case preserved.
#[must_use]
pub fn tidy_tag(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_names_avoid_repetition() {
        assert_eq!(
            camera_name(Some("NIKON CORPORATION"), Some("NIKON D750")).as_deref(),
            Some("NIKON D750")
        );
        assert_eq!(
            camera_name(Some("Google"), Some("Pixel 7")).as_deref(),
            Some("Google Pixel 7")
        );
        assert_eq!(
            camera_name(Some("OLYMPUS IMAGING CORP."), Some("E-M10")).as_deref(),
            Some("Olympus E-M10")
        );
        assert_eq!(camera_name(None, Some("  ")), None);
    }

    #[test]
    fn geo_points_reject_placeholders() {
        assert!(GeoPoint::new(0.0, 0.0, None).is_none());
        assert!(GeoPoint::new(91.0, 0.5, None).is_none());
        assert!(GeoPoint::new(51.5, -0.12, Some(f64::NAN)).is_some_and(|p| p.altitude.is_none()));
    }

    #[test]
    fn tags_normalize() {
        assert_eq!(normalize_tag("  Summer   Trip "), "summer trip");
        assert_eq!(tidy_tag("  Summer   Trip "), "Summer Trip");
    }

    #[test]
    fn stamps_detect_recreated_files() {
        let base = FileStamp {
            size: 10,
            mtime_ns: 1,
            ctime_ns: 2,
            btime_ns: Some(3),
            device: 1,
            inode: 7,
        };
        let recreated = FileStamp {
            ctime_ns: 5,
            btime_ns: Some(5),
            ..base
        };
        assert!(base.same_file_unchanged(&base));
        assert!(!base.same_file_unchanged(&recreated));
        assert!(base.looks_unmodified(&recreated));
    }
}
