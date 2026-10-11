//! The SQLite catalog of a MediaNest library.
//!
//! A catalog records sources, assets, file locations, user metadata, the
//! review inbox and the journal of file operations. It never touches media
//! files. One [`Writer`] owns all writes; any number of read-only [`Db`]
//! connections may query concurrently thanks to WAL journaling.

mod convert;
mod db;
mod error;
mod export;
mod records;
pub mod schema;
mod view;
mod writer;

pub use db::Db;
pub use error::{CatalogError, Result};
pub use export::{
    EXPORT_FORMAT, EXPORT_VERSION, ExportedAlbum, ExportedAsset, ExportedPath, MetadataExport,
    parse_hash,
};
pub use records::{
    Album, Asset, Facet, FileLocation, FileRecord, JobRecord, JobState, LibraryStats, MapPoint,
    Operation, ReviewItem, Source, Tag, ThumbnailWork, TimelineItem, flags,
};
pub use view::{Scope, Sort, View};
pub use writer::{Added, NewFile, ReviewSubject, UserMetadata, Writer};
