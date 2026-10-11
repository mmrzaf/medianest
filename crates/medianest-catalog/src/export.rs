//! Portable metadata export.
//!
//! The export holds everything a person put into the library: tags, albums,
//! favorites, ratings, captions, rotations, hidden flags and dates they set.
//! Assets are identified by content hash and by path, so the document can be
//! applied to a rebuilt catalog.

use std::path::Path;

use medianest_core::{AssetId, CaptureTime, DateSource, PathBytes, now_ms};
use rusqlite::backup::Backup;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::db::Db;
use crate::writer::UserMetadata;

pub const EXPORT_FORMAT: &str = "medianest-metadata";
pub const EXPORT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetadataExport {
    pub format: String,
    pub version: u32,
    pub library: String,
    pub exported_ms: i64,
    pub albums: Vec<ExportedAlbum>,
    pub assets: Vec<ExportedAsset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedAlbum {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// A path that survives JSON even when it is not valid UTF-8.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedPath {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relpath: Option<String>,
    /// Hex-encoded bytes, used when the path is not valid UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relpath_hex: Option<String>,
}

impl ExportedPath {
    #[must_use]
    pub fn new(source: &PathBytes, relpath: &PathBytes) -> Self {
        match std::str::from_utf8(relpath.as_bytes()) {
            Ok(text) => Self {
                source: source.display(),
                relpath: Some(text.to_owned()),
                relpath_hex: None,
            },
            Err(_) => Self {
                source: source.display(),
                relpath: None,
                relpath_hex: Some(hex(relpath.as_bytes())),
            },
        }
    }

    /// The relative path bytes, decoding the hex form when present.
    #[must_use]
    pub fn relpath_bytes(&self) -> Option<PathBytes> {
        if let Some(text) = &self.relpath {
            return Some(PathBytes::from_vec(text.as_bytes().to_vec()));
        }
        let hex = self.relpath_hex.as_deref()?;
        if hex.len() % 2 != 0 {
            return None;
        }
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()
            .map(PathBytes::from_vec)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedAsset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub size: u64,
    pub name: String,
    pub paths: Vec<ExportedPath>,
    #[serde(flatten)]
    pub metadata: UserMetadata,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Parses a 64-character hex hash.
#[must_use]
pub fn parse_hash(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

impl ExportedAsset {
    #[must_use]
    pub fn hash_bytes(&self) -> Option<[u8; 32]> {
        self.hash.as_deref().and_then(parse_hash)
    }
}

impl Db {
    /// Collects user metadata for every asset that has any.
    pub fn export_metadata(&self) -> Result<MetadataExport> {
        let library = self.library_uuid()?.to_string();
        let albums = self
            .albums()?
            .into_iter()
            .map(|album| ExportedAlbum {
                name: album.name,
                query: album.query,
            })
            .collect();
        let ids: Vec<AssetId> = {
            let mut statement = self.conn.prepare(
                "SELECT id FROM assets a WHERE favorite = 1 OR rating > 0 OR hidden = 1 OR caption IS NOT NULL \
                 OR rotation != 0 OR taken_source = 6 OR EXISTS (SELECT 1 FROM asset_tags t WHERE t.asset_id = a.id) \
                 OR EXISTS (SELECT 1 FROM album_assets x WHERE x.asset_id = a.id) ORDER BY id",
            )?;
            statement
                .query_map([], |row| row.get(0).map(AssetId))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut assets = Vec::with_capacity(ids.len());
        for id in ids {
            let asset = self.asset(id)?;
            let paths = self
                .files_of(id)?
                .iter()
                .map(|location| ExportedPath::new(&location.source_path, &location.file.relpath))
                .collect();
            let manual_taken: Option<CaptureTime> =
                (asset.taken.source == DateSource::Manual).then_some(asset.taken);
            assets.push(ExportedAsset {
                hash: asset.hash.as_ref().map(|hash| hex(hash)),
                size: asset.size,
                name: asset.name.clone(),
                paths,
                metadata: UserMetadata {
                    favorite: asset.favorite,
                    rating: asset.rating,
                    hidden: asset.hidden,
                    caption: asset.caption.clone(),
                    rotation: asset.rotation,
                    manual_taken,
                    tags: self.tags_of(id)?.into_iter().map(|tag| tag.name).collect(),
                    albums: self
                        .albums_of(id)?
                        .into_iter()
                        .filter(|album| !album.is_smart())
                        .map(|album| album.name)
                        .collect(),
                },
            });
        }
        Ok(MetadataExport {
            format: EXPORT_FORMAT.to_owned(),
            version: EXPORT_VERSION,
            library,
            exported_ms: now_ms(),
            albums,
            assets,
        })
    }

    /// Copies the live catalog into a standalone database file.
    pub fn backup_to(&self, destination: &Path) -> Result<()> {
        let mut target = rusqlite::Connection::open(destination)?;
        {
            let backup = Backup::new(&self.conn, &mut target)?;
            backup.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
        }
        target.execute_batch("PRAGMA journal_mode = DELETE;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_paths_preserve_non_utf8_bytes() {
        let source = PathBytes::from_vec(b"/media/photos".to_vec());
        let raw = PathBytes::from_vec(b"caf\xe9/IMG_1.jpg".to_vec());
        let exported = ExportedPath::new(&source, &raw);
        assert!(exported.relpath.is_none());
        assert_eq!(exported.relpath_bytes(), Some(raw));
        let utf8 = PathBytes::from_vec(b"trip/IMG_1.jpg".to_vec());
        assert_eq!(
            ExportedPath::new(&source, &utf8).relpath.as_deref(),
            Some("trip/IMG_1.jpg")
        );
    }

    #[test]
    fn hashes_round_trip_through_hex() {
        let hash = [0xabu8; 32];
        assert_eq!(parse_hash(&hex(&hash)), Some(hash));
        assert_eq!(parse_hash("zz"), None);
    }
}
