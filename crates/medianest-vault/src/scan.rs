//! Reconciling a linked folder with the catalog.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use medianest_catalog::{FileRecord, NewFile, ReviewSubject};
use medianest_core::{
    AssetMetadata, FileStamp, FileStatus, PathBytes, ReviewKind, SourceId, SourceKind,
    SourceStatus, ThumbState,
};
use medianest_media::{ProbeError, has_media_extension};
use walkdir::WalkDir;

use crate::control::{Control, Progress};
use crate::fsops::hash_file;
use crate::{Result, Vault, VaultError};

/// Files catalogued per transaction during a scan.
const BATCH: usize = 256;

/// What a scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanReport {
    /// Media files seen on disk.
    pub found: u64,
    pub added: u64,
    /// Existing files whose size or date changed.
    pub modified: u64,
    /// Files recognised at a new path.
    pub moved: u64,
    /// Catalogued files no longer on disk.
    pub missing: u64,
    /// Files that reappeared after being missing.
    pub restored: u64,
    pub unchanged: u64,
    /// Files that could not be read, with the reason.
    pub failed: Vec<(String, String)>,
    /// The folder could not be reached; nothing was changed.
    pub offline: bool,
}

/// Folder and file names skipped while scanning: hidden entries, MediaNest's
/// own temporary files and system clutter from NAS boxes, Windows and macOS.
#[must_use]
pub fn is_ignored_name(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes.starts_with(b".")
        || matches!(
            bytes,
            b"@eaDir"
                | b"#recycle"
                | b"#snapshot"
                | b"$RECYCLE.BIN"
                | b"System Volume Information"
                | b"lost+found"
        )
}

struct Seen {
    relpath: PathBytes,
    path: PathBuf,
    stamp: FileStamp,
}

impl Vault<'_> {
    /// Brings the catalog in line with a linked folder's contents.
    ///
    /// Unchanged files cost a `stat` and nothing else. New files are probed in
    /// parallel and catalogued in batches. A file that disappeared from one
    /// path and appeared at another with the same content keeps its tags and
    /// albums. If the folder is unreachable, or suddenly empty while the
    /// catalog knows files in it (an unmounted drive), nothing is marked
    /// missing.
    pub fn scan_source(
        &self,
        source_id: SourceId,
        probe: &(dyn Fn(&Path) -> std::result::Result<AssetMetadata, ProbeError> + Sync),
        control: &Control,
    ) -> Result<ScanReport> {
        let source = self.writer().source(source_id)?;
        if source.kind != SourceKind::Linked {
            return Err(VaultError::InvalidState);
        }
        let root = source.path.to_path_buf();
        let mut report = ScanReport::default();
        if !root.is_dir() {
            self.write(|writer| writer.set_source_status(source_id, SourceStatus::Offline))?;
            report.offline = true;
            return Ok(report);
        }
        let known: HashMap<Vec<u8>, FileRecord> = self
            .writer()
            .source_files(source_id)?
            .into_iter()
            .map(|record| (record.relpath.as_bytes().to_vec(), record))
            .collect();
        let excluded = self.writer().exclusions(source_id)?;

        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut new_files: Vec<Seen> = Vec::new();
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_ignored_name(entry.file_name()));
        for entry in walker {
            control.checkpoint()?;
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_file() || !has_media_extension(entry.path()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&root) else {
                continue;
            };
            let relpath = PathBytes::from_path(relative);
            if excluded.contains(relpath.as_bytes()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let stamp = FileStamp::from_metadata(&metadata);
            report.found += 1;
            seen.insert(relpath.as_bytes().to_vec());
            match known.get(relpath.as_bytes()) {
                Some(record) if record.trash_path.is_some() => {
                    // A trashed file is back where it was; restore it.
                    let (file, asset) = (record.id, record.asset_id);
                    self.write(|writer| {
                        writer.set_trash_path(file, None)?;
                        writer.set_trashed(&[asset], false)
                    })?;
                    report.restored += 1;
                }
                Some(record) if stamp.looks_unmodified(&record.stamp) => {
                    if record.status == FileStatus::Missing {
                        let status = if record.hash.is_some() {
                            FileStatus::Verified
                        } else {
                            FileStatus::Unverified
                        };
                        let file = record.id;
                        self.write(|writer| {
                            writer.update_file_stamp(file, &stamp, status)?;
                            writer.set_file_status(file, status)
                        })?;
                        report.restored += 1;
                    } else {
                        report.unchanged += 1;
                    }
                }
                Some(record) => {
                    // Modified in place. Hashing later decides whether the
                    // content really changed; until then it is unverified.
                    let (file, asset, had_hash) =
                        (record.id, record.asset_id, record.hash.is_some());
                    self.write(|writer| {
                        writer.update_file_stamp(file, &stamp, FileStatus::Unverified)
                    })?;
                    if !had_hash {
                        if let Ok(metadata) = probe(entry.path()) {
                            self.write(|writer| {
                                writer.refresh_metadata(asset, &metadata)?;
                                writer.set_thumbnail(asset, ThumbState::Pending, None, None)
                            })?;
                        }
                    }
                    report.modified += 1;
                }
                None => new_files.push(Seen {
                    relpath,
                    path: entry.path().to_path_buf(),
                    stamp,
                }),
            }
        }

        let gone: Vec<&FileRecord> = known
            .values()
            .filter(|record| {
                !seen.contains(record.relpath.as_bytes()) && record.trash_path.is_none()
            })
            .collect();
        if report.found == 0 && !gone.is_empty() {
            // An empty mount point: treat the source as offline.
            self.write(|writer| writer.set_source_status(source_id, SourceStatus::Offline))?;
            return Ok(ScanReport {
                offline: true,
                ..ScanReport::default()
            });
        }

        // Recognise moves and renames before cataloguing anything new.
        let mut claimed: HashSet<i64> = HashSet::new();
        let mut truly_new = Vec::with_capacity(new_files.len());
        for candidate in new_files {
            let mut matched = None;
            for record in gone.iter().filter(|record| {
                record.stamp.size == candidate.stamp.size && !claimed.contains(&record.id.get())
            }) {
                let same_name =
                    record.relpath.as_path().file_name() == candidate.relpath.as_path().file_name();
                let same_object = record.stamp.device == candidate.stamp.device
                    && record.stamp.inode == candidate.stamp.inode;
                if record.stamp.mtime_ns == candidate.stamp.mtime_ns && (same_object || same_name) {
                    matched = Some(record.id);
                    break;
                }
                if let Some(hash) = record.hash {
                    if hash_file(&candidate.path, control).is_ok_and(|(found, _)| found == hash) {
                        matched = Some(record.id);
                        break;
                    }
                }
            }
            match matched {
                Some(file) => {
                    claimed.insert(file.get());
                    self.write(|writer| {
                        writer.move_file(file, &candidate.relpath, &candidate.stamp)
                    })?;
                    report.moved += 1;
                }
                None => truly_new.push(candidate),
            }
        }

        for record in gone
            .iter()
            .filter(|record| !claimed.contains(&record.id.get()))
        {
            if record.status != FileStatus::Missing {
                let file = record.id;
                self.write(|writer| writer.set_file_status(file, FileStatus::Missing))?;
            }
            report.missing += 1;
        }

        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
        for chunk in truly_new.chunks(BATCH) {
            control.checkpoint()?;
            let probed: Vec<(usize, std::result::Result<AssetMetadata, ProbeError>)> =
                std::thread::scope(|scope| {
                    let per_thread = chunk.len().div_ceil(threads);
                    let handles: Vec<_> = chunk
                        .chunks(per_thread.max(1))
                        .enumerate()
                        .map(|(part, files)| {
                            scope.spawn(move || {
                                files
                                    .iter()
                                    .enumerate()
                                    .map(|(index, file)| {
                                        control.report(Progress::Item {
                                            label: file.relpath.file_name_lossy(),
                                        });
                                        (part * per_thread + index, probe(&file.path))
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|handle| handle.join().unwrap_or_default())
                        .collect()
                });
            let mut items = Vec::with_capacity(chunk.len());
            for (index, result) in probed {
                let Some(file) = chunk.get(index) else {
                    continue;
                };
                match result {
                    Ok(metadata) => items.push((
                        NewFile {
                            source: source_id,
                            relpath: &file.relpath,
                            stamp: file.stamp,
                            status: FileStatus::Unverified,
                            hash: None,
                        },
                        file.relpath.file_name_lossy(),
                        metadata,
                    )),
                    Err(ProbeError::Unsupported) => {}
                    Err(error) => {
                        let path = PathBytes::from_path(&file.path);
                        let message = error.to_string();
                        self.write(|writer| {
                            writer.open_review(
                                ReviewKind::Unreadable,
                                ReviewSubject::Path(&path),
                                &serde_json::json!({ "path": path.display(), "reason": message }),
                            )
                        })?;
                        report
                            .failed
                            .push((file.relpath.display(), error.to_string()));
                    }
                }
                control.report(Progress::Done);
            }
            let added = self.write(|writer| writer.add_files(&items))?;
            report.added += added.len() as u64;
        }

        self.write(|writer| writer.mark_scanned(source_id))?;
        Ok(report)
    }
}

/// Folders that must not be added as sources: the library itself, a folder
/// inside it, or a folder containing it.
pub(crate) fn check_source_folder(library_root: &Path, folder: &Path) -> Result<PathBuf> {
    let folder = folder
        .canonicalize()
        .map_err(VaultError::io("could not open", folder))?;
    if !folder.is_dir() {
        return Err(VaultError::NotAFolder(folder));
    }
    if folder.starts_with(library_root) {
        return Err(VaultError::InsideLibrary(folder));
    }
    if library_root.starts_with(&folder) {
        return Err(VaultError::ContainsLibrary(folder));
    }
    Ok(folder)
}

impl Vault<'_> {
    /// Registers a linked folder. Call [`Vault::scan_source`] next to index it.
    pub fn add_source(&self, folder: &Path) -> Result<SourceId> {
        let folder = check_source_folder(self.layout.root(), folder)?;
        let name = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let existing = self.writer().source_by_path(&folder)?;
        if let Some(existing) = existing {
            return Ok(existing.id);
        }
        self.write(|writer| writer.add_source(&folder, &name))
    }

    /// Points a linked source at the folder's new location after it moved.
    pub fn relocate_source(&self, source: SourceId, folder: &Path) -> Result<()> {
        let folder = check_source_folder(self.layout.root(), folder)?;
        self.write(|writer| {
            writer.relocate_source(source, &folder)?;
            writer.set_source_status(source, SourceStatus::Available)
        })
    }
}
