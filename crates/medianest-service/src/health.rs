//! One overview of how the library is doing.

use medianest_catalog::{LibraryStats, Source};
use medianest_core::{ReviewKind, SourceKind, SourceStatus, now_ms};
use medianest_vault::BackupInfo;
use serde::Serialize;

use crate::{Library, Result};

/// Disk space used by the thumbnail and preview cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CacheUsage {
    pub thumbnails: u64,
    pub previews: u64,
}

/// Something that needs attention, worded for people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub title: String,
    pub detail: String,
    /// The review category that resolves it, if any.
    pub review: Option<ReviewKind>,
}

/// The health page in one structure.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HealthReport {
    pub stats: LibraryStats,
    pub sources: Vec<Source>,
    pub database_ok: bool,
    pub database_message: String,
    pub backups: Vec<BackupInfo>,
    pub cache: CacheUsage,
    pub video_tools: bool,
    pub last_verified_ms: Option<i64>,
    pub reviews: Vec<(ReviewKind, u64)>,
    pub issues: Vec<Issue>,
}

impl HealthReport {
    /// A single line for the sidebar: "All 12,480 files verified" and similar.
    #[must_use]
    pub fn headline(&self) -> String {
        if !self.database_ok {
            return "The catalog needs repair".into();
        }
        if !self.issues.is_empty() {
            return match self.issues.len() {
                1 => self.issues[0].title.clone(),
                n => format!("{n} things need attention"),
            };
        }
        if self.stats.unverified_files > 0 {
            return format!(
                "Checking {} new files",
                group_digits(self.stats.unverified_files)
            );
        }
        format!("All {} files verified", group_digits(self.stats.files))
    }
}

/// `12480` → `12,480`.
#[must_use]
pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

impl Library {
    /// Gathers everything the health page shows. Runs a quick database check.
    pub fn health(&self) -> Result<HealthReport> {
        let (stats, sources, check, reviews) = self.read(|db| {
            Ok((
                db.stats()?,
                db.sources()?,
                db.quick_check()?,
                db.review_counts()?,
            ))
        })?;
        let backups = self.backups().unwrap_or_default();
        let (thumbnails, previews) = self.inner.cache.usage();
        let mut issues = Vec::new();
        if stats.missing_files > 0 {
            issues.push(Issue {
                title: format!("{} files are missing", group_digits(stats.missing_files)),
                detail: "They were moved or deleted outside MediaNest.".into(),
                review: Some(ReviewKind::MissingFile),
            });
        }
        if stats.changed_files > 0 {
            issues.push(Issue {
                title: format!("{} files changed", group_digits(stats.changed_files)),
                detail: "Their content differs from when they were checked.".into(),
                review: Some(ReviewKind::ChangedFile),
            });
        }
        for source in sources
            .iter()
            .filter(|s| s.kind == SourceKind::Linked && s.status == SourceStatus::Offline)
        {
            issues.push(Issue {
                title: format!("{} is not connected", source.name),
                detail: format!(
                    "{} is unavailable. Connect the drive or tell MediaNest where it moved.",
                    source.path.display()
                ),
                review: None,
            });
        }
        let pending = reviews
            .iter()
            .find(|(kind, _)| *kind == ReviewKind::PendingRemoval)
            .map_or(0, |(_, count)| *count);
        if pending > 0 {
            issues.push(Issue {
                title: format!("{pending} originals wait for removal"),
                detail: "Moves are copied and verified; approve removing the originals.".into(),
                review: Some(ReviewKind::PendingRemoval),
            });
        }
        if backups
            .first()
            .is_none_or(|backup| now_ms() - backup.created_ms > 7 * 86_400_000)
            && stats.assets > 0
        {
            issues.push(Issue {
                title: "No recent catalog backup".into(),
                detail: "Back up the catalog to keep your tags, albums and dates safe.".into(),
                review: None,
            });
        }
        Ok(HealthReport {
            database_ok: check == "ok",
            database_message: check,
            last_verified_ms: stats.oldest_check_ms,
            stats,
            sources,
            backups,
            cache: CacheUsage {
                thumbnails,
                previews,
            },
            video_tools: self.inner.toolkit.video.available(),
            reviews,
            issues,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(12480), "12,480");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}
