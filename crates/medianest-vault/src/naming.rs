//! Where managed copies go.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use medianest_core::{AssetMetadata, PathBytes};

/// The folder for unknown or guessed dates.
pub const UNSORTED: &str = "Unsorted";

fn clean_name(name: &[u8]) -> Vec<u8> {
    let trimmed: &[u8] = {
        let start = name
            .iter()
            .position(|byte| *byte != b'.')
            .unwrap_or(name.len());
        &name[start..]
    };
    if trimmed.is_empty() {
        b"unnamed".to_vec()
    } else {
        trimmed.to_vec()
    }
}

/// Destination of a managed copy, relative to `Media/`: `YYYY/MM/DD/name`
/// when the capture date is trustworthy, otherwise `Unsorted/name`.
#[must_use]
pub fn managed_relpath(metadata: &AssetMetadata, file_name: &OsStr) -> PathBytes {
    let name = clean_name(file_name.as_bytes());
    let folder = match metadata
        .taken
        .local()
        .filter(|_| metadata.taken.is_trustworthy())
    {
        Some(local) => local.format("%Y/%m/%d").to_string(),
        None => UNSORTED.to_owned(),
    };
    let mut bytes = folder.into_bytes();
    bytes.push(b'/');
    bytes.extend_from_slice(&name);
    PathBytes::from_vec(bytes)
}

/// The same path with a short content hash before the extension, used when a
/// different file already has the plain name: `IMG_1.jpg` → `IMG_1~a1b2c3d4.jpg`.
#[must_use]
pub fn with_hash_suffix(relpath: &PathBytes, hash: &[u8; 32]) -> PathBytes {
    let path = relpath.as_path();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path.file_stem().map_or(&[][..], OsStr::as_bytes);
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    );
    let mut name = stem.to_vec();
    name.push(b'~');
    name.extend_from_slice(suffix.as_bytes());
    if let Some(extension) = path.extension() {
        name.push(b'.');
        name.extend_from_slice(extension.as_bytes());
    }
    PathBytes::from_path(&parent.join(OsStr::from_bytes(&name)))
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDateTime;
    use medianest_core::{CaptureTime, Confidence, DateSource, Format};

    use super::*;

    fn metadata(source: DateSource, confidence: Confidence) -> AssetMetadata {
        let mut meta = AssetMetadata::minimal(Format::Jpeg);
        let local = NaiveDateTime::parse_from_str("2024-05-10 18:00:00", "%Y-%m-%d %H:%M:%S")
            .expect("date");
        meta.taken = CaptureTime::floating(local, source, confidence);
        meta
    }

    #[test]
    fn trustworthy_dates_get_dated_folders() {
        let path = managed_relpath(
            &metadata(DateSource::Exif, Confidence::Medium),
            OsStr::new("DSC04400.JPG"),
        );
        assert_eq!(path.display(), "2024/05/10/DSC04400.JPG");
    }

    #[test]
    fn guessed_dates_go_to_unsorted() {
        let path = managed_relpath(
            &metadata(DateSource::FileModified, Confidence::Low),
            OsStr::new(".hidden.jpg"),
        );
        assert_eq!(path.display(), "Unsorted/hidden.jpg");
    }

    #[test]
    fn collisions_get_a_hash_suffix() {
        let path = PathBytes::from_vec(b"2024/05/10/IMG_1.jpg".to_vec());
        let renamed = with_hash_suffix(&path, &[0xab; 32]);
        assert_eq!(renamed.display(), "2024/05/10/IMG_1~abababab.jpg");
    }
}
