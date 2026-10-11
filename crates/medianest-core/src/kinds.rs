use std::fmt;

use serde::{Deserialize, Serialize};

/// A persisted integer or string did not match any known variant.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value}")]
pub struct UnknownValue {
    pub kind: &'static str,
    pub value: String,
}

int_enum!(
    /// Whether an asset is a still image or a moving one.
    MediaKind {
        Photo = 1,
        Video = 2,
    }
);

int_enum!(
    /// What a source folder is.
    SourceKind {
        /// The library's own `Media/` folder. Files here are owned by MediaNest.
        Managed = 1,
        /// A folder indexed in place. Files here belong to the user.
        Linked = 2,
    }
);

int_enum!(
    /// Reachability of a source folder.
    SourceStatus {
        Available = 1,
        /// The folder could not be reached, for example an unplugged drive.
        Offline = 2,
    }
);

int_enum!(
    /// Integrity state of one file location.
    FileStatus {
        /// Catalogued but not hashed yet.
        Unverified = 1,
        /// Content hash recorded and matching.
        Verified = 2,
        /// The file is no longer at its path.
        Missing = 3,
        /// The bytes differ from what was recorded.
        Changed = 4,
    }
);

int_enum!(
    /// Category of a review-inbox entry.
    ReviewKind {
        /// The capture date could only be guessed.
        NeedsDate = 1,
        /// A file disappeared from its location.
        MissingFile = 2,
        /// A file's bytes changed outside MediaNest.
        ChangedFile = 3,
        /// An import could not bring a file in.
        FailedImport = 4,
        /// A move import finished copying; the original waits for removal approval.
        PendingRemoval = 5,
        /// Crash recovery could not decide an operation's outcome on its own.
        InterruptedOperation = 6,
        /// A file could not be read or decoded.
        Unreadable = 7,
    }
);

int_enum!(
    /// Resolution of a review-inbox entry.
    ReviewState {
        Open = 0,
        Resolved = 1,
        Dismissed = 2,
    }
);

int_enum!(
    /// What a journaled file operation does.
    OperationKind {
        /// Copy a file into the managed folder.
        CopyIn = 1,
        /// Copy a file into the managed folder, then remove the original after approval.
        MoveIn = 2,
    }
);

int_enum!(
    /// Durable checkpoints of a file operation, in order.
    OperationState {
        /// Recorded before any file is touched.
        Planned = 1,
        /// A temporary copy is being written next to the destination.
        Copying = 2,
        /// The temporary copy matches the source byte for byte.
        Verified = 3,
        /// The copy is at its final path.
        Installed = 4,
        /// The catalog references the installed copy.
        Catalogued = 5,
        /// A move waits for the user to approve removing the original.
        AwaitingRemoval = 6,
        /// The original was renamed aside and is about to be removed.
        Removing = 7,
        /// Finished successfully.
        Completed = 8,
        /// Stopped safely; the original is untouched.
        Failed = 9,
        /// Recovery could not decide; a person has to look.
        NeedsReview = 10,
        /// Stopped at the user's request before installation.
        Cancelled = 11,
    }
);

impl OperationState {
    /// The operation needs no further work.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

int_enum!(
    /// Progress of thumbnail generation for an asset.
    ThumbState {
        Pending = 0,
        Ready = 1,
        Failed = 2,
    }
);

int_enum!(
    /// How an album orders its contents.
    AlbumSort {
        OldestFirst = 0,
        NewestFirst = 1,
        Manual = 2,
    }
);

impl ReviewKind {
    /// Short plural heading used by the review inbox.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::NeedsDate => "Needs a date",
            Self::MissingFile => "Missing files",
            Self::ChangedFile => "Changed files",
            Self::FailedImport => "Failed imports",
            Self::PendingRemoval => "Originals waiting for removal",
            Self::InterruptedOperation => "Interrupted operations",
            Self::Unreadable => "Unreadable files",
        }
    }

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::NeedsDate => "needs-date",
            Self::MissingFile => "missing",
            Self::ChangedFile => "changed",
            Self::FailedImport => "failed-import",
            Self::PendingRemoval => "pending-removal",
            Self::InterruptedOperation => "interrupted",
            Self::Unreadable => "unreadable",
        }
    }
}

/// File formats MediaNest understands, identified by content and extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Gif,
    Tiff,
    Heic,
    Avif,
    Jxl,
    Dng,
    Cr2,
    Cr3,
    Nef,
    Arw,
    Orf,
    Rw2,
    Raf,
    Pef,
    Srw,
    Mp4,
    Mov,
    Mkv,
    Webm,
    Avi,
    M2ts,
    ThreeGp,
}

impl Format {
    pub const ALL: &'static [Self] = &[
        Self::Jpeg,
        Self::Png,
        Self::Webp,
        Self::Gif,
        Self::Tiff,
        Self::Heic,
        Self::Avif,
        Self::Jxl,
        Self::Dng,
        Self::Cr2,
        Self::Cr3,
        Self::Nef,
        Self::Arw,
        Self::Orf,
        Self::Rw2,
        Self::Raf,
        Self::Pef,
        Self::Srw,
        Self::Mp4,
        Self::Mov,
        Self::Mkv,
        Self::Webm,
        Self::Avi,
        Self::M2ts,
        Self::ThreeGp,
    ];

    /// Stable lowercase name stored in the catalog.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Tiff => "tiff",
            Self::Heic => "heic",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            Self::Dng => "dng",
            Self::Cr2 => "cr2",
            Self::Cr3 => "cr3",
            Self::Nef => "nef",
            Self::Arw => "arw",
            Self::Orf => "orf",
            Self::Rw2 => "rw2",
            Self::Raf => "raf",
            Self::Pef => "pef",
            Self::Srw => "srw",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Avi => "avi",
            Self::M2ts => "m2ts",
            Self::ThreeGp => "3gp",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, UnknownValue> {
        Self::ALL
            .iter()
            .copied()
            .find(|format| format.code() == code)
            .ok_or_else(|| UnknownValue {
                kind: "Format",
                value: code.to_owned(),
            })
    }

    /// Format implied by a file extension, case-insensitively.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let lower = extension.to_ascii_lowercase();
        Some(match lower.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::Webp,
            "gif" => Self::Gif,
            "tif" | "tiff" => Self::Tiff,
            "heic" | "heif" | "hif" => Self::Heic,
            "avif" => Self::Avif,
            "jxl" => Self::Jxl,
            "dng" => Self::Dng,
            "cr2" => Self::Cr2,
            "cr3" => Self::Cr3,
            "nef" | "nrw" => Self::Nef,
            "arw" | "srf" | "sr2" => Self::Arw,
            "orf" => Self::Orf,
            "rw2" => Self::Rw2,
            "raf" => Self::Raf,
            "pef" => Self::Pef,
            "srw" => Self::Srw,
            "mp4" | "m4v" => Self::Mp4,
            "mov" | "qt" => Self::Mov,
            "mkv" => Self::Mkv,
            "webm" => Self::Webm,
            "avi" => Self::Avi,
            "mts" | "m2ts" => Self::M2ts,
            "3gp" | "3g2" => Self::ThreeGp,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn kind(self) -> MediaKind {
        match self {
            Self::Mp4
            | Self::Mov
            | Self::Mkv
            | Self::Webm
            | Self::Avi
            | Self::M2ts
            | Self::ThreeGp => MediaKind::Video,
            _ => MediaKind::Photo,
        }
    }

    #[must_use]
    pub const fn is_raw(self) -> bool {
        matches!(
            self,
            Self::Dng
                | Self::Cr2
                | Self::Cr3
                | Self::Nef
                | Self::Arw
                | Self::Orf
                | Self::Rw2
                | Self::Raf
                | Self::Pef
                | Self::Srw
        )
    }

    /// RAW formats built on the TIFF container, whose metadata a TIFF reader can parse.
    #[must_use]
    pub const fn is_tiff_based(self) -> bool {
        matches!(
            self,
            Self::Tiff
                | Self::Dng
                | Self::Cr2
                | Self::Nef
                | Self::Arw
                | Self::Orf
                | Self::Rw2
                | Self::Pef
                | Self::Srw
        )
    }

    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Tiff => "image/tiff",
            Self::Heic => "image/heif",
            Self::Avif => "image/avif",
            Self::Jxl => "image/jxl",
            Self::Dng => "image/x-adobe-dng",
            Self::Cr2 => "image/x-canon-cr2",
            Self::Cr3 => "image/x-canon-cr3",
            Self::Nef => "image/x-nikon-nef",
            Self::Arw => "image/x-sony-arw",
            Self::Orf => "image/x-olympus-orf",
            Self::Rw2 => "image/x-panasonic-rw2",
            Self::Raf => "image/x-fuji-raf",
            Self::Pef => "image/x-pentax-pef",
            Self::Srw => "image/x-samsung-srw",
            Self::Mp4 => "video/mp4",
            Self::Mov => "video/quicktime",
            Self::Mkv => "video/x-matroska",
            Self::Webm => "video/webm",
            Self::Avi => "video/x-msvideo",
            Self::M2ts => "video/mp2t",
            Self::ThreeGp => "video/3gpp",
        }
    }

    /// Human-readable name for the info panel.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Gif => "GIF",
            Self::Tiff => "TIFF",
            Self::Heic => "HEIF",
            Self::Avif => "AVIF",
            Self::Jxl => "JPEG XL",
            Self::Dng => "DNG RAW",
            Self::Cr2 => "Canon RAW",
            Self::Cr3 => "Canon RAW",
            Self::Nef => "Nikon RAW",
            Self::Arw => "Sony RAW",
            Self::Orf => "Olympus RAW",
            Self::Rw2 => "Panasonic RAW",
            Self::Raf => "Fujifilm RAW",
            Self::Pef => "Pentax RAW",
            Self::Srw => "Samsung RAW",
            Self::Mp4 => "MPEG-4 video",
            Self::Mov => "QuickTime video",
            Self::Mkv => "Matroska video",
            Self::Webm => "WebM video",
            Self::Avi => "AVI video",
            Self::M2ts => "AVCHD video",
            Self::ThreeGp => "3GP video",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_enums_round_trip() {
        for kind in ReviewKind::ALL {
            assert_eq!(ReviewKind::from_code(kind.code()), Ok(*kind));
        }
        assert!(FileStatus::from_code(99).is_err());
    }

    #[test]
    fn formats_round_trip_and_classify() {
        for format in Format::ALL {
            assert_eq!(Format::from_code(format.code()), Ok(*format));
        }
        assert_eq!(Format::from_extension("JPG"), Some(Format::Jpeg));
        assert_eq!(Format::from_extension("m4v"), Some(Format::Mp4));
        assert_eq!(Format::from_extension("txt"), None);
        assert_eq!(Format::Mov.kind(), MediaKind::Video);
        assert!(Format::Nef.is_raw() && Format::Nef.is_tiff_based());
        assert!(!Format::Cr3.is_tiff_based());
    }
}
