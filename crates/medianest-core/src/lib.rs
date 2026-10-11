//! Domain types shared by every MediaNest crate.
//!
//! This crate has no I/O. It defines identifiers, the capture-time model,
//! enumerations persisted in the catalog, byte-safe paths and the search
//! query language.

#[macro_use]
mod macros;

mod ids;
mod kinds;
mod path;
pub mod query;
mod time;

mod metadata;

pub use ids::{
    AlbumId, AssetId, AssetUuid, FileId, JobId, LibraryUuid, OperationUuid, ReviewId, SourceId,
    TagId,
};
pub use kinds::{
    AlbumSort, FileStatus, Format, MediaKind, OperationKind, OperationState, ReviewKind,
    ReviewState, SourceKind, SourceStatus, ThumbState, UnknownValue,
};
pub use metadata::{
    AssetMetadata, FileStamp, GeoPoint, Place, camera_name, normalize_tag, orientation_swaps_axes,
    tidy_tag,
};
pub use path::PathBytes;
pub use time::{
    CaptureTime, Confidence, DateSource, Day, local_ms_from_naive, naive_from_local_ms,
};

/// Milliseconds since the Unix epoch for "now".
#[must_use]
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
