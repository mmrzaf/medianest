//! Careful filesystem primitives.

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::Path;

use rustix::fs::{CWD, Mode, OFlags, RenameFlags};

use crate::control::{Control, Progress};
use crate::{Result, VaultError};

const BUFFER: usize = 1 << 20;

/// Opens an existing regular file for reading without following a final symlink.
pub fn open_regular(path: &Path) -> Result<(File, Metadata)> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| {
        if errno == rustix::io::Errno::LOOP {
            VaultError::Symlink(path.to_path_buf())
        } else {
            VaultError::Io {
                action: "could not open",
                path: path.to_path_buf(),
                source: errno.into(),
            }
        }
    })?;
    let file = File::from(fd);
    let metadata = file
        .metadata()
        .map_err(VaultError::io("could not inspect", path))?;
    if !metadata.is_file() {
        return Err(VaultError::Io {
            action: "not a regular file:",
            path: path.to_path_buf(),
            source: io::Error::from(io::ErrorKind::InvalidInput),
        });
    }
    Ok((file, metadata))
}

/// Streams `reader` to completion, hashing with BLAKE3. Returns the hash and byte count.
pub fn hash_reader(mut reader: impl Read, control: &Control) -> Result<([u8; 32], u64)> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; BUFFER];
    let mut total = 0u64;
    loop {
        control.checkpoint()?;
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(VaultError::Io {
                    action: "could not read",
                    path: Default::default(),
                    source: error,
                });
            }
        };
        hasher.update(&buffer[..read]);
        total += read as u64;
        control.report(Progress::Bytes(read as u64));
    }
    Ok((*hasher.finalize().as_bytes(), total))
}

/// Hashes a file's full content.
pub fn hash_file(path: &Path, control: &Control) -> Result<([u8; 32], u64)> {
    let (file, _) = open_regular(path)?;
    hash_reader(file, control).map_err(|error| with_path(error, path))
}

fn with_path(error: VaultError, path: &Path) -> VaultError {
    match error {
        VaultError::Io {
            action,
            path: empty,
            source,
        } if empty.as_os_str().is_empty() => VaultError::Io {
            action,
            path: path.to_path_buf(),
            source,
        },
        other => other,
    }
}

/// Copies `source` into `destination`, hashing while streaming, and syncs the
/// destination to disk. Returns the hash and byte count of what was read.
pub fn copy_hashed(
    source: &mut File,
    destination: &mut File,
    control: &Control,
    destination_path: &Path,
) -> Result<([u8; 32], u64)> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; BUFFER];
    let mut total = 0u64;
    loop {
        control.checkpoint()?;
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(VaultError::Io {
                    action: "could not read the original",
                    path: destination_path.to_path_buf(),
                    source: error,
                });
            }
        };
        hasher.update(&buffer[..read]);
        destination
            .write_all(&buffer[..read])
            .map_err(VaultError::io("could not write", destination_path))?;
        total += read as u64;
        control.report(Progress::Bytes(read as u64));
    }
    destination
        .sync_all()
        .map_err(VaultError::io("could not flush", destination_path))?;
    Ok((*hasher.finalize().as_bytes(), total))
}

/// Flushes a directory entry change (create, rename, unlink) to disk.
pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(VaultError::io("could not flush folder", path))
}

/// Renames `from` to `to`, failing if `to` exists. Never overwrites.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    match rustix::fs::renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(errno) if errno == rustix::io::Errno::INVAL || errno == rustix::io::Errno::NOSYS => {
            // The filesystem lacks atomic no-replace renames: a hard link
            // also refuses to replace, then the old name is removed.
            fs::hard_link(from, to)?;
            fs::remove_file(from)
        }
        Err(errno) => Err(errno.into()),
    }
}

/// Creates a new file that must not already exist.
pub fn create_new(path: &Path) -> Result<File> {
    File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(VaultError::io("could not create", path))
}

/// Filesystems where SQLite locking and durable writes are unreliable.
pub fn network_filesystem(path: &Path) -> Option<&'static str> {
    let stat = rustix::fs::statfs(path).ok()?;
    #[allow(clippy::cast_sign_loss, clippy::unnecessary_cast)]
    let kind = stat.f_type as u64;
    match kind {
        0x6969 => Some("NFS"),
        0x517B => Some("SMB"),
        0xFF53_4D42 => Some("CIFS"),
        0xFE53_4D42 => Some("SMB2"),
        0x0102_1997 => Some("9P"),
        0x5346_414F => Some("AFS"),
        0x7375_7245 => Some("Coda"),
        0x0BD0_0BD0 => Some("Lustre"),
        0x1937_0128 => Some("GlusterFS"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_and_hashes_identically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("a.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&source, &data).expect("write");
        let (mut input, _) = open_regular(&source).expect("open");
        let target = dir.path().join("b.bin");
        let mut output = create_new(&target).expect("create");
        let (hash, size) =
            copy_hashed(&mut input, &mut output, &Control::new(), &target).expect("copy");
        assert_eq!(size, data.len() as u64);
        assert_eq!(hash, *blake3::hash(&data).as_bytes());
        assert_eq!(hash_file(&target, &Control::new()).expect("hash").0, hash);
        assert!(create_new(&target).is_err());
    }

    #[test]
    fn renames_never_replace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"a").expect("write");
        fs::write(&b, b"b").expect("write");
        assert!(rename_no_replace(&a, &b).is_err());
        assert_eq!(fs::read(&b).expect("read"), b"b");
        let c = dir.path().join("c");
        rename_no_replace(&a, &c).expect("rename");
        assert!(!a.exists() && c.exists());
    }

    #[test]
    fn refuses_symlinks_and_cancels() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        fs::write(&real, b"x").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(matches!(open_regular(&link), Err(VaultError::Symlink(_))));
        let control = Control::new();
        control.cancel();
        assert!(matches!(
            hash_file(&real, &control),
            Err(VaultError::Cancelled)
        ));
    }
}
