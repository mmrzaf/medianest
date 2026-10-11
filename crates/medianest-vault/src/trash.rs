//! Moving assets to the system trash and back.
//!
//! MediaNest never deletes a photo outright. "Move to Trash" sends the files
//! to the desktop's trash (the same one the file manager uses), so they can
//! be restored from MediaNest or from the file manager until the trash is
//! emptied.

use std::path::{Path, PathBuf};

use medianest_core::{AssetId, AssetUuid, PathBytes, SourceKind};

use crate::{Result, Vault, VaultError};

/// What to do with files in linked folders when their asset is trashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LinkedPolicy {
    /// Move the file to the system trash too.
    MoveToTrash,
    /// Leave the file alone and stop showing it in MediaNest.
    RemoveFromLibrary,
}

/// What a trash or restore request did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrashReport {
    pub trashed: u32,
    pub restored: u32,
    /// Assets removed from the library without touching their files.
    pub forgotten: Vec<AssetUuid>,
    /// Files that could not be handled, with the reason.
    pub failed: Vec<(String, String)>,
}

fn find_trashed(original: &Path) -> Option<trash::TrashItem> {
    let items = trash::os_limited::list().ok()?;
    items
        .into_iter()
        .filter(|item| item.original_path() == original)
        .max_by_key(|item| item.time_deleted)
}

impl Vault<'_> {
    /// Moves assets to the trash. Linked files follow `linked`.
    pub fn trash_assets(&self, assets: &[AssetId], linked: LinkedPolicy) -> Result<TrashReport> {
        let mut report = TrashReport::default();
        for &asset in assets {
            let files = self.writer().files_of(asset)?;
            let mut trashed_any = false;
            for location in files {
                if location.file.trash_path.is_some() {
                    continue;
                }
                let path = location.path();
                let remove_only = location.source_kind == SourceKind::Linked
                    && linked == LinkedPolicy::RemoveFromLibrary;
                if remove_only {
                    let (source, relpath, file) = (
                        location.file.source_id,
                        location.file.relpath.clone(),
                        location.file.id,
                    );
                    let forgotten = self.write(|writer| {
                        writer.exclude(source, &relpath)?;
                        writer.remove_file(file)
                    })?;
                    if let Some(uuid) = forgotten {
                        report.forgotten.push(uuid);
                    }
                    continue;
                }
                if !path.exists() {
                    continue;
                }
                match trash::delete(&path) {
                    Ok(()) => {
                        let original = PathBytes::from_path(&path);
                        let file = location.file.id;
                        self.write(|writer| writer.set_trash_path(file, Some(&original)))?;
                        trashed_any = true;
                    }
                    Err(error) => report
                        .failed
                        .push((path.display().to_string(), error.to_string())),
                }
            }
            let still_exists = self.writer().try_asset(asset)?.is_some();
            let all_trashed = still_exists
                && self
                    .writer()
                    .files_of(asset)?
                    .iter()
                    .all(|f| f.file.trash_path.is_some());
            if still_exists && (trashed_any || all_trashed) {
                self.write(|writer| writer.set_trashed(&[asset], true))?;
                report.trashed += 1;
            }
        }
        Ok(report)
    }

    /// Brings assets back from the trash, restoring their files.
    pub fn restore_assets(&self, assets: &[AssetId]) -> Result<TrashReport> {
        let mut report = TrashReport::default();
        for &asset in assets {
            let trashed = self.writer().trash_paths(asset)?;
            let mut all_restored = true;
            for (file, original) in trashed {
                let original_path: PathBuf = original.to_path_buf();
                if original_path.exists() {
                    // Something already sits at the original path; do not replace it.
                    if let Some(item) = find_trashed(&original_path) {
                        report.failed.push((
                            original.display(),
                            format!(
                                "a different file now uses this name; “{}” is still in the trash",
                                item.name.to_string_lossy()
                            ),
                        ));
                        all_restored = false;
                        continue;
                    }
                    self.write(|writer| writer.set_trash_path(file, None))?;
                    continue;
                }
                match find_trashed(&original_path) {
                    Some(item) => match trash::os_limited::restore_all([item]) {
                        Ok(()) => self.write(|writer| writer.set_trash_path(file, None))?,
                        Err(error) => {
                            report.failed.push((original.display(), error.to_string()));
                            all_restored = false;
                        }
                    },
                    None => {
                        report
                            .failed
                            .push((original.display(), "no longer in the trash".into()));
                        all_restored = false;
                    }
                }
            }
            if all_restored {
                self.write(|writer| writer.set_trashed(&[asset], false))?;
                report.restored += 1;
            }
        }
        Ok(report)
    }

    /// Forgets trashed assets whose files are no longer in the system trash
    /// (the user emptied it). Returns their uuids for cache cleanup.
    pub fn purge_emptied_trash(&self) -> Result<Vec<AssetUuid>> {
        let trashed = self.writer().trashed_before(i64::MAX)?;
        if trashed.is_empty() {
            return Ok(Vec::new());
        }
        let items =
            trash::os_limited::list().map_err(|error| VaultError::Trash(error.to_string()))?;
        let in_trash: std::collections::HashSet<PathBuf> =
            items.iter().map(trash::TrashItem::original_path).collect();
        let mut forgotten = Vec::new();
        for asset in trashed {
            let paths = self.writer().trash_paths(asset)?;
            let gone = !paths.is_empty()
                && paths.iter().all(|(_, original)| {
                    let path = original.to_path_buf();
                    !in_trash.contains(&path) && !path.exists()
                });
            if gone {
                forgotten.push(self.write(|writer| writer.forget_asset(asset))?);
            }
        }
        Ok(forgotten)
    }
}
