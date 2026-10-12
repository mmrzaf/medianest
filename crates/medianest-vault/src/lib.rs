//! Safe file operations for a MediaNest library.
//!
//! Every change to a file goes through this crate and follows the same rules:
//!
//! - Originals are read, never modified.
//! - Nothing is ever overwritten: files are installed with no-replace renames.
//! - Copies are verified byte for byte (BLAKE3) before they count.
//! - Each durable step is journaled first, so an interruption at any point can
//!   be recovered without guessing.
//! - Originals are removed only after the copy is verified and they are
//!   re-checked to still be the very file that was copied; otherwise they are
//!   kept for the user to decide.
//! - Deleting means moving to the system trash, which can be undone.

mod backup;
mod control;
mod error;
pub mod faults;
mod fsops;
mod import;
mod layout;
mod naming;
mod recovery;
mod removal;
mod scan;
mod trash;
mod verify;

use std::sync::{Mutex, MutexGuard, PoisonError};

use medianest_catalog::Writer;

pub use backup::{BackupInfo, create_backup, list_backups, prune_backups, restore_backup};
pub use control::{Control, Progress};
pub use error::{Result, VaultError};
pub use faults::{Checkpoint, CrashAt, Faults, NoFaults};
pub use fsops::{hash_file, network_filesystem};
pub use import::{ImportMode, ImportOutcome};
pub use layout::{Layout, LibraryLock, Manifest, OpenedLibrary, create, is_library, open};
pub use naming::managed_relpath;
pub use recovery::RecoveryReport;
pub use scan::{ScanReport, is_ignored_name};
pub use trash::{LinkedPolicy, TrashReport};
pub use verify::VerifyOutcome;

/// Performs file operations for one open library.
pub struct Vault<'a> {
    pub layout: &'a Layout,
    pub catalog: &'a Mutex<Writer>,
    pub faults: &'a dyn Faults,
}

impl std::fmt::Debug for Vault<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl<'a> Vault<'a> {
    #[must_use]
    pub fn new(layout: &'a Layout, catalog: &'a Mutex<Writer>) -> Self {
        Self {
            layout,
            catalog,
            faults: &NoFaults,
        }
    }

    /// Short exclusive access to the catalog writer.
    pub(crate) fn writer(&self) -> MutexGuard<'_, Writer> {
        self.catalog.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn write<R>(
        &self,
        action: impl FnOnce(&mut Writer) -> medianest_catalog::Result<R>,
    ) -> Result<R> {
        let mut writer = self.writer();
        Ok(action(&mut writer)?)
    }
}
