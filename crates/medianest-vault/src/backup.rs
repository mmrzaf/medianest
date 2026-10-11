//! Catalog backups.
//!
//! Backups hold the catalog only: tags, albums, dates and everything else
//! people add. They do not replace backing up the photos themselves.

use std::fs;
use std::path::{Path, PathBuf};

use medianest_catalog::Db;

use crate::fsops::sync_dir;
use crate::layout::Layout;
use crate::{Result, VaultError};

/// A catalog backup on disk.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub created_ms: i64,
    pub size: u64,
}

/// Writes a consistent copy of the catalog into the library's backup folder.
pub fn create_backup(db: &Db, layout: &Layout) -> Result<PathBuf> {
    let folder = layout.backups();
    fs::create_dir_all(&folder).map_err(VaultError::io("could not create", &folder))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%3f");
    let target = folder.join(format!("catalog-{stamp}.sqlite3"));
    let partial = folder.join(format!(".catalog-{stamp}.partial"));
    db.backup_to(&partial)?;
    fs::File::open(&partial)
        .and_then(|file| file.sync_all())
        .map_err(VaultError::io("could not flush", &partial))?;
    fs::rename(&partial, &target).map_err(VaultError::io("could not finish", &target))?;
    sync_dir(&folder)?;
    Ok(target)
}

/// Backups, newest first.
pub fn list_backups(layout: &Layout) -> Result<Vec<BackupInfo>> {
    let folder = layout.backups();
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(VaultError::io("could not read", &folder)(error)),
    };
    let mut backups: Vec<BackupInfo> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("catalog-") && name.ends_with(".sqlite3")
        })
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let created_ms = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())?;
            Some(BackupInfo {
                path: entry.path(),
                created_ms,
                size: meta.len(),
            })
        })
        .collect();
    backups.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| b.path.cmp(&a.path))
    });
    Ok(backups)
}

/// Deletes all but the `keep` newest backups.
pub fn prune_backups(layout: &Layout, keep: usize) -> Result<usize> {
    let mut removed = 0;
    for backup in list_backups(layout)?.into_iter().skip(keep) {
        if fs::remove_file(&backup.path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Replaces the catalog with a backup. The library must be closed.
///
/// The current catalog is kept next to it as `catalog.before-restore-…`, so a
/// restore can itself be undone.
pub fn restore_backup(layout: &Layout, backup: &Path) -> Result<PathBuf> {
    let check = Db::open_read_only(backup)?;
    if check.library_uuid()? != {
        let current = Db::open_read_only(&layout.catalog())?;
        current.library_uuid()?
    } {
        return Err(VaultError::NotALibrary(backup.to_path_buf()));
    }
    drop(check);
    let catalog = layout.catalog();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let saved = layout
        .internal()
        .join(format!("catalog.before-restore-{stamp}.sqlite3"));
    {
        let current = Db::open_read_only(&catalog)?;
        current.backup_to(&saved)?;
    }
    let incoming = layout.internal().join(".catalog.restoring");
    fs::copy(backup, &incoming).map_err(VaultError::io("could not copy", backup))?;
    fs::File::open(&incoming)
        .and_then(|file| file.sync_all())
        .map_err(VaultError::io("could not flush", &incoming))?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", catalog.display()));
        let _ = fs::remove_file(side);
    }
    fs::rename(&incoming, &catalog).map_err(VaultError::io("could not replace", &catalog))?;
    sync_dir(&layout.internal())?;
    Ok(saved)
}
