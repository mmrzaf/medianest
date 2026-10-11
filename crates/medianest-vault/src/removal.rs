//! Removing originals after a move, once the user approves.

use std::fs;
use std::path::Path;

use medianest_catalog::Operation;
use medianest_core::{
    FileStamp, OperationKind, OperationState, OperationUuid, PathBytes, ReviewState,
};

use crate::control::Control;
use crate::faults::{Checkpoint, hit};
use crate::fsops::{hash_file, rename_no_replace, sync_dir};
use crate::{Result, Vault, VaultError};

pub(crate) fn aside_name(operation: OperationUuid) -> String {
    format!(".medianest-removing-{}", operation.simple())
}

impl Vault<'_> {
    /// Removes the original of a staged move.
    ///
    /// Refuses unless a verified copy is in the library and the original is
    /// still the very same file, unchanged since it was copied: same device,
    /// inode, size, modification, change and birth times, and the same content.
    /// The original is first renamed aside atomically, so the path can never
    /// point at a different file at the moment of removal.
    pub fn approve_removal(&self, uuid: OperationUuid, control: &Control) -> Result<()> {
        let mut operation = self.writer().operation(uuid)?;
        if operation.kind != OperationKind::MoveIn
            || operation.state != OperationState::AwaitingRemoval
        {
            return Err(VaultError::InvalidState);
        }
        let hash = operation.hash.ok_or(VaultError::InvalidState)?;
        let stamp = operation.source_stamp.ok_or(VaultError::InvalidState)?;
        let source = operation
            .source_path
            .as_ref()
            .map(PathBytes::to_path_buf)
            .ok_or(VaultError::InvalidState)?;

        let asset = operation.asset_id.ok_or(VaultError::CopyNotVerified)?;
        let copy = self
            .managed_copy(asset)?
            .ok_or(VaultError::CopyNotVerified)?;
        let copy_ok = hash_file(&copy, control).is_ok_and(|(found, _)| found == hash);
        if !copy_ok {
            return Err(VaultError::CopyNotVerified);
        }

        let current = match fs::symlink_metadata(&source) {
            Ok(meta) => FileStamp::from_metadata(&meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Already gone: nothing to remove.
                return self.complete_removal(&mut operation);
            }
            Err(error) => {
                return Err(VaultError::Io {
                    action: "could not inspect",
                    path: source,
                    source: error,
                });
            }
        };
        if !stamp.same_file_unchanged(&current) {
            return Err(self.refuse(&mut operation, VaultError::SourceReplaced(source)));
        }
        let content_same = hash_file(&source, control).is_ok_and(|(found, _)| found == hash);
        if !content_same {
            return Err(self.refuse(&mut operation, VaultError::SourceReplaced(source)));
        }

        let parent = source
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf();
        let aside = parent.join(aside_name(uuid));
        operation.aside_path = Some(PathBytes::from_path(&aside));
        operation.state = OperationState::Removing;
        self.write(|writer| writer.save_operation(&operation))?;
        rename_no_replace(&source, &aside)
            .map_err(VaultError::io("could not move aside", &source))?;
        sync_dir(&parent)?;
        hit(self.faults, Checkpoint::AfterAside)?;
        self.remove_aside(&mut operation, &aside)
    }

    /// Deletes the renamed-aside original after confirming it is the same file object.
    pub(crate) fn remove_aside(&self, operation: &mut Operation, aside: &Path) -> Result<()> {
        let stamp = operation.source_stamp.ok_or(VaultError::InvalidState)?;
        let meta =
            fs::symlink_metadata(aside).map_err(VaultError::io("could not inspect", aside))?;
        let found = FileStamp::from_metadata(&meta);
        // A rename updates the change time, so compare the other identity fields.
        let same = found.device == stamp.device
            && found.inode == stamp.inode
            && found.size == stamp.size
            && found.mtime_ns == stamp.mtime_ns;
        if !same {
            operation.state = OperationState::NeedsReview;
            operation.error =
                Some("the file set aside for removal is not the imported original".into());
            self.write(|writer| writer.save_operation(operation))?;
            return Err(VaultError::SourceReplaced(aside.to_path_buf()));
        }
        fs::remove_file(aside).map_err(VaultError::io("could not remove", aside))?;
        if let Some(parent) = aside.parent() {
            sync_dir(parent)?;
        }
        tracing::info!(operation = %operation.uuid, "original removed after approval");
        self.complete_removal(operation)
    }

    fn complete_removal(&self, operation: &mut Operation) -> Result<()> {
        operation.state = OperationState::Completed;
        operation.error = None;
        let uuid = operation.uuid;
        self.write(|writer| {
            writer.save_operation(operation)?;
            writer.close_operation_reviews(uuid, ReviewState::Resolved)
        })
    }

    fn refuse(&self, operation: &mut Operation, error: VaultError) -> VaultError {
        operation.error = Some(error.to_string());
        let _ = self.write(|writer| writer.save_operation(operation));
        error
    }

    /// Keeps the original of a staged move: the move becomes a copy.
    pub fn keep_original(&self, uuid: OperationUuid) -> Result<()> {
        let mut operation = self.writer().operation(uuid)?;
        if operation.state != OperationState::AwaitingRemoval {
            return Err(VaultError::InvalidState);
        }
        operation.state = OperationState::Completed;
        operation.error = Some("original kept".into());
        self.write(|writer| {
            writer.save_operation(&operation)?;
            writer.close_operation_reviews(uuid, ReviewState::Dismissed)
        })
    }
}
