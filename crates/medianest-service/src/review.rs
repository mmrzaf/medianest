//! Acting on review inbox items.

use std::path::Path;

use medianest_catalog::ReviewItem;
use medianest_core::{FileStamp, ReviewId, ReviewKind, ReviewState, SourceKind};
use medianest_vault::{Control, ImportMode};

use crate::events::Event;
use crate::jobs::Lane;
use crate::{Error, Library, Result};

/// What to do with a review item. The meaning depends on its kind; see
/// [`Library::review_actions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    Primary,
    Secondary,
    Dismiss,
}

impl Library {
    /// Open review items, optionally of one kind.
    pub fn review_items(&self, kind: Option<ReviewKind>) -> Result<Vec<ReviewItem>> {
        self.read(|db| db.reviews(kind, 5_000))
    }

    /// Open items per kind. Photos with guessed or missing dates are counted
    /// under [`ReviewKind::NeedsDate`]; they are found with a search rather
    /// than stored as items.
    pub fn review_counts(&self) -> Result<Vec<(ReviewKind, u64)>> {
        let (mut counts, stats) = self.read(|db| Ok((db.review_counts()?, db.stats()?)))?;
        let needs_date = stats.unknown_dates + stats.uncertain_dates;
        if needs_date > 0 {
            counts.insert(0, (ReviewKind::NeedsDate, needs_date));
        }
        Ok(counts)
    }

    /// Button labels for a kind: primary, then secondary when there is one.
    #[must_use]
    pub fn review_actions(kind: ReviewKind) -> (&'static str, Option<&'static str>) {
        match kind {
            ReviewKind::NeedsDate => ("Set date…", None),
            ReviewKind::MissingFile => ("Remove from library", Some("Look again")),
            ReviewKind::ChangedFile => ("Use the new version", None),
            ReviewKind::FailedImport => ("Try again", None),
            ReviewKind::PendingRemoval => ("Remove original", Some("Keep original")),
            ReviewKind::InterruptedOperation => ("Check again", None),
            ReviewKind::Unreadable => ("Look again", None),
        }
    }

    /// Applies an action to one item. Returns a short message for a toast.
    pub fn resolve_review(&self, id: ReviewId, action: ReviewAction) -> Result<String> {
        let item = self.read(|db| db.review(id))?;
        let message = self.apply(&item, action)?;
        self.emit(Event::ReviewChanged);
        Ok(message)
    }

    /// Applies an action to every open item of a kind.
    pub fn resolve_all(
        &self,
        kind: ReviewKind,
        action: ReviewAction,
    ) -> Result<(usize, Vec<String>)> {
        let items = self.review_items(Some(kind))?;
        let mut done = 0;
        let mut errors = Vec::new();
        for item in &items {
            match self.apply(item, action) {
                Ok(_) => done += 1,
                Err(error) => errors.push(format!("{}: {error}", item.subject)),
            }
        }
        self.emit(Event::ReviewChanged);
        Ok((done, errors))
    }

    fn dismiss(&self, item: &ReviewItem) -> Result<String> {
        self.write(|writer| writer.close_review(item.id, ReviewState::Dismissed))?;
        Ok("Dismissed".into())
    }

    fn apply(&self, item: &ReviewItem, action: ReviewAction) -> Result<String> {
        if action == ReviewAction::Dismiss {
            return self.dismiss(item);
        }
        match (item.kind, action) {
            (ReviewKind::MissingFile, ReviewAction::Primary) => {
                let file = item
                    .file_id
                    .ok_or_else(|| Error::Invalid("no file recorded".into()))?;
                let location = self.read(|db| db.file(file))?;
                let relpath = location.file.relpath.clone();
                let source = location.file.source_id;
                let linked = location.source_kind == SourceKind::Linked;
                let forgotten = self.write(|writer| {
                    if linked {
                        writer.exclude(source, &relpath)?;
                    }
                    writer.close_review(item.id, ReviewState::Resolved)?;
                    writer.remove_file(file)
                })?;
                if let Some(uuid) = forgotten {
                    self.inner.cache.remove(uuid);
                }
                self.emit(Event::ContentChanged);
                Ok("Removed from the library".into())
            }
            (ReviewKind::MissingFile | ReviewKind::Unreadable, _) => {
                let source = match item.file_id {
                    Some(file) => Some(self.read(|db| db.file(file))?.file.source_id),
                    None => None,
                };
                self.scan(source, Lane::Foreground);
                Ok("Looking again".into())
            }
            (ReviewKind::ChangedFile, _) => {
                let file = item
                    .file_id
                    .ok_or_else(|| Error::Invalid("no file recorded".into()))?;
                let location = self.read(|db| db.file(file))?;
                let path = location.path();
                let metadata = self.probe(&path)?;
                let (hash, _) = medianest_vault::hash_file(&path, &Control::new())?;
                let stamp = std::fs::symlink_metadata(&path)
                    .map(|meta| FileStamp::from_metadata(&meta))
                    .map_err(Error::io(&path))?;
                self.write(|writer| writer.accept_changed(file, &hash, &stamp, &metadata))?;
                if let Ok(asset) = self.read(|db| db.file(file)) {
                    let uuid = self.read(|db| db.asset(asset.file.asset_id))?.uuid;
                    self.inner.cache.remove(uuid);
                }
                self.poke_thumbnails();
                self.emit(Event::ContentChanged);
                Ok("Updated to the new version".into())
            }
            (ReviewKind::FailedImport, _) => {
                let path = item
                    .path
                    .as_ref()
                    .map(|path| path.to_path_buf())
                    .ok_or_else(|| Error::Invalid("no path recorded".into()))?;
                self.import_one(Path::new(&path), ImportMode::Copy)?;
                Ok("Imported".into())
            }
            (ReviewKind::PendingRemoval, ReviewAction::Primary) => {
                let operation = item
                    .operation
                    .ok_or_else(|| Error::Invalid("no operation recorded".into()))?;
                self.vault().approve_removal(operation, &Control::new())?;
                Ok("Original removed".into())
            }
            (ReviewKind::PendingRemoval, _) => {
                let operation = item
                    .operation
                    .ok_or_else(|| Error::Invalid("no operation recorded".into()))?;
                self.vault().keep_original(operation)?;
                Ok("Original kept".into())
            }
            (ReviewKind::InterruptedOperation, _) => {
                let probe = |path: &Path| self.probe(path);
                let report = self.vault().recover(&probe)?;
                if let Some(operation) = item.operation {
                    let state = self.read(|db| db.operation(operation))?.state;
                    if state != medianest_core::OperationState::NeedsReview {
                        self.write(|writer| writer.close_review(item.id, ReviewState::Resolved))?;
                    }
                }
                self.emit(Event::ContentChanged);
                Ok(if report.needs_review > 0 {
                    "Still needs your attention".into()
                } else {
                    "Resolved".into()
                })
            }
            (ReviewKind::NeedsDate, _) => {
                Err(Error::Invalid("set the date from the photo instead".into()))
            }
        }
    }
}
