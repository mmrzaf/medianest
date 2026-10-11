use std::path::PathBuf;

use medianest_catalog::CatalogError;
use thiserror::Error;

pub type Result<T, E = VaultError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not a MediaNest library")]
    NotALibrary(PathBuf),
    #[error("{0} already contains a MediaNest library")]
    AlreadyALibrary(PathBuf),
    #[error("this library was created by a newer version of MediaNest")]
    LibraryTooNew,
    #[error("the library is open in another MediaNest window or program")]
    Locked,
    #[error("libraries cannot live on network storage ({0}); choose a local or USB disk")]
    NetworkFilesystem(&'static str),
    #[error("{0} is inside the library; add it from outside the library instead")]
    InsideLibrary(PathBuf),
    #[error("the library is inside {0}; a folder cannot contain its own library")]
    ContainsLibrary(PathBuf),
    #[error("{0} is not a folder")]
    NotAFolder(PathBuf),
    #[error("{0} is a symbolic link; MediaNest only imports regular files")]
    Symlink(PathBuf),
    #[error("{0} changed while it was being copied; it was left untouched")]
    SourceChanged(PathBuf),
    #[error("the copy of {0} did not match the original; nothing was installed")]
    VerificationFailed(PathBuf),
    #[error("{0} is no longer the file that was imported; it was not removed")]
    SourceReplaced(PathBuf),
    #[error("the library copy is missing or damaged, so the original was kept")]
    CopyNotVerified,
    #[error("this operation cannot be approved in its current state")]
    InvalidState,
    #[error("stopped")]
    Cancelled,
    #[error(transparent)]
    Probe(#[from] medianest_media::ProbeError),
    #[error("could not use the trash: {0}")]
    Trash(String),
    /// A simulated crash from fault-injection tests.
    #[error("simulated interruption at {0:?}")]
    Interrupted(crate::faults::Checkpoint),
}

impl VaultError {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}
