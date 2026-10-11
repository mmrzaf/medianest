//! Copying originals out of the library, and portable metadata.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use medianest_catalog::{EXPORT_FORMAT, MetadataExport};
use medianest_core::{AssetId, PathBytes};
use serde::Serialize;

use crate::events::Event;
use crate::jobs::{JobId, Lane};
use crate::{Error, Library, Result};

/// How to export originals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    pub destination: PathBuf,
    /// Put files in `YYYY/MM` folders instead of one flat folder.
    pub folders_by_date: bool,
    /// Also export the video half of Live Photos.
    pub include_motion: bool,
}

/// Result of applying a metadata export to this library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MetadataImportReport {
    pub entries: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub albums_created: usize,
    pub applied: bool,
}

/// A path in `folder` named like `name` that does not exist yet:
/// `IMG_1.jpg`, `IMG_1 (2).jpg`, `IMG_1 (3).jpg`…
fn free_name(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .unwrap_or(candidate)
}

fn copy_out(source: &Path, target: &Path, control: &medianest_vault::Control) -> Result<u64> {
    let mut input = File::open(source).map_err(Error::io(source))?;
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(Error::io(target))?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        if control.checkpoint().is_err() {
            drop(output);
            let _ = fs::remove_file(target);
            return Err(Error::Cancelled);
        }
        let read = std::io::Read::read(&mut input, &mut buffer).map_err(Error::io(source))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(Error::io(target))?;
        total += read as u64;
        control.report(medianest_vault::Progress::Bytes(read as u64));
    }
    output.sync_all().map_err(Error::io(target))?;
    if let Ok(modified) = fs::metadata(source).and_then(|meta| meta.modified()) {
        let _ = output.set_modified(modified);
    }
    Ok(total)
}

impl Library {
    /// Copies the originals of assets to a folder. Never overwrites files there.
    pub fn export_files(&self, ids: Vec<AssetId>, options: ExportOptions) -> JobId {
        let title = match ids.len() {
            1 => "Exporting 1 item".to_owned(),
            n => format!("Exporting {n} items"),
        };
        self.submit(
            "export",
            title,
            Lane::Foreground,
            move |library, context| {
                fs::create_dir_all(&options.destination)
                    .map_err(Error::io(&options.destination))?;
                let mut work: Vec<(PathBuf, String, Option<i64>)> = Vec::new();
                for id in &ids {
                    let asset = library.read(|db| db.asset(*id))?;
                    let Ok(path) = library.original_path(*id) else {
                        continue;
                    };
                    let name = path.file_name().map_or_else(
                        || asset.name.clone(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    if options.include_motion {
                        if let Ok(Some(video)) = library.motion_video(*id) {
                            let stem = name
                                .rsplit_once('.')
                                .map_or(name.as_str(), |(stem, _)| stem);
                            work.push((video, format!("{stem}.mp4"), asset.taken.local_ms));
                        }
                    }
                    work.push((path, name, asset.taken.local_ms));
                }
                context.set_total(work.len() as u64);
                context.set_bytes_total(
                    work.iter()
                        .filter_map(|(path, _, _)| fs::metadata(path).ok())
                        .map(|meta| meta.len())
                        .sum(),
                );
                let mut exported = 0;
                let mut failed = 0;
                for (path, name, taken) in work {
                    context.checkpoint()?;
                    context.set_current(name.clone());
                    let folder = match (
                        options.folders_by_date,
                        taken.and_then(medianest_core::naive_from_local_ms),
                    ) {
                        (true, Some(local)) => {
                            options.destination.join(local.format("%Y/%m").to_string())
                        }
                        _ => options.destination.clone(),
                    };
                    fs::create_dir_all(&folder).map_err(Error::io(&folder))?;
                    let target = free_name(&folder, &name);
                    match copy_out(&path, &target, context.control()) {
                        Ok(_) => exported += 1,
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(error) => {
                            tracing::warn!(%error, "export failed");
                            failed += 1;
                        }
                    }
                    context.advance();
                }
                Ok(if failed == 0 {
                    format!(
                        "Exported {exported} files to {}",
                        options.destination.display()
                    )
                } else {
                    format!("Exported {exported} files · {failed} could not be copied")
                })
            },
        )
    }

    /// Writes tags, albums, ratings and other edits to a JSON file.
    pub fn export_metadata(&self, path: &Path) -> Result<usize> {
        let document = self.read(medianest_catalog::Db::export_metadata)?;
        let json = serde_json::to_vec_pretty(&document)
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let temporary = path.with_extension("json.partial");
        {
            let mut file = File::create(&temporary).map_err(Error::io(&temporary))?;
            file.write_all(&json).map_err(Error::io(&temporary))?;
            file.sync_all().map_err(Error::io(&temporary))?;
        }
        fs::rename(&temporary, path).map_err(Error::io(path))?;
        Ok(document.assets.len())
    }

    /// Applies a metadata export. With `dry_run`, only reports what would match.
    /// A catalog backup is made before changes are applied.
    pub fn import_metadata(&self, path: &Path, dry_run: bool) -> Result<MetadataImportReport> {
        let bytes = fs::read(path).map_err(Error::io(path))?;
        let document: MetadataExport = serde_json::from_slice(&bytes)
            .map_err(|error| Error::Invalid(format!("not a MediaNest metadata file: {error}")))?;
        if document.format != EXPORT_FORMAT {
            return Err(Error::Invalid("not a MediaNest metadata file".into()));
        }
        if document.version > medianest_catalog::EXPORT_VERSION {
            return Err(Error::Invalid(
                "this file was made by a newer MediaNest".into(),
            ));
        }
        let mut report = MetadataImportReport {
            entries: document.assets.len(),
            ..MetadataImportReport::default()
        };
        let sources = self.sources()?;
        let mut matches = Vec::new();
        for entry in &document.assets {
            let by_hash = match entry.hash_bytes() {
                Some(hash) => self.read(|db| db.asset_by_hash(&hash))?,
                None => None,
            };
            let by_path = || -> Result<Option<AssetId>> {
                for exported in &entry.paths {
                    let Some(relpath) = exported.relpath_bytes() else {
                        continue;
                    };
                    for source in &sources {
                        if source.path.display() == exported.source
                            || PathBytes::from_vec(exported.source.as_bytes().to_vec())
                                == source.path
                        {
                            if let Some(file) =
                                self.read(|db| db.file_by_path(source.id, &relpath))?
                            {
                                return Ok(Some(file.asset_id));
                            }
                        }
                    }
                }
                Ok(None)
            };
            match by_hash.map_or_else(by_path, |id| Ok(Some(id)))? {
                Some(asset) => matches.push((asset, entry.metadata.clone())),
                None => report.unmatched += 1,
            }
        }
        report.matched = matches.len();
        let existing: Vec<String> = self
            .read(|db| db.albums())?
            .into_iter()
            .map(|album| album.name.to_lowercase())
            .collect();
        let new_smart: Vec<_> = document
            .albums
            .iter()
            .filter(|album| album.query.is_some() && !existing.contains(&album.name.to_lowercase()))
            .collect();
        report.albums_created = new_smart.len();
        if dry_run {
            return Ok(report);
        }
        self.backup_now()?;
        for album in new_smart {
            self.write(|writer| writer.create_album(&album.name, album.query.as_deref()))?;
        }
        for (asset, metadata) in &matches {
            self.write(|writer| writer.apply_user_metadata(*asset, metadata))?;
        }
        report.applied = true;
        self.emit(Event::ContentChanged);
        self.emit(Event::TagsChanged);
        self.emit(Event::AlbumsChanged);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_never_collide() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(free_name(dir.path(), "a.jpg"), dir.path().join("a.jpg"));
        fs::write(dir.path().join("a.jpg"), b"x").expect("write");
        assert_eq!(free_name(dir.path(), "a.jpg"), dir.path().join("a (2).jpg"));
        fs::write(dir.path().join("a (2).jpg"), b"x").expect("write");
        assert_eq!(free_name(dir.path(), "a.jpg"), dir.path().join("a (3).jpg"));
        assert_eq!(free_name(dir.path(), "noext"), dir.path().join("noext"));
    }
}
