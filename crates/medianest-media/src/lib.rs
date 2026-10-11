//! Reading photos and videos: metadata, pixels, thumbnails and place names.
//!
//! Nothing in this crate ever writes to a media file.

pub mod bmff;
pub mod dates;
pub mod decode;
pub mod exif;
pub mod ffmpeg;
pub mod geocode;
pub mod jpeg;
pub mod probe;
pub mod sniff;
pub mod thumbs;

pub use decode::{DecodeError, Decoder, HELPER_FLAG, Size, run_helper_if_requested};
pub use ffmpeg::{ToolError, VideoInfo, VideoTools};
pub use probe::{MediaToolkit, ProbeError};
pub use sniff::{detect, has_media_extension};
pub use thumbs::{
    PREVIEW_LONG_SIDE, THUMB_SHORT_SIDE, ThumbnailCache, ThumbnailError, ThumbnailFacts,
    ThumbnailSource, cache_home, hash_distance, render,
};
