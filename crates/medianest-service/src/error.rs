use medianest_catalog::CatalogError;
use medianest_vault::VaultError;
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors reported to the user interface. Messages are written for people.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Probe(#[from] medianest_media::ProbeError),
    #[error(transparent)]
    Thumbnail(#[from] medianest_media::ThumbnailError),
    #[error(transparent)]
    Decode(#[from] medianest_media::DecodeError),
    #[error("could not read or write {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
    #[error(
        "Stopped after {done} of {total} items: the library's disk is full. \
         Free some space and import again to continue."
    )]
    DiskFull { done: u64, total: usize },
    #[error("the library was closed")]
    Closed,
    #[error("stopped")]
    Cancelled,
}

impl Error {
    pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    /// Whether the user stopped the work on purpose.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Vault(VaultError::Cancelled))
    }
}
