use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A filesystem path stored as raw bytes.
///
/// Linux filenames are arbitrary bytes and need not be valid UTF-8. Paths are
/// persisted in this form so every file can be found again exactly. Use
/// [`PathBytes::display`] for presentation only.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PathBytes(Vec<u8>);

impl PathBytes {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        Self(path.as_os_str().as_bytes().to_vec())
    }

    #[must_use]
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(OsStr::from_bytes(&self.0))
    }

    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        self.as_path().to_path_buf()
    }

    /// Lossy UTF-8 rendering for display. Never use the result to open files.
    #[must_use]
    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }

    /// The final component, rendered lossily.
    #[must_use]
    pub fn file_name_lossy(&self) -> String {
        self.as_path().file_name().map_or_else(
            || self.display(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Lowercased extension, if it is valid UTF-8.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        self.as_path()
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for PathBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_path())
    }
}

impl From<&Path> for PathBytes {
    fn from(path: &Path) -> Self {
        Self::from_path(path)
    }
}

impl From<PathBuf> for PathBytes {
    fn from(path: PathBuf) -> Self {
        Self(path.into_os_string().into_vec_bytes())
    }
}

trait IntoVecBytes {
    fn into_vec_bytes(self) -> Vec<u8>;
}

impl IntoVecBytes for std::ffi::OsString {
    fn into_vec_bytes(self) -> Vec<u8> {
        std::os::unix::ffi::OsStringExt::into_vec(self)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    #[test]
    fn non_utf8_names_round_trip_exactly() {
        let raw = b"/tmp/caf\xe9/IMG_\xff.JPG".to_vec();
        let path = PathBuf::from(OsString::from_vec(raw.clone()));
        let stored = PathBytes::from_path(&path);
        assert_eq!(stored.as_bytes(), raw.as_slice());
        assert_eq!(stored.to_path_buf(), path);
        assert_eq!(stored.extension().as_deref(), Some("jpg"));
        assert!(stored.display().contains('\u{fffd}'));
    }
}
