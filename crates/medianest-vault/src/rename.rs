//! Moving a file into the library by renaming it.
//!
//! On the library's own filesystem a rename needs no extra space and copies
//! nothing. There is also no second copy to compare against, so the file is
//! hashed once before the rename (the catalog needs the hash anyway) and its
//! identity is checked after it: same device, inode, size and modification
//! time. If cataloguing fails afterwards the file is renamed back.

use std::fs::{self, File, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use medianest_catalog::Operation;
use medianest_core::{AssetMetadata, FileStamp, OperationKind, OperationState, PathBytes};

use crate::control::Control;
use crate::faults::{Checkpoint, hit};
use crate::fsops::{create_dir_all_durable, hash_reader, rename_no_replace, sync_dir};
use crate::naming::with_hash_suffix;
use crate::{ImportOutcome, Result, Vault, VaultError};

impl Vault<'_> {
    /// Whether moving `source` into the library can be a rename: it is on the
    /// library's filesystem and has no other hard links. A second link would
    /// leave the library's file sharing its content with a file outside it.
    pub(crate) fn can_rename(&self, source: &Metadata) -> bool {
        source.nlink() == 1
            && fs::metadata(self.layout.media()).is_ok_and(|media| media.dev() == source.dev())
    }

    /// Moves the operation's source into the library by renaming it.
    ///
    /// Returns `None` when the rename turns out to cross filesystems (a bind
    /// mount or a btrfs subvolume looks like the same device); the caller then
    /// copies instead.
    pub(crate) fn rename_in(
        &self,
        operation: &mut Operation,
        source_file: &mut File,
        metadata: &AssetMetadata,
        control: &Control,
    ) -> Result<Option<ImportOutcome>> {
        let source = operation
            .source_path
            .as_ref()
            .map(PathBytes::to_path_buf)
            .ok_or(VaultError::InvalidState)?;
        let stamp = operation.source_stamp.ok_or(VaultError::InvalidState)?;
        let planned = operation
            .dest_relpath
            .clone()
            .ok_or(VaultError::InvalidState)?;

        let (hash, size) = match hash_reader(&mut *source_file, control) {
            Ok(result) => result,
            Err(error) => {
                let state = if matches!(error, VaultError::Cancelled) {
                    OperationState::Cancelled
                } else {
                    OperationState::Failed
                };
                self.fail(operation, state, &error);
                return Err(error);
            }
        };
        let unchanged = fs::symlink_metadata(&source)
            .map(|meta| FileStamp::from_metadata(&meta))
            .is_ok_and(|after| stamp.same_file_unchanged(&after) && after.size == size);
        if !unchanged {
            let error = VaultError::SourceChanged(source);
            self.fail(operation, OperationState::Failed, &error);
            return Err(error);
        }
        operation.hash = Some(hash);

        // Identical content already lives in the library: the original is a
        // duplicate, removed like the original of any other move.
        let existing = self.writer().asset_by_hash(&hash)?;
        if let Some(existing) = existing {
            if self.managed_copy(existing)?.is_some() {
                // The original is removed like any move's: only the kind changes.
                operation.kind = OperationKind::MoveIn;
                operation.asset_id = Some(existing);
                return self
                    .finish(
                        operation,
                        ImportOutcome::AlreadyInLibrary {
                            asset: existing,
                            awaiting_removal: None,
                        },
                        control,
                    )
                    .map(Some);
            }
        }

        // The name is chosen and journaled before the rename, so recovery
        // knows where to look.
        let chosen = [planned.clone(), with_hash_suffix(&planned, &hash)]
            .into_iter()
            .find(|candidate| {
                let path = self.layout.media().join(candidate.as_path());
                matches!(
                    fs::symlink_metadata(path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            });
        let Some(chosen) = chosen else {
            let error = VaultError::Io {
                action: "could not find a free name for",
                path: self.layout.media().join(planned.as_path()),
                source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            };
            self.fail(operation, OperationState::Failed, &error);
            return Err(error);
        };
        let destination = self.layout.media().join(chosen.as_path());
        let folder = destination
            .parent()
            .map_or_else(|| self.layout.media(), Path::to_path_buf);
        if let Err(error) = create_dir_all_durable(&folder) {
            self.fail(operation, OperationState::Failed, &error);
            return Err(error);
        }
        operation.dest_relpath = Some(chosen);
        operation.state = OperationState::Verified;
        self.save(operation)?;
        hit(self.faults, Checkpoint::AfterVerified)?;

        let renamed = if self.faults.rename_crosses_devices() {
            Err(std::io::ErrorKind::CrossesDevices.into())
        } else {
            rename_no_replace(&source, &destination)
        };
        if let Err(error) = renamed {
            if error.kind() == std::io::ErrorKind::CrossesDevices {
                operation.dest_relpath = Some(planned);
                return Ok(None);
            }
            let error = VaultError::io("could not move", &source)(error);
            self.fail(operation, OperationState::Failed, &error);
            return Err(error);
        }
        hit(self.faults, Checkpoint::AfterRename)?;
        let mut folders = vec![folder];
        folders.extend(source.parent().map(Path::to_path_buf));
        for folder in folders {
            sync_dir(&folder)?;
        }

        let in_place = fs::symlink_metadata(&destination)
            .map(|meta| FileStamp::from_metadata(&meta))
            .is_ok_and(|found| stamp.same_file_renamed(&found));
        if !in_place {
            let error = VaultError::VerificationFailed(source);
            self.undo_rename(operation, &error)?;
            return Err(error);
        }
        hit(self.faults, Checkpoint::AfterInstall)?;

        match self.catalog_installed(operation, metadata, control) {
            Ok(outcome) => Ok(Some(outcome)),
            Err(error @ VaultError::Interrupted(_)) => Err(error),
            // The catalog never learned of the file: put it back where it was.
            Err(error) if operation.file_id.is_none() => {
                self.undo_rename(operation, &error)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    /// Renames the file back to where it came from and records the failure.
    /// When that is impossible too, a person has to look.
    pub(crate) fn undo_rename(&self, operation: &mut Operation, error: &VaultError) -> Result<()> {
        let (Some(source), Some(relpath)) = (
            operation.source_path.as_ref().map(PathBytes::to_path_buf),
            operation.dest_relpath.as_ref(),
        ) else {
            return Err(VaultError::InvalidState);
        };
        let destination = self.layout.media().join(relpath.as_path());
        match rename_no_replace(&destination, &source) {
            Ok(()) => {
                for folder in [destination.parent(), source.parent()]
                    .into_iter()
                    .flatten()
                {
                    sync_dir(folder)?;
                }
                self.fail(operation, OperationState::Failed, error);
                Ok(())
            }
            Err(undo) => self.escalate(
                operation,
                &format!("could not put the original back after: {error} ({undo})"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use medianest_core::OperationKind;

    use super::*;

    #[test]
    fn undoing_a_rename_puts_the_original_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = crate::create(&dir.path().join("Library")).expect("create");
        let writer = Mutex::new(opened.writer);
        let vault = Vault::new(&opened.layout, &writer);

        let source = dir.path().join("Camera/IMG_1.jpg");
        fs::create_dir_all(source.parent().unwrap()).expect("dir");
        fs::write(&source, b"photo").expect("write");
        let stamp = FileStamp::from_metadata(&fs::metadata(&source).expect("stat"));
        let relpath = PathBytes::from_path(Path::new("2023/07/01/IMG_1.jpg"));
        let mut operation = Operation::planned(
            OperationKind::RenameIn,
            PathBytes::from_path(&source),
            stamp,
            relpath.clone(),
            None,
        );
        let placed = opened.layout.media().join(relpath.as_path());
        fs::create_dir_all(placed.parent().unwrap()).expect("dir");
        fs::rename(&source, &placed).expect("rename");

        vault
            .undo_rename(&mut operation, &VaultError::Cancelled)
            .expect("undo");
        assert_eq!(fs::read(&source).expect("original is back"), b"photo");
        assert!(!placed.exists());
        assert_eq!(operation.state, OperationState::Failed);
    }

    #[test]
    fn undoing_into_an_occupied_name_asks_for_review() {
        let dir = tempfile::tempdir().expect("tempdir");
        let opened = crate::create(&dir.path().join("Library")).expect("create");
        let writer = Mutex::new(opened.writer);
        let vault = Vault::new(&opened.layout, &writer);

        let source = dir.path().join("IMG_1.jpg");
        fs::write(&source, b"someone else's file").expect("write");
        let stamp = FileStamp::from_metadata(&fs::metadata(&source).expect("stat"));
        let relpath = PathBytes::from_path(Path::new("IMG_1.jpg"));
        let mut operation = Operation::planned(
            OperationKind::RenameIn,
            PathBytes::from_path(&source),
            stamp,
            relpath.clone(),
            None,
        );
        let placed = opened.layout.media().join(relpath.as_path());
        fs::write(&placed, b"library file").expect("write");

        vault
            .undo_rename(&mut operation, &VaultError::Cancelled)
            .expect("escalated");
        assert_eq!(operation.state, OperationState::NeedsReview);
        assert_eq!(
            fs::read(&source).expect("untouched"),
            b"someone else's file"
        );
        assert_eq!(fs::read(&placed).expect("untouched"), b"library file");
    }
}
