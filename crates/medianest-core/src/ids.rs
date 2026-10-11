use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! row_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = std::num::ParseIntError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.trim().parse().map(Self)
            }
        }
    };
}

row_id!(
    /// A catalogued photo or video, independent of where its bytes live.
    AssetId
);
row_id!(
    /// One on-disk location of an asset.
    FileId
);
row_id!(
    /// The managed folder or a linked folder.
    SourceId
);
row_id!(
    /// A tag.
    TagId
);
row_id!(
    /// A manual or smart album.
    AlbumId
);
row_id!(
    /// An entry in the review inbox.
    ReviewId
);
row_id!(
    /// A background job recorded in the job history.
    JobId
);

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            #[must_use]
            pub fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(Uuid::from_bytes(bytes))
            }

            /// Parses the 16 raw bytes stored in the catalog.
            #[must_use]
            pub fn from_slice(bytes: &[u8]) -> Option<Self> {
                Uuid::from_slice(bytes).ok().map(Self)
            }

            #[must_use]
            pub fn as_bytes(&self) -> &[u8; 16] {
                self.0.as_bytes()
            }

            /// Lowercase hyphenated form without allocation-heavy formatting.
            #[must_use]
            pub fn simple(&self) -> String {
                self.0.simple().to_string()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s.trim()).map(Self)
            }
        }
    };
}

uuid_id!(
    /// Stable identity of a library across moves and backups.
    LibraryUuid
);
uuid_id!(
    /// Portable identity of an asset. Names its cached thumbnails.
    AssetUuid
);
uuid_id!(
    /// Identity of a journaled file operation. Appears in temporary file names.
    OperationUuid
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_ids_round_trip_through_strings() {
        let id: AssetId = " 42 ".parse().unwrap();
        assert_eq!(id, AssetId(42));
        assert_eq!(id.to_string(), "42");
    }

    #[test]
    fn uuid_ids_are_time_ordered() {
        let a = OperationUuid::new();
        let b = OperationUuid::new();
        assert!(a <= b);
        assert_eq!(OperationUuid::from_slice(a.as_bytes()), Some(a));
        assert_eq!(a.to_string().parse::<OperationUuid>().ok(), Some(a));
    }
}
