//! Checkpoints where tests can simulate a crash.

/// Durable steps of a managed import and of source removal.
///
/// Not every import reaches every step: a rename move has no temporary copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Checkpoint {
    AfterPlan,
    AfterTempCreated,
    AfterCopy,
    AfterVerified,
    AfterRename,
    AfterInstall,
    AfterCatalogued,
    AfterAside,
}

impl Checkpoint {
    pub const ALL: &'static [Self] = &[
        Self::AfterPlan,
        Self::AfterTempCreated,
        Self::AfterCopy,
        Self::AfterVerified,
        Self::AfterRename,
        Self::AfterInstall,
        Self::AfterCatalogued,
        Self::AfterAside,
    ];
}

/// Decides whether to simulate a crash at a checkpoint.
pub trait Faults: Send + Sync {
    fn crash_at(&self, checkpoint: Checkpoint) -> bool;

    /// Makes a rename fail as if the destination were on another filesystem.
    fn rename_crosses_devices(&self) -> bool {
        false
    }
}

/// Production behaviour: never crash on purpose.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFaults;

impl Faults for NoFaults {
    fn crash_at(&self, _: Checkpoint) -> bool {
        false
    }
}

/// Crashes at exactly one checkpoint.
#[derive(Clone, Copy, Debug)]
pub struct CrashAt(pub Checkpoint);

impl Faults for CrashAt {
    fn crash_at(&self, checkpoint: Checkpoint) -> bool {
        self.0 == checkpoint
    }
}

/// Renames fail as if the library were on another filesystem than the source.
#[derive(Clone, Copy, Debug)]
pub struct CrossDevice;

impl Faults for CrossDevice {
    fn crash_at(&self, _: Checkpoint) -> bool {
        false
    }

    fn rename_crosses_devices(&self) -> bool {
        true
    }
}

pub(crate) fn hit(faults: &dyn Faults, checkpoint: Checkpoint) -> crate::Result<()> {
    if faults.crash_at(checkpoint) {
        return Err(crate::VaultError::Interrupted(checkpoint));
    }
    Ok(())
}
