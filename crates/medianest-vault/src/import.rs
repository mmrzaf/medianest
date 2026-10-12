//! Verified, journaled copies into the managed folder.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use medianest_catalog::{NewFile, Operation, ReviewSubject};
use medianest_core::{
    AssetId, AssetMetadata, FileId, FileStamp, FileStatus, OperationKind, OperationState,
    OperationUuid, PathBytes, ReviewKind, SourceKind,
};

use crate::control::Control;
use crate::faults::{Checkpoint, hit};
use crate::fsops::{copy_hashed, create_new, hash_file, open_regular, rename_no_replace, sync_dir};
use crate::naming::{managed_relpath, with_hash_suffix};
use crate::{Result, Vault, VaultError};

/// What happens to originals after a managed import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImportMode {
    /// Originals stay where they are.
    Copy,
    /// Each original is removed as soon as its library copy is verified, so
    /// both copies never take up space for longer than one file. An original
    /// that cannot be removed safely is kept and left for the user to decide.
    Move,
}

/// Result of importing one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// A new verified copy now lives in the library.
    Imported {
        asset: AssetId,
        file: FileId,
        relpath: PathBytes,
        new_asset: bool,
        /// For moves: the operation whose original could not be removed and
        /// waits for the user to decide.
        awaiting_removal: Option<OperationUuid>,
    },
    /// Identical content is already in the managed folder; nothing was copied.
    AlreadyInLibrary {
        asset: AssetId,
        awaiting_removal: Option<OperationUuid>,
    },
}

pub(crate) fn temp_name(operation: OperationUuid) -> String {
    format!(".medianest-{}.part", operation.simple())
}

impl Vault<'_> {
    fn save(&self, operation: &Operation) -> Result<()> {
        self.write(|writer| writer.save_operation(operation))
    }

    fn fail(&self, operation: &mut Operation, state: OperationState, error: &VaultError) {
        operation.state = state;
        operation.error = Some(error.to_string());
        if let Err(save_error) = self.save(operation) {
            tracing::error!(operation = %operation.uuid, error = %save_error, "could not record failed operation");
        }
    }

    fn remove_temp(&self, path: &Path) {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), %error, "could not remove a temporary copy");
            }
        }
        if let Some(parent) = path.parent() {
            let _ = sync_dir(parent);
        }
    }

    /// The managed folder's source id and a present managed copy of `asset`, if any.
    pub(crate) fn managed_copy(&self, asset: AssetId) -> Result<Option<PathBuf>> {
        let files = self.writer().files_of(asset)?;
        Ok(files
            .into_iter()
            .find(|location| {
                location.source_kind == SourceKind::Managed
                    && matches!(
                        location.file.status,
                        FileStatus::Verified | FileStatus::Unverified
                    )
                    && location.file.trash_path.is_none()
            })
            .map(|location| location.path()))
    }

    /// Copies one file into the managed folder.
    ///
    /// The copy is written next to its destination under a temporary name,
    /// synced, re-read and compared with the original's hash, then installed
    /// with a rename that never replaces an existing file. The original is
    /// only read. For [`ImportMode::Move`] it is then removed, after checks
    /// that it is still the file that was copied.
    pub fn import_file(
        &self,
        source: &Path,
        metadata: &AssetMetadata,
        mode: ImportMode,
        batch: Option<&str>,
        control: &Control,
    ) -> Result<ImportOutcome> {
        control.checkpoint()?;
        let (mut source_file, source_metadata) = open_regular(source)?;
        let source = source
            .canonicalize()
            .map_err(VaultError::io("could not resolve", source))?;
        if self.layout.contains(&source) {
            return Err(VaultError::InsideLibrary(source));
        }
        let stamp = FileStamp::from_metadata(&source_metadata);
        let file_name = source.file_name().unwrap_or_else(|| OsStr::new("unnamed"));
        let relpath = managed_relpath(metadata, file_name);
        let kind = match mode {
            ImportMode::Copy => OperationKind::CopyIn,
            ImportMode::Move => OperationKind::MoveIn,
        };
        let mut operation = Operation::planned(
            kind,
            PathBytes::from_path(&source),
            stamp,
            relpath.clone(),
            batch.map(str::to_owned),
        );
        self.save(&operation)?;
        hit(self.faults, Checkpoint::AfterPlan)?;

        let destination = self.layout.media().join(relpath.as_path());
        let folder = destination
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.layout.media());
        if let Err(error) = fs::create_dir_all(&folder) {
            let error = VaultError::Io {
                action: "could not create",
                path: folder,
                source: error,
            };
            self.fail(&mut operation, OperationState::Failed, &error);
            return Err(error);
        }
        sync_dir(&folder)?;
        if let Some(parent) = folder.parent() {
            sync_dir(parent)?;
        }
        let temp = folder.join(temp_name(operation.uuid));
        operation.temp_path = Some(PathBytes::from_path(&temp));
        operation.state = OperationState::Copying;
        self.save(&operation)?;
        let mut temp_file = match create_new(&temp) {
            Ok(file) => file,
            Err(error) => {
                self.fail(&mut operation, OperationState::Failed, &error);
                return Err(error);
            }
        };
        hit(self.faults, Checkpoint::AfterTempCreated)?;

        let copied = copy_hashed(&mut source_file, &mut temp_file, control, &temp);
        drop(temp_file);
        let (hash, size) = match copied {
            Ok(result) => result,
            Err(error) => {
                self.remove_temp(&temp);
                let state = if matches!(error, VaultError::Cancelled) {
                    OperationState::Cancelled
                } else {
                    OperationState::Failed
                };
                self.fail(&mut operation, state, &error);
                return Err(error);
            }
        };
        sync_dir(&folder)?;
        hit(self.faults, Checkpoint::AfterCopy)?;

        // The original must still be the same, untouched file.
        let unchanged = fs::symlink_metadata(&source)
            .map(|meta| FileStamp::from_metadata(&meta))
            .is_ok_and(|after| stamp.same_file_unchanged(&after) && after.size == size);
        if !unchanged {
            self.remove_temp(&temp);
            let error = VaultError::SourceChanged(source.clone());
            self.fail(&mut operation, OperationState::Failed, &error);
            return Err(error);
        }
        // Read the copy back from disk and compare.
        let verified = hash_file(&temp, &Control::new())
            .is_ok_and(|(check, length)| check == hash && length == size);
        if !verified {
            self.remove_temp(&temp);
            let error = VaultError::VerificationFailed(source.clone());
            self.fail(&mut operation, OperationState::Failed, &error);
            return Err(error);
        }
        operation.hash = Some(hash);
        operation.state = OperationState::Verified;
        self.save(&operation)?;
        hit(self.faults, Checkpoint::AfterVerified)?;

        self.finish_from_verified(&mut operation, metadata, control)
    }

    /// Continues an operation whose temporary copy is verified: deduplicate,
    /// install, catalog and finish. Shared by imports and crash recovery.
    pub(crate) fn finish_from_verified(
        &self,
        operation: &mut Operation,
        metadata: &AssetMetadata,
        control: &Control,
    ) -> Result<ImportOutcome> {
        let hash = operation.hash.ok_or(VaultError::InvalidState)?;
        let temp = operation
            .temp_path
            .as_ref()
            .map(PathBytes::to_path_buf)
            .ok_or(VaultError::InvalidState)?;
        let relpath = operation
            .dest_relpath
            .clone()
            .ok_or(VaultError::InvalidState)?;

        let existing = self.writer().asset_by_hash(&hash)?;
        if let Some(existing) = existing {
            if self.managed_copy(existing)?.is_some() {
                self.remove_temp(&temp);
                operation.asset_id = Some(existing);
                return self.finish(
                    operation,
                    ImportOutcome::AlreadyInLibrary {
                        asset: existing,
                        awaiting_removal: None,
                    },
                    control,
                );
            }
        }

        let installed = self.install(&temp, &relpath, &hash)?;
        operation.dest_relpath = Some(installed.clone());
        operation.state = OperationState::Installed;
        self.save(operation)?;
        hit(self.faults, Checkpoint::AfterInstall)?;

        self.catalog_installed(operation, metadata, control)
    }

    /// Records an installed copy in the catalog, then finishes the operation.
    pub(crate) fn catalog_installed(
        &self,
        operation: &mut Operation,
        metadata: &AssetMetadata,
        control: &Control,
    ) -> Result<ImportOutcome> {
        let hash = operation.hash.ok_or(VaultError::InvalidState)?;
        let relpath = operation
            .dest_relpath
            .clone()
            .ok_or(VaultError::InvalidState)?;
        let destination = self.layout.media().join(relpath.as_path());
        let stamp = fs::symlink_metadata(&destination)
            .map(|meta| FileStamp::from_metadata(&meta))
            .map_err(VaultError::io("could not inspect", &destination))?;
        let managed = self.writer().managed_source()?.id;
        let name = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let existing_file = self.writer().file_by_path(managed, &relpath)?;
        let (asset, file, new_asset) = match existing_file {
            // Recovery may find the copy already catalogued.
            Some(record) => (record.asset_id, record.id, false),
            None => {
                operation.state = OperationState::Catalogued;
                let added = self.write(|writer| {
                    writer.catalog_installed(
                        operation,
                        &NewFile {
                            source: managed,
                            relpath: &relpath,
                            stamp,
                            status: FileStatus::Verified,
                            hash: Some(hash),
                        },
                        &name,
                        metadata,
                    )
                })?;
                (added.asset, added.file, added.new_asset)
            }
        };
        operation.asset_id = Some(asset);
        operation.file_id = Some(file);
        operation.state = OperationState::Catalogued;
        self.save(operation)?;
        hit(self.faults, Checkpoint::AfterCatalogued)?;
        self.finish(
            operation,
            ImportOutcome::Imported {
                asset,
                file,
                relpath,
                new_asset,
                awaiting_removal: None,
            },
            control,
        )
    }

    /// Completes a copy, or completes a move by removing the original. When
    /// the original cannot be removed safely it is kept and a review is opened.
    pub(crate) fn finish(
        &self,
        operation: &mut Operation,
        outcome: ImportOutcome,
        control: &Control,
    ) -> Result<ImportOutcome> {
        match operation.kind {
            OperationKind::CopyIn => {
                operation.state = OperationState::Completed;
                self.save(operation)?;
                Ok(outcome)
            }
            OperationKind::MoveIn => {
                operation.state = OperationState::AwaitingRemoval;
                self.save(operation)?;
                let uuid = operation.uuid;
                match self.approve_removal(uuid, control) {
                    Ok(()) => return Ok(outcome),
                    Err(error @ (VaultError::Cancelled | VaultError::Interrupted(_))) => {
                        return Err(error);
                    }
                    Err(error) => {
                        tracing::warn!(operation = %uuid, %error, "original kept after a move");
                        operation.error = Some(error.to_string());
                        self.save(operation)?;
                    }
                }
                self.open_removal_review(operation)?;
                Ok(match outcome {
                    ImportOutcome::Imported {
                        asset,
                        file,
                        relpath,
                        new_asset,
                        ..
                    } => ImportOutcome::Imported {
                        asset,
                        file,
                        relpath,
                        new_asset,
                        awaiting_removal: Some(uuid),
                    },
                    ImportOutcome::AlreadyInLibrary { asset, .. } => {
                        ImportOutcome::AlreadyInLibrary {
                            asset,
                            awaiting_removal: Some(uuid),
                        }
                    }
                })
            }
        }
    }

    /// Asks the user what to do with an original that was not removed.
    pub(crate) fn open_removal_review(&self, operation: &Operation) -> Result<()> {
        let uuid = operation.uuid;
        let source = operation.source_path.clone();
        let detail = serde_json::json!({
            "source": source.as_ref().map(PathBytes::display),
            "copy": operation.dest_relpath.as_ref().map(PathBytes::display),
            "size": operation.source_stamp.map(|stamp| stamp.size),
            "reason": operation.error,
        });
        self.write(|writer| {
            writer.open_review(
                ReviewKind::PendingRemoval,
                ReviewSubject::Operation(uuid, source.as_ref()),
                &detail,
            )
        })?;
        Ok(())
    }

    /// Moves a verified temporary copy to its final name without replacing
    /// anything. If a different file holds the name, a hash suffix is added.
    /// If an identical file is already there, it is reused.
    fn install(&self, temp: &Path, relpath: &PathBytes, hash: &[u8; 32]) -> Result<PathBytes> {
        let candidates = [relpath.clone(), with_hash_suffix(relpath, hash)];
        for candidate in candidates {
            let destination = self.layout.media().join(candidate.as_path());
            match rename_no_replace(temp, &destination) {
                Ok(()) => {
                    if let Some(parent) = destination.parent() {
                        sync_dir(parent)?;
                    }
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let same = hash_file(&destination, &Control::new())
                        .is_ok_and(|(existing, _)| existing == *hash);
                    if same {
                        self.remove_temp(temp);
                        return Ok(candidate);
                    }
                }
                Err(error) => {
                    return Err(VaultError::Io {
                        action: "could not install",
                        path: destination,
                        source: error,
                    });
                }
            }
        }
        Err(VaultError::Io {
            action: "could not find a free name for",
            path: self.layout.media().join(relpath.as_path()),
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        })
    }
}
