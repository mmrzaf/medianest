//! Finishing or undoing operations interrupted by a crash or power loss.
//!
//! Recovery only advances an operation when the bytes on disk prove the
//! journaled state, and never removes an original on its own. Running it
//! twice has the same effect as running it once.

use std::fs;
use std::path::{Path, PathBuf};

use medianest_catalog::{Operation, ReviewSubject};
use medianest_core::{AssetMetadata, OperationState, PathBytes, ReviewKind};
use medianest_media::ProbeError;

use crate::control::Control;
use crate::fsops::hash_file;
use crate::import::temp_name;
use crate::{ImportOutcome, Result, Vault, VaultError};

/// What recovery did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RecoveryReport {
    /// Finished copies or removals.
    pub completed: u32,
    /// Interrupted before anything was installed; cleaned up, originals untouched.
    pub rolled_back: u32,
    /// Moves whose original could not be removed and wait for the user.
    pub awaiting_removal: u32,
    /// Situations recovery could not decide; listed in the review inbox.
    pub needs_review: u32,
}

impl RecoveryReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.completed + self.rolled_back + self.needs_review == 0
    }
}

type Probe<'p> = &'p dyn Fn(&Path) -> std::result::Result<AssetMetadata, ProbeError>;

impl Vault<'_> {
    /// Reconciles every unfinished operation with what is on disk.
    pub fn recover(&self, probe: Probe<'_>) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        let operations = self.writer().unfinished_operations()?;
        for mut operation in operations {
            let uuid = operation.uuid;
            match self.recover_one(&mut operation, probe) {
                Ok(Outcome::Completed) => report.completed += 1,
                Ok(Outcome::RolledBack) => report.rolled_back += 1,
                Ok(Outcome::Awaiting) => report.awaiting_removal += 1,
                Ok(Outcome::Review) => report.needs_review += 1,
                Err(error) => {
                    tracing::error!(operation = %uuid, %error, "recovery could not decide");
                    self.escalate(&mut operation, &error.to_string())?;
                    report.needs_review += 1;
                }
            }
        }
        if !report.is_empty() {
            tracing::info!(?report, "recovered interrupted operations");
        }
        Ok(report)
    }

    fn temp_of(&self, operation: &Operation) -> Option<PathBuf> {
        let temp = operation.temp_path.as_ref()?.to_path_buf();
        // Only ever touch a temporary file MediaNest itself named, inside Media/.
        let expected = temp_name(operation.uuid);
        let valid = temp.starts_with(self.layout.media())
            && temp
                .file_name()
                .is_some_and(|name| name == expected.as_str());
        valid.then_some(temp)
    }

    fn destination_of(&self, operation: &Operation) -> Option<PathBuf> {
        let relpath = operation.dest_relpath.as_ref()?;
        Some(self.layout.media().join(relpath.as_path()))
    }

    fn metadata_for(
        &self,
        operation: &Operation,
        fallback: &Path,
        probe: Probe<'_>,
    ) -> AssetMetadata {
        operation
            .source_path
            .as_ref()
            .map(PathBytes::to_path_buf)
            .filter(|source| source.is_file())
            .and_then(|source| probe(&source).ok())
            .or_else(|| probe(fallback).ok())
            .unwrap_or_else(|| {
                let format =
                    medianest_media::detect(fallback).unwrap_or(medianest_core::Format::Jpeg);
                AssetMetadata::minimal(format)
            })
    }

    fn roll_back(&self, operation: &mut Operation, reason: &str) -> Result<Outcome> {
        if let Some(temp) = self.temp_of(operation) {
            match fs::remove_file(&temp) {
                Ok(()) => {
                    if let Some(parent) = temp.parent() {
                        let _ = crate::fsops::sync_dir(parent);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(VaultError::io("could not remove", temp)(error)),
            }
        }
        operation.state = OperationState::Failed;
        operation.error = Some(reason.to_owned());
        self.write(|writer| writer.save_operation(operation))?;
        Ok(Outcome::RolledBack)
    }

    fn escalate(&self, operation: &mut Operation, reason: &str) -> Result<()> {
        operation.state = OperationState::NeedsReview;
        operation.error = Some(reason.to_owned());
        let uuid = operation.uuid;
        let source = operation.source_path.clone();
        let detail = serde_json::json!({
            "reason": reason,
            "source": source.as_ref().map(PathBytes::display),
            "copy": operation.dest_relpath.as_ref().map(PathBytes::display),
        });
        self.write(|writer| {
            writer.save_operation(operation)?;
            writer.open_review(
                ReviewKind::InterruptedOperation,
                ReviewSubject::Operation(uuid, source.as_ref()),
                &detail,
            )?;
            Ok(())
        })
    }

    fn from_outcome(outcome: &ImportOutcome) -> Outcome {
        match outcome {
            ImportOutcome::Imported {
                awaiting_removal: Some(_),
                ..
            }
            | ImportOutcome::AlreadyInLibrary {
                awaiting_removal: Some(_),
                ..
            } => Outcome::Awaiting,
            _ => Outcome::Completed,
        }
    }

    fn recover_one(&self, operation: &mut Operation, probe: Probe<'_>) -> Result<Outcome> {
        match operation.state {
            OperationState::Planned | OperationState::Copying => self.roll_back(
                operation,
                "interrupted before the copy was complete; the original is untouched",
            ),
            OperationState::Verified => {
                let hash = operation.hash.ok_or(VaultError::InvalidState)?;
                if let Some(temp) = self.temp_of(operation).filter(|temp| temp.is_file()) {
                    let intact =
                        hash_file(&temp, &Control::new()).is_ok_and(|(found, _)| found == hash);
                    if !intact {
                        return self.roll_back(
                            operation,
                            "the interrupted copy was incomplete; the original is untouched",
                        );
                    }
                    let metadata = self.metadata_for(operation, &temp, probe);
                    let outcome =
                        self.finish_from_verified(operation, &metadata, &Control::new())?;
                    return Ok(Self::from_outcome(&outcome));
                }
                // The rename may have happened just before the interruption.
                let installed = self
                    .destination_of(operation)
                    .filter(|path| path.is_file())
                    .filter(|path| {
                        hash_file(path, &Control::new()).is_ok_and(|(found, _)| found == hash)
                    });
                match installed {
                    Some(destination) => {
                        operation.state = OperationState::Installed;
                        let metadata = self.metadata_for(operation, &destination, probe);
                        let outcome =
                            self.catalog_installed(operation, &metadata, &Control::new())?;
                        Ok(Self::from_outcome(&outcome))
                    }
                    None => self.roll_back(
                        operation,
                        "the interrupted copy was not found; the original is untouched",
                    ),
                }
            }
            OperationState::Installed => {
                let hash = operation.hash.ok_or(VaultError::InvalidState)?;
                let destination = self
                    .destination_of(operation)
                    .filter(|path| path.is_file())
                    .filter(|path| {
                        hash_file(path, &Control::new()).is_ok_and(|(found, _)| found == hash)
                    });
                match destination {
                    Some(destination) => {
                        let metadata = self.metadata_for(operation, &destination, probe);
                        let outcome =
                            self.catalog_installed(operation, &metadata, &Control::new())?;
                        Ok(Self::from_outcome(&outcome))
                    }
                    None => {
                        self.escalate(
                            operation,
                            "the installed copy is missing or does not match the original",
                        )?;
                        Ok(Outcome::Review)
                    }
                }
            }
            OperationState::Catalogued => {
                let asset = operation.asset_id.ok_or(VaultError::InvalidState)?;
                let file = operation.file_id.ok_or(VaultError::InvalidState)?;
                let relpath = operation
                    .dest_relpath
                    .clone()
                    .ok_or(VaultError::InvalidState)?;
                let outcome = self.finish(
                    operation,
                    ImportOutcome::Imported {
                        asset,
                        file,
                        relpath,
                        new_asset: false,
                        awaiting_removal: None,
                    },
                    &Control::new(),
                )?;
                Ok(Self::from_outcome(&outcome))
            }
            OperationState::AwaitingRemoval => {
                self.open_removal_review(operation)?;
                Ok(Outcome::Awaiting)
            }
            OperationState::Removing => {
                let aside = operation.aside_path.as_ref().map(PathBytes::to_path_buf);
                let source = operation.source_path.as_ref().map(PathBytes::to_path_buf);
                match (aside.filter(|path| path.is_file()), source) {
                    (Some(aside), _) => {
                        self.remove_aside(operation, &aside)?;
                        Ok(Outcome::Completed)
                    }
                    (None, Some(source)) if source.exists() => {
                        // The rename never happened; ask again.
                        operation.state = OperationState::AwaitingRemoval;
                        operation.aside_path = None;
                        self.write(|writer| writer.save_operation(operation))?;
                        Ok(Outcome::Awaiting)
                    }
                    _ => {
                        operation.state = OperationState::Completed;
                        let uuid = operation.uuid;
                        self.write(|writer| {
                            writer.save_operation(operation)?;
                            writer.close_operation_reviews(
                                uuid,
                                medianest_core::ReviewState::Resolved,
                            )
                        })?;
                        Ok(Outcome::Completed)
                    }
                }
            }
            OperationState::NeedsReview => Ok(Outcome::Review),
            OperationState::Completed | OperationState::Failed | OperationState::Cancelled => {
                Ok(Outcome::Completed)
            }
        }
    }
}

enum Outcome {
    Completed,
    RolledBack,
    Awaiting,
    Review,
}
