//! Checking that files are still where they were, with the same content.

use std::fs;

use medianest_catalog::FileLocation;
use medianest_core::{AssetId, FileStamp, FileStatus, SourceStatus};

use crate::control::{Control, Progress};
use crate::fsops::hash_file;
use crate::{Result, Vault, VaultError};

/// Result of verifying one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Present, and the content matches (or was recorded for the first time).
    Verified,
    /// No longer at its path.
    Missing,
    /// The content differs from what was recorded.
    Changed,
    /// The content matched another asset; the two were merged into this one.
    Merged { removed: AssetId },
    /// The source is offline, so nothing could be checked.
    Skipped,
}

impl Vault<'_> {
    /// Hashes one file and records the result.
    pub fn verify_file(&self, location: &FileLocation, control: &Control) -> Result<VerifyOutcome> {
        if location.source_status == SourceStatus::Offline || location.file.trash_path.is_some() {
            return Ok(VerifyOutcome::Skipped);
        }
        let path = location.path();
        control.report(Progress::Item {
            label: location.file.relpath.file_name_lossy(),
        });
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) | Err(_) => {
                if !location.source_path.as_path().is_dir() {
                    // The whole source vanished, e.g. a drive was unplugged.
                    return Ok(VerifyOutcome::Skipped);
                }
                if location.file.status != FileStatus::Missing {
                    self.write(|writer| {
                        writer.set_file_status(location.file.id, FileStatus::Missing)
                    })?;
                }
                return Ok(VerifyOutcome::Missing);
            }
        };
        let stamp = FileStamp::from_metadata(&metadata);
        let (hash, _) = match hash_file(&path, control) {
            Ok(result) => result,
            Err(VaultError::Cancelled) => return Err(VaultError::Cancelled),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "could not read file for verification");
                return Ok(VerifyOutcome::Skipped);
            }
        };
        let file = location.file.id;
        let restamp = stamp != location.file.stamp;
        let merged = self.write(|writer| {
            if restamp {
                writer.update_file_stamp(file, &stamp, location.file.status)?;
            }
            writer.set_file_hash(file, &hash)
        })?;
        control.report(Progress::Done);
        if let Some(removed) = merged {
            return Ok(VerifyOutcome::Merged { removed });
        }
        let status = self.writer().file(file)?.file.status;
        Ok(if status == FileStatus::Changed {
            VerifyOutcome::Changed
        } else {
            VerifyOutcome::Verified
        })
    }
}
