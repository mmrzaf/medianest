//! Planning and running imports into the managed folder.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use medianest_catalog::ReviewSubject;
use medianest_core::{AssetMetadata, MediaKind, PathBytes, ReviewKind};
use medianest_media::{ProbeError, has_media_extension};
use medianest_vault::{
    Control, ImportMode, ImportOutcome, VaultError, is_ignored_name, managed_relpath,
};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::events::Event;
use crate::jobs::{JobId, Lane};
use crate::{Error, Library, Result};

/// What an import would do with one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ImportItemState {
    New,
    /// The same content is already in the library.
    AlreadyInLibrary,
    /// Another file in this selection has the same content.
    DuplicateInSelection,
    Unreadable(String),
}

/// One file considered for import.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportItem {
    pub path: PathBuf,
    pub size: u64,
    pub metadata: Option<AssetMetadata>,
    pub state: ImportItemState,
    /// Where it would go inside `Media/`.
    pub destination: Option<PathBytes>,
    /// The original is on the same filesystem as the library, so moving it
    /// frees the space it takes there.
    pub on_library_volume: bool,
    /// A move could remove the original afterwards.
    pub source_removable: bool,
}

/// Everything the import dialog shows before anything is copied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportPlan {
    pub items: Vec<ImportItem>,
    pub new_count: u64,
    pub new_bytes: u64,
    pub photos: u64,
    pub videos: u64,
    pub already_in_library: u64,
    pub duplicates_in_selection: u64,
    pub unreadable: u64,
    /// Files that are not photos or videos, left alone.
    pub other_files: u64,
    /// New items whose date had to be guessed; they go to `Unsorted/`.
    pub uncertain_dates: u64,
    /// Earliest and latest capture times among new items (local wall time).
    pub date_range: Option<(i64, i64)>,
    /// New items per (year, month).
    pub months: Vec<(i32, u32, u64)>,
}

/// Whether an import fits on the library's disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceCheck {
    /// Bytes that must be free, including the reserve for catalog and thumbnails.
    pub required: u64,
    pub available: u64,
    /// Bytes on the library's own disk whose originals a move cannot remove,
    /// so they take space twice.
    pub unremovable_bytes: u64,
}

impl SpaceCheck {
    #[must_use]
    pub fn is_short(&self) -> bool {
        self.available < self.required
    }

    /// What an import of `items` needs on the library's disk, given `available`.
    ///
    /// Moving only helps for originals on that same disk: each is removed
    /// right after its copy, so just the largest is ever there twice.
    /// Originals elsewhere free space elsewhere.
    #[must_use]
    pub fn for_items(items: &[ImportItem], mode: ImportMode, available: u64) -> Self {
        let new = items
            .iter()
            .filter(|item| item.state == ImportItemState::New);
        let sum = |items: &[&ImportItem]| items.iter().map(|item| item.size).sum::<u64>();
        let (needed, unremovable_bytes) = match mode {
            ImportMode::Copy => (sum(&new.collect::<Vec<_>>()), 0),
            ImportMode::Move => {
                let (local, elsewhere): (Vec<&ImportItem>, Vec<&ImportItem>) =
                    new.partition(|item| item.on_library_volume);
                let (removable, kept): (Vec<&ImportItem>, Vec<&ImportItem>) =
                    local.into_iter().partition(|item| item.source_removable);
                let largest = removable.iter().map(|item| item.size).max().unwrap_or(0);
                (sum(&elsewhere) + sum(&kept) + largest, sum(&kept))
            }
        };
        Self {
            required: needed.saturating_add(medianest_vault::SPACE_RESERVE),
            available,
            unremovable_bytes,
        }
    }
}

/// How to run an import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOptions {
    pub mode: ImportMode,
}

fn collect_candidates(
    paths: &[PathBuf],
    control: &Control,
    other_files: &mut u64,
) -> Result<Vec<(PathBuf, u64)>> {
    let mut found = Vec::new();
    for root in paths {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_ignored_name(entry.file_name()));
        for entry in walker {
            control.checkpoint().map_err(|_| Error::Cancelled)?;
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_file() {
                continue;
            }
            if !has_media_extension(entry.path()) {
                let sidecar = entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "xmp" | "aae" | "thm" | "lrv" | "json"
                        )
                    });
                if !sidecar {
                    *other_files += 1;
                }
                continue;
            }
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            found.push((entry.path().to_path_buf(), size));
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

impl Library {
    /// Examines files and folders and describes what importing them would do.
    /// Nothing is copied. `progress` receives (examined, total) counts.
    pub fn plan_import(
        &self,
        paths: &[PathBuf],
        control: &Control,
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> Result<ImportPlan> {
        let mut plan = ImportPlan::default();
        let library_device = std::fs::metadata(self.layout().media())
            .ok()
            .map(|meta| meta.dev());
        let candidates = collect_candidates(paths, control, &mut plan.other_files)?;
        for path in paths {
            if self
                .layout()
                .contains(&path.canonicalize().unwrap_or_else(|_| path.clone()))
            {
                return Err(Error::Vault(VaultError::InsideLibrary(path.clone())));
            }
        }
        let total = candidates.len() as u64;
        progress(0, total);
        let examined = std::sync::atomic::AtomicU64::new(0);
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
        let chunk = candidates.len().div_ceil(threads).max(1);
        let probed: Vec<(PathBuf, u64, std::result::Result<AssetMetadata, ProbeError>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = candidates
                    .chunks(chunk)
                    .map(|part| {
                        let examined = &examined;
                        scope.spawn(move || {
                            part.iter()
                                .map_while(|(path, size)| {
                                    if control.is_cancelled() {
                                        return None;
                                    }
                                    let result = self.probe(path);
                                    let done = examined
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                        + 1;
                                    if done % 16 == 0 || done == total {
                                        progress(done, total);
                                    }
                                    Some((path.clone(), *size, result))
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
        control.checkpoint().map_err(|_| Error::Cancelled)?;

        // Content hashes, computed only where a size collision makes a duplicate possible.
        let mut by_size: HashMap<u64, usize> = HashMap::new();
        for (_, size, _) in &probed {
            *by_size.entry(*size).or_default() += 1;
        }
        let mut seen_hashes: HashMap<[u8; 32], PathBuf> = HashMap::new();
        let mut months: HashMap<(i32, u32), u64> = HashMap::new();
        for (path, size, result) in probed {
            control.checkpoint().map_err(|_| Error::Cancelled)?;
            let metadata = match result {
                Ok(metadata) => metadata,
                Err(ProbeError::Unsupported) => {
                    plan.other_files += 1;
                    continue;
                }
                Err(error) => {
                    plan.unreadable += 1;
                    plan.items.push(ImportItem {
                        path,
                        size,
                        metadata: None,
                        state: ImportItemState::Unreadable(error.to_string()),
                        destination: None,
                        on_library_volume: false,
                        source_removable: false,
                    });
                    continue;
                }
            };
            let library_candidates = self.read(|db| db.files_with_size(size))?;
            let mut state = ImportItemState::New;
            let needs_hash =
                !library_candidates.is_empty() || by_size.get(&size).copied().unwrap_or(0) > 1;
            if needs_hash {
                if let Ok((hash, _)) = medianest_vault::hash_file(&path, control) {
                    let in_library = library_candidates
                        .iter()
                        .any(|location| location.file.hash == Some(hash))
                        || self.read(|db| db.asset_by_hash(&hash))?.is_some();
                    if in_library {
                        state = ImportItemState::AlreadyInLibrary;
                    } else if let std::collections::hash_map::Entry::Vacant(slot) =
                        seen_hashes.entry(hash)
                    {
                        slot.insert(path.clone());
                    } else {
                        state = ImportItemState::DuplicateInSelection;
                    }
                }
            }
            let name = path.file_name().unwrap_or_default().to_owned();
            let destination = Some(managed_relpath(&metadata, &name));
            match state {
                ImportItemState::New => {
                    plan.new_count += 1;
                    plan.new_bytes += size;
                    match metadata.kind {
                        MediaKind::Photo => plan.photos += 1,
                        MediaKind::Video => plan.videos += 1,
                    }
                    if !metadata.taken.is_trustworthy() {
                        plan.uncertain_dates += 1;
                    }
                    if let Some(local) = metadata
                        .taken
                        .local_ms
                        .filter(|_| metadata.taken.is_trustworthy())
                    {
                        plan.date_range = Some(match plan.date_range {
                            Some((low, high)) => (low.min(local), high.max(local)),
                            None => (local, local),
                        });
                        if let Some((year, month)) =
                            metadata.taken.day().and_then(|day| day.year_month())
                        {
                            *months.entry((year, month)).or_default() += 1;
                        }
                    }
                }
                ImportItemState::AlreadyInLibrary => plan.already_in_library += 1,
                ImportItemState::DuplicateInSelection => plan.duplicates_in_selection += 1,
                ImportItemState::Unreadable(_) => plan.unreadable += 1,
            }
            let on_library_volume = library_device.is_some_and(|device| {
                std::fs::metadata(&path).is_ok_and(|meta| meta.dev() == device)
            });
            let source_removable = path.parent().is_some_and(medianest_vault::can_remove_from);
            plan.items.push(ImportItem {
                path,
                size,
                metadata: Some(metadata),
                state,
                destination,
                on_library_volume,
                source_removable,
            });
        }
        let mut months: Vec<(i32, u32, u64)> =
            months.into_iter().map(|((y, m), n)| (y, m, n)).collect();
        months.sort_unstable();
        plan.months = months;
        Ok(plan)
    }

    /// Checks that `plan` fits on the library's disk when run in `mode`.
    pub fn space_check(&self, plan: &ImportPlan, mode: ImportMode) -> Result<SpaceCheck> {
        let available = medianest_vault::available_space(&self.layout().media())?;
        Ok(SpaceCheck::for_items(&plan.items, mode, available))
    }

    /// Imports the new items of a plan. Returns the job doing it.
    pub fn import(&self, plan: ImportPlan, options: ImportOptions) -> JobId {
        let items: Vec<ImportItem> = plan
            .items
            .into_iter()
            .filter(|item| {
                item.state == ImportItemState::New
                    || (options.mode == ImportMode::Move
                        && item.state == ImportItemState::AlreadyInLibrary)
            })
            .collect();
        let count = items.len();
        let title = match (options.mode, count) {
            (ImportMode::Copy, 1) => "Importing 1 item".to_owned(),
            (ImportMode::Copy, n) => format!("Importing {n} items"),
            (ImportMode::Move, 1) => "Moving 1 item into the library".to_owned(),
            (ImportMode::Move, n) => format!("Moving {n} items into the library"),
        };
        self.submit(
            "import",
            title,
            Lane::Foreground,
            move |library, context| {
                context.set_total(items.len() as u64);
                context.set_bytes_total(items.iter().map(|item| item.size).sum());
                let batch = medianest_core::OperationUuid::new().to_string();
                let (mut imported, mut existing, mut failed, mut awaiting) =
                    (0u64, 0u64, 0u64, 0u64);
                let mut last_refresh = std::time::Instant::now();
                for item in &items {
                    context.checkpoint()?;
                    context.set_current(
                        item.path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                    let metadata = match &item.metadata {
                        Some(metadata) => metadata.clone(),
                        None => match library.probe(&item.path) {
                            Ok(metadata) => metadata,
                            Err(error) => {
                                library.record_failed_import(&item.path, &error.to_string())?;
                                failed += 1;
                                context.advance();
                                continue;
                            }
                        },
                    };
                    match library.vault().import_file(
                        &item.path,
                        &metadata,
                        options.mode,
                        Some(&batch),
                        context.control(),
                    ) {
                        Ok(ImportOutcome::Imported {
                            awaiting_removal, ..
                        }) => {
                            imported += 1;
                            awaiting += u64::from(awaiting_removal.is_some());
                            library.close_failed_import(&item.path)?;
                        }
                        Ok(ImportOutcome::AlreadyInLibrary {
                            awaiting_removal, ..
                        }) => {
                            existing += 1;
                            awaiting += u64::from(awaiting_removal.is_some());
                        }
                        Err(VaultError::Cancelled) => return Err(Error::Cancelled),
                        Err(VaultError::NoSpace(_)) => {
                            library.poke_thumbnails();
                            library.emit(Event::ContentChanged);
                            return Err(Error::DiskFull {
                                done: imported + existing + failed,
                                total: items.len(),
                            });
                        }
                        Err(error) => {
                            tracing::warn!(path = %item.path.display(), %error, "import failed");
                            library.record_failed_import(&item.path, &error.to_string())?;
                            failed += 1;
                        }
                    }
                    context.advance();
                    if last_refresh.elapsed().as_secs() >= 2 {
                        last_refresh = std::time::Instant::now();
                        library.poke_thumbnails();
                        library.emit(Event::ContentChanged);
                    }
                }
                library.poke_thumbnails();
                library.emit(Event::ContentChanged);
                library.emit(Event::ReviewChanged);
                let mut parts = vec![match imported {
                    1 => "Imported 1 item".to_owned(),
                    n => format!("Imported {n} items"),
                }];
                if existing > 0 {
                    parts.push(format!("{existing} already in the library"));
                }
                if failed > 0 {
                    parts.push(format!("{failed} could not be imported"));
                }
                if awaiting > 0 {
                    parts.push(format!("{awaiting} originals were kept for your review"));
                }
                Ok(parts.join(" · "))
            },
        )
    }

    fn record_failed_import(&self, path: &Path, reason: &str) -> Result<()> {
        let bytes = PathBytes::from_path(path);
        self.write(|writer| {
            writer.open_review(
                ReviewKind::FailedImport,
                ReviewSubject::Path(&bytes),
                &serde_json::json!({ "path": path.display().to_string(), "reason": reason }),
            )
        })?;
        Ok(())
    }

    fn close_failed_import(&self, path: &Path) -> Result<()> {
        let bytes = PathBytes::from_path(path);
        self.write(|writer| writer.close_path_reviews(ReviewKind::FailedImport, &bytes))
    }

    /// Imports a single file again, for example from the review inbox.
    pub fn import_one(&self, path: &Path, mode: ImportMode) -> Result<ImportOutcome> {
        let metadata = self.probe(path)?;
        let outcome = self
            .vault()
            .import_file(path, &metadata, mode, None, &Control::new());
        match &outcome {
            Ok(_) => self.close_failed_import(path)?,
            Err(error) => self.record_failed_import(path, &error.to_string())?,
        }
        self.poke_thumbnails();
        self.emit(Event::ContentChanged);
        self.emit(Event::ReviewChanged);
        Ok(outcome?)
    }
}
