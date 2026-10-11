//! Verification, hashing, backups and housekeeping, scheduled quietly.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use medianest_core::{FileId, FileStatus, SourceKind, now_ms};
use medianest_vault::{BackupInfo, OpenedLibrary, VerifyOutcome};
use serde::{Deserialize, Serialize};

use crate::events::Event;
use crate::jobs::{JobId, Lane};
use crate::{Inner, Library, Result};

const DAY_MS: i64 = 86_400_000;

/// How the library looks after itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Rescan linked folders when their contents change.
    pub watch_folders: bool,
    /// Re-check every file's content this often; 0 turns it off.
    pub verify_every_days: u32,
    /// Back up the catalog this often; 0 turns it off.
    pub backup_every_hours: u32,
    /// How many catalog backups to keep.
    pub keep_backups: usize,
    /// Upper bound for cached viewer previews.
    pub preview_cache_mb: u64,
    /// Hash newly found files in linked folders in the background.
    pub background_hashing: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            watch_folders: true,
            verify_every_days: 30,
            backup_every_hours: 24,
            keep_backups: 14,
            preview_cache_mb: 1024,
            background_hashing: true,
        }
    }
}

pub(crate) struct Scheduler {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Scheduler {
    pub(crate) fn start(library: &Library) -> Self {
        let (stop, stopped) = channel::<()>();
        let weak: Weak<Inner> = Arc::downgrade(&library.inner);
        let thread = std::thread::Builder::new()
            .name("maintenance".into())
            .spawn(move || {
                // Let the interface settle before doing anything.
                let mut wait = Duration::from_secs(15);
                loop {
                    if stopped.recv_timeout(wait).is_ok() {
                        return;
                    }
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let library = Library { inner };
                    if library.is_closed() {
                        return;
                    }
                    library.tick();
                    drop(library);
                    wait = Duration::from_secs(30 * 60);
                }
            })
            .ok();
        Self { stop, thread }
    }

    pub(crate) fn stop(mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

impl Library {
    /// One round of housekeeping. Every step is cheap when there is nothing to do.
    pub(crate) fn tick(&self) {
        let settings = self.settings();
        match self.vault().purge_emptied_trash() {
            Ok(forgotten) if !forgotten.is_empty() => {
                for uuid in &forgotten {
                    self.inner.cache.remove(*uuid);
                }
                self.emit(Event::ContentChanged);
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "could not check the trash"),
        }
        if settings.backup_every_hours > 0 {
            let newest = medianest_vault::list_backups(self.layout())
                .ok()
                .and_then(|backups| backups.first().map(|backup| backup.created_ms));
            let due = newest.is_none_or(|created| {
                now_ms() - created > i64::from(settings.backup_every_hours) * 3_600_000
            });
            if due {
                if let Err(error) = self.backup_now() {
                    tracing::warn!(%error, "scheduled catalog backup failed");
                }
            }
        }
        if settings.background_hashing {
            self.schedule_hashing();
        }
        if settings.verify_every_days > 0 {
            let cutoff = now_ms() - i64::from(settings.verify_every_days) * DAY_MS;
            let stale = self
                .read(|db| db.files_to_verify(None, cutoff, FileId(0), 1))
                .is_ok_and(|files| {
                    files
                        .iter()
                        .any(|f| f.file.status != FileStatus::Unverified)
                });
            if stale && !self.job_running("verify") {
                self.verify(None, Some(cutoff), Lane::Background);
            }
        }
        self.name_places(2_000);
        let limit = settings.preview_cache_mb.saturating_mul(1024 * 1024);
        self.inner.cache.trim_previews(limit);
        let _ = self.write(|writer| writer.prune_operations(now_ms() - 90 * DAY_MS));
    }

    fn job_running(&self, kind: &str) -> bool {
        self.jobs()
            .iter()
            .any(|job| job.kind == kind && !job.state.is_finished())
    }

    /// Hashes linked files that have not been verified yet, in the background.
    pub fn schedule_hashing(&self) {
        if self.job_running("hash") {
            return;
        }
        let pending = self
            .read(|db| db.files_with_status(FileStatus::Unverified, FileId(0), 1))
            .is_ok_and(|files| !files.is_empty());
        if !pending {
            return;
        }
        let _job = self.submit(
            "hash",
            "Checking new files",
            Lane::Background,
            |library, context| {
                let total = library.read(|db| Ok(db.stats()?.unverified_files))?;
                context.set_total(total);
                let (mut checked, mut merged) = (0u64, 0u64);
                let mut after = FileId(0);
                loop {
                    context.checkpoint()?;
                    let batch = library
                        .read(|db| db.files_with_status(FileStatus::Unverified, after, 200))?;
                    let Some(last) = batch.last() else {
                        break;
                    };
                    after = last.file.id;
                    for location in batch {
                        context.checkpoint()?;
                        match library.vault().verify_file(&location, context.control()) {
                            Ok(VerifyOutcome::Merged { .. }) => merged += 1,
                            Ok(_) => {}
                            Err(medianest_vault::VaultError::Cancelled) => {
                                return Err(crate::Error::Cancelled);
                            }
                            Err(error) => tracing::warn!(%error, "could not check a file"),
                        }
                        checked += 1;
                        context.advance();
                    }
                }
                if merged > 0 {
                    library.emit(Event::ContentChanged);
                }
                library.emit(Event::ReviewChanged);
                Ok(match merged {
                    0 => format!("Checked {checked} files"),
                    n => format!("Checked {checked} files · {n} duplicates combined"),
                })
            },
        );
    }

    /// Re-reads files and compares them with their recorded content.
    ///
    /// `kind` limits the check to managed or linked files; `older_than_ms`
    /// skips files checked more recently than that time.
    pub fn verify(
        &self,
        kind: Option<SourceKind>,
        older_than_ms: Option<i64>,
        lane: Lane,
    ) -> JobId {
        let title = match kind {
            Some(SourceKind::Managed) => "Verifying library files",
            Some(SourceKind::Linked) => "Verifying linked folders",
            None => "Verifying all files",
        };
        self.submit("verify", title, lane, move |library, context| {
            let started = now_ms();
            let cutoff = older_than_ms.unwrap_or(started);
            let stats = library.read(medianest_catalog::Db::stats)?;
            context.set_total(match kind {
                Some(SourceKind::Managed) => stats.managed_files,
                Some(SourceKind::Linked) => stats.linked_files,
                None => stats.files,
            });
            let (mut verified, mut missing, mut changed, mut skipped) = (0u64, 0u64, 0u64, 0u64);
            let mut after = FileId(0);
            loop {
                context.checkpoint()?;
                let batch = library.read(|db| db.files_to_verify(kind, cutoff, after, 200))?;
                let Some(last) = batch.last() else {
                    break;
                };
                after = last.file.id;
                for location in batch {
                    match library.vault().verify_file(&location, context.control()) {
                        Ok(VerifyOutcome::Verified | VerifyOutcome::Merged { .. }) => verified += 1,
                        Ok(VerifyOutcome::Missing) => missing += 1,
                        Ok(VerifyOutcome::Changed) => changed += 1,
                        Ok(VerifyOutcome::Skipped) => skipped += 1,
                        Err(medianest_vault::VaultError::Cancelled) => {
                            return Err(crate::Error::Cancelled);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "verification error");
                            skipped += 1;
                        }
                    }
                    context.advance();
                }
            }
            library.emit(Event::ReviewChanged);
            if missing + changed > 0 {
                library.emit(Event::ContentChanged);
            }
            let mut summary = format!("{verified} files verified");
            if missing > 0 {
                summary.push_str(&format!(" · {missing} missing"));
            }
            if changed > 0 {
                summary.push_str(&format!(" · {changed} changed"));
            }
            if skipped > 0 {
                summary.push_str(&format!(" · {skipped} not reachable"));
            }
            Ok(summary)
        })
    }

    /// Writes a catalog backup now and prunes old ones.
    pub fn backup_now(&self) -> Result<PathBuf> {
        let keep = self.settings().keep_backups.max(1);
        let path = {
            let writer = self
                .inner
                .writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            medianest_vault::create_backup(&writer, self.layout())?
        };
        medianest_vault::prune_backups(self.layout(), keep)?;
        Ok(path)
    }

    pub fn backups(&self) -> Result<Vec<BackupInfo>> {
        Ok(medianest_vault::list_backups(self.layout())?)
    }

    /// Replaces a closed library's catalog with a backup. Returns where the
    /// replaced catalog was saved.
    pub fn restore_backup(root: &Path, backup: &Path) -> Result<PathBuf> {
        let OpenedLibrary {
            layout,
            writer,
            lock,
            ..
        } = medianest_vault::open(root)?;
        drop(writer);
        let saved = medianest_vault::restore_backup(&layout, backup)?;
        drop(lock);
        Ok(saved)
    }

    /// Names places for photos that have coordinates but no place yet.
    pub(crate) fn name_places(&self, limit: u32) {
        let Ok(pending) = self.read(|db| db.assets_needing_place(limit)) else {
            return;
        };
        let mut named = 0;
        for (asset, latitude, longitude) in pending {
            let Some(point) = medianest_core::GeoPoint::new(latitude, longitude, None) else {
                continue;
            };
            let place = medianest_media::geocode::reverse(point);
            if self
                .write(|writer| writer.set_place(asset, place.as_ref()))
                .is_ok()
            {
                named += 1;
            }
        }
        if named > 0 {
            self.emit(Event::ContentChanged);
        }
    }

    /// Regenerates thumbnails: all of them, or only those that failed.
    pub fn rebuild_thumbnails(&self, failed_only: bool) -> Result<usize> {
        if !failed_only {
            self.inner
                .cache
                .clear()
                .map_err(crate::Error::io(self.inner.cache.root()))?;
        }
        let queued = self.write(|writer| writer.reset_thumbnails(failed_only))?;
        self.poke_thumbnails();
        self.emit(Event::ContentChanged);
        Ok(queued)
    }
}
