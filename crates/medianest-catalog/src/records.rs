//! Plain data returned by catalog queries.

use medianest_core::{
    AlbumId, AlbumSort, AssetId, AssetUuid, CaptureTime, FileId, FileStamp, FileStatus, Format,
    GeoPoint, JobId, MediaKind, OperationKind, OperationState, OperationUuid, PathBytes, Place,
    ReviewId, ReviewKind, ReviewState, SourceId, SourceKind, SourceStatus, TagId, ThumbState,
    camera_name, orientation_swaps_axes,
};
use serde::{Deserialize, Serialize};

/// A folder MediaNest reads media from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub path: PathBytes,
    pub name: String,
    pub watch: bool,
    pub status: SourceStatus,
    pub added_ms: i64,
    pub scanned_ms: Option<i64>,
    /// Number of file locations in this source.
    pub file_count: u64,
    /// Total size of those files in bytes.
    pub total_bytes: u64,
    /// Files whose status is missing or changed.
    pub problem_count: u64,
}

/// Everything known about one asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub uuid: AssetUuid,
    pub kind: MediaKind,
    pub format: Format,
    /// Display name, normally the file name of the primary location.
    pub name: String,
    pub hash: Option<[u8; 32]>,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub orientation: u8,
    /// Extra clockwise rotation chosen by the user, in degrees.
    pub rotation: u16,
    pub taken: CaptureTime,
    /// The date as read from the file, kept when the user overrides it.
    pub probed_taken: CaptureTime,
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
    pub favorite: bool,
    pub rating: u8,
    pub hidden: bool,
    pub caption: Option<String>,
    pub trashed_ms: Option<i64>,
    pub screenshot: bool,
    pub content_identifier: Option<String>,
    pub motion_video_offset: Option<u64>,
    pub live_video: Option<AssetId>,
    pub live_component: bool,
    pub color: Option<u32>,
    pub phash: Option<u64>,
    pub thumb_state: ThumbState,
    /// Worst status across the asset's files.
    pub file_state: FileStatus,
    pub added_ms: i64,
    pub updated_ms: i64,
}

impl Asset {
    /// Display size after EXIF orientation and user rotation.
    #[must_use]
    pub fn display_size(&self) -> Option<(u32, u32)> {
        let (width, height) = (self.width?, self.height?);
        let swap = orientation_swaps_axes(self.orientation) ^ (self.rotation % 180 == 90);
        Some(if swap {
            (height, width)
        } else {
            (width, height)
        })
    }

    #[must_use]
    pub fn camera_name(&self) -> Option<String> {
        camera_name(self.camera_make.as_deref(), self.camera_model.as_deref())
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live_video.is_some() || self.motion_video_offset.is_some()
    }

    #[must_use]
    pub fn megapixels(&self) -> Option<f64> {
        let (width, height) = (self.width?, self.height?);
        Some(f64::from(width) * f64::from(height) / 1_000_000.0)
    }
}

/// One on-disk location of an asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: FileId,
    pub asset_id: AssetId,
    pub source_id: SourceId,
    pub relpath: PathBytes,
    pub stamp: FileStamp,
    pub status: FileStatus,
    pub hash: Option<[u8; 32]>,
    pub checked_ms: Option<i64>,
    pub added_ms: i64,
}

/// A file location joined with its source, ready to open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileLocation {
    pub file: FileRecord,
    pub source_kind: SourceKind,
    pub source_path: PathBytes,
    pub source_name: String,
    pub source_status: SourceStatus,
}

impl FileLocation {
    /// Absolute path of the file.
    #[must_use]
    pub fn path(&self) -> std::path::PathBuf {
        self.source_path.as_path().join(self.file.relpath.as_path())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    /// Saved search for smart albums; `None` for manual albums.
    pub query: Option<String>,
    pub cover: Option<AssetId>,
    pub sort: AlbumSort,
    /// Items in a manual album. Smart albums report zero; count them with a query.
    pub count: u64,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl Album {
    #[must_use]
    pub fn is_smart(&self) -> bool {
        self.query.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: ReviewId,
    pub kind: ReviewKind,
    pub asset_id: Option<AssetId>,
    pub file_id: Option<FileId>,
    pub operation: Option<OperationUuid>,
    pub path: Option<PathBytes>,
    pub detail: serde_json::Value,
    pub state: ReviewState,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Display name of the asset or file concerned.
    pub subject: String,
}

/// A journaled file operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub uuid: OperationUuid,
    pub kind: OperationKind,
    pub state: OperationState,
    pub source_path: Option<PathBytes>,
    pub temp_path: Option<PathBytes>,
    /// Destination relative to the managed folder.
    pub dest_relpath: Option<PathBytes>,
    /// Where the original was renamed while it is being removed.
    pub aside_path: Option<PathBytes>,
    pub source_stamp: Option<FileStamp>,
    pub hash: Option<[u8; 32]>,
    pub asset_id: Option<AssetId>,
    pub file_id: Option<FileId>,
    pub batch: Option<String>,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl Operation {
    #[must_use]
    pub fn planned(
        kind: OperationKind,
        source: PathBytes,
        stamp: FileStamp,
        dest_relpath: PathBytes,
        batch: Option<String>,
    ) -> Self {
        let now = medianest_core::now_ms();
        Self {
            uuid: OperationUuid::new(),
            kind,
            state: OperationState::Planned,
            source_path: Some(source),
            temp_path: None,
            dest_relpath: Some(dest_relpath),
            aside_path: None,
            source_stamp: Some(stamp),
            hash: None,
            asset_id: None,
            file_id: None,
            batch,
            error: None,
            created_ms: now,
            updated_ms: now,
        }
    }
}

/// Outcome of a background job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running = 0,
    Completed = 1,
    Failed = 2,
    Cancelled = 3,
}

impl JobState {
    #[must_use]
    pub const fn code(self) -> i64 {
        self as i64
    }

    #[must_use]
    pub const fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Completed,
            2 => Self::Failed,
            3 => Self::Cancelled,
            _ => Self::Running,
        }
    }
}

/// A finished or running background job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: String,
    pub title: String,
    pub state: JobState,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub summary: String,
}

/// Bit flags carried by every timeline item.
pub mod flags {
    pub const FAVORITE: u16 = 1;
    pub const VIDEO: u16 = 1 << 1;
    pub const LIVE: u16 = 1 << 2;
    pub const RAW: u16 = 1 << 3;
    pub const MISSING: u16 = 1 << 4;
    pub const CHANGED: u16 = 1 << 5;
    pub const UNCERTAIN_DATE: u16 = 1 << 6;
    pub const HIDDEN: u16 = 1 << 7;
    pub const TRASHED: u16 = 1 << 8;
    pub const THUMB_FAILED: u16 = 1 << 9;
    pub const THUMB_READY: u16 = 1 << 10;
    pub const UNVERIFIED: u16 = 1 << 11;
}

/// The minimum the photo grid needs per asset. Kept small: libraries hold
/// hundreds of thousands of these in memory.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub id: AssetId,
    pub uuid: AssetUuid,
    /// Local capture time, or `None` when unknown.
    pub local_ms: Option<i64>,
    /// Display width divided by height.
    pub aspect: f32,
    /// Average color as `0xRRGGBB`, used as a placeholder.
    pub color: u32,
    pub flags: u16,
    pub rating: u8,
    pub duration_ms: u32,
}

impl TimelineItem {
    #[must_use]
    pub const fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// Totals shown on the health page and in the sidebar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryStats {
    pub assets: u64,
    pub photos: u64,
    pub videos: u64,
    pub favorites: u64,
    pub hidden: u64,
    pub trashed: u64,
    pub unknown_dates: u64,
    pub uncertain_dates: u64,
    pub with_location: u64,
    pub files: u64,
    pub managed_files: u64,
    pub linked_files: u64,
    pub managed_bytes: u64,
    pub linked_bytes: u64,
    pub verified_files: u64,
    pub unverified_files: u64,
    pub missing_files: u64,
    pub changed_files: u64,
    pub thumbs_pending: u64,
    pub thumbs_failed: u64,
    pub tags: u64,
    pub albums: u64,
    pub open_reviews: u64,
    pub oldest_check_ms: Option<i64>,
}

/// A count of assets per distinct value, for search suggestions and places.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facet {
    pub value: String,
    pub detail: Option<String>,
    pub count: u64,
}

/// An asset that still needs a thumbnail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThumbnailWork {
    pub id: AssetId,
    pub uuid: AssetUuid,
    pub format: Format,
    pub orientation: u8,
    pub motion_video_offset: Option<u64>,
    pub path: std::path::PathBuf,
}

/// A point for the map.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MapPoint {
    pub id: AssetId,
    pub latitude: f64,
    pub longitude: f64,
}
