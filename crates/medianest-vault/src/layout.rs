//! Library folders, the manifest and the single-writer lock.
//!
//! ```text
//! <root>/
//!   Media/                 managed photos and videos, in YYYY/MM/DD folders
//!   .medianest/
//!     library.json         identity and format version
//!     catalog.sqlite3      the catalog
//!     backups/             catalog backups
//!     lock                 held while the library is open
//! ```

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use medianest_catalog::Writer;
use medianest_core::{LibraryUuid, PathBytes, now_ms};
use rustix::fs::FlockOperation;
use serde::{Deserialize, Serialize};

use crate::fsops::{network_filesystem, sync_dir};
use crate::{Result, VaultError};

pub const LIBRARY_FORMAT: &str = "medianest-library";
pub const LIBRARY_VERSION: u32 = 1;

/// Paths inside a library folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Managed media lives here.
    #[must_use]
    pub fn media(&self) -> PathBuf {
        self.root.join("Media")
    }

    #[must_use]
    pub fn internal(&self) -> PathBuf {
        self.root.join(".medianest")
    }

    #[must_use]
    pub fn catalog(&self) -> PathBuf {
        self.internal().join("catalog.sqlite3")
    }

    #[must_use]
    pub fn manifest(&self) -> PathBuf {
        self.internal().join("library.json")
    }

    #[must_use]
    pub fn backups(&self) -> PathBuf {
        self.internal().join("backups")
    }

    fn lock_path(&self) -> PathBuf {
        self.internal().join("lock")
    }

    /// Whether `path` is the library or inside it.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

/// Identity of a library, stored in `.medianest/library.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub uuid: LibraryUuid,
    pub created_ms: i64,
}

/// Held while a library is open. Dropping it releases the library.
#[derive(Debug)]
pub struct LibraryLock {
    _file: File,
}

impl LibraryLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(VaultError::io("could not open the lock file", path))?;
        rustix::fs::flock(file.as_fd(), FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| VaultError::Locked)?;
        Ok(Self { _file: file })
    }
}

/// An open library: its folders, identity, catalog writer and lock.
#[derive(Debug)]
pub struct OpenedLibrary {
    pub layout: Layout,
    pub manifest: Manifest,
    pub writer: Writer,
    pub lock: LibraryLock,
}

/// Whether a folder holds a MediaNest library.
#[must_use]
pub fn is_library(root: &Path) -> bool {
    Layout::new(root.to_path_buf()).manifest().is_file()
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let json = serde_json::to_vec_pretty(manifest).map_err(|error| VaultError::Io {
        action: "could not encode",
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;
    let temporary = path.with_extension("json.tmp");
    let mut file =
        File::create(&temporary).map_err(VaultError::io("could not write", &temporary))?;
    file.write_all(&json)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(VaultError::io("could not write", &temporary))?;
    fs::rename(&temporary, path).map_err(VaultError::io("could not write", path))?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(VaultError::io("could not resolve", path));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| VaultError::NotAFolder(path.to_path_buf()))?;
    Ok(parent
        .canonicalize()
        .map_err(VaultError::io("could not resolve", parent))?
        .join(name))
}

/// Creates a new library at `root`. The folder may exist and contain other files.
pub fn create(root: &Path) -> Result<OpenedLibrary> {
    let root = absolute(root)?;
    if is_library(&root) {
        return Err(VaultError::AlreadyALibrary(root));
    }
    fs::create_dir_all(&root).map_err(VaultError::io("could not create", &root))?;
    if let Some(kind) = network_filesystem(&root) {
        return Err(VaultError::NetworkFilesystem(kind));
    }
    let layout = Layout::new(root);
    for dir in [layout.media(), layout.internal(), layout.backups()] {
        fs::create_dir_all(&dir).map_err(VaultError::io("could not create", &dir))?;
    }
    let lock = LibraryLock::acquire(&layout.lock_path())?;
    let manifest = Manifest {
        format: LIBRARY_FORMAT.to_owned(),
        version: LIBRARY_VERSION,
        uuid: LibraryUuid::new(),
        created_ms: now_ms(),
    };
    let writer = match Writer::create(&layout.catalog(), manifest.uuid, &layout.media()) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = fs::remove_file(layout.catalog());
            return Err(error.into());
        }
    };
    write_manifest(&layout.manifest(), &manifest)?;
    sync_dir(layout.root())?;
    tracing::info!(root = %layout.root().display(), uuid = %manifest.uuid, "library created");
    Ok(OpenedLibrary {
        layout,
        manifest,
        writer,
        lock,
    })
}

/// Opens an existing library, taking its lock.
pub fn open(root: &Path) -> Result<OpenedLibrary> {
    let root = root
        .canonicalize()
        .map_err(|_| VaultError::NotALibrary(root.to_path_buf()))?;
    let layout = Layout::new(root.clone());
    let manifest_bytes =
        fs::read(layout.manifest()).map_err(|_| VaultError::NotALibrary(root.clone()))?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| VaultError::NotALibrary(root.clone()))?;
    if manifest.format != LIBRARY_FORMAT {
        return Err(VaultError::NotALibrary(root));
    }
    if manifest.version > LIBRARY_VERSION {
        return Err(VaultError::LibraryTooNew);
    }
    if let Some(kind) = network_filesystem(&root) {
        return Err(VaultError::NetworkFilesystem(kind));
    }
    fs::create_dir_all(layout.backups())
        .map_err(VaultError::io("could not create", layout.backups()))?;
    fs::create_dir_all(layout.media())
        .map_err(VaultError::io("could not create", layout.media()))?;
    let lock = LibraryLock::acquire(&layout.lock_path())?;
    let mut writer = Writer::open(&layout.catalog())?;
    if writer.library_uuid()? != manifest.uuid {
        return Err(VaultError::NotALibrary(root));
    }
    let managed = writer.managed_source()?;
    if managed.path != PathBytes::from_path(&layout.media()) {
        tracing::info!(from = %managed.path.display(), to = %layout.media().display(), "library moved; updating its location");
        writer.set_managed_root(&layout.media())?;
    }
    Ok(OpenedLibrary {
        layout,
        manifest,
        writer,
        lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_open_and_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("Library");
        let created = create(&root).expect("create");
        let uuid = created.manifest.uuid;
        assert!(is_library(&root));
        assert!(matches!(open(&root), Err(VaultError::Locked)));
        assert!(matches!(create(&root), Err(VaultError::AlreadyALibrary(_))));
        drop(created);
        let opened = open(&root).expect("open");
        assert_eq!(opened.manifest.uuid, uuid);
        assert!(opened.layout.media().is_dir());
    }

    #[test]
    fn moved_libraries_update_their_managed_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first");
        drop(create(&first).expect("create"));
        let second = dir.path().join("second");
        fs::rename(&first, &second).expect("move");
        let opened = open(&second).expect("open");
        let managed = opened.writer.managed_source().expect("managed");
        assert_eq!(managed.path.to_path_buf(), opened.layout.media());
    }

    #[test]
    fn rejects_folders_that_are_not_libraries() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(open(dir.path()), Err(VaultError::NotALibrary(_))));
    }
}
