use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use serde::{Deserialize, Serialize};

int_enum!(
    /// Where a capture time came from, from least to most trusted.
    DateSource {
        /// No date could be found.
        Unknown = 0,
        /// The file's last-modified time. Often the copy time, not the capture time.
        FileModified = 1,
        /// A date embedded in the filename, such as `PXL_20230701_101500.jpg`.
        FileName = 2,
        /// Video container metadata.
        Container = 3,
        /// EXIF original capture time without a timezone offset.
        Exif = 4,
        /// EXIF original capture time with a timezone offset.
        ExifWithOffset = 5,
        /// Set by the user.
        Manual = 6,
    }
);

int_enum!(
    /// How much a capture time can be trusted.
    Confidence {
        Unknown = 0,
        /// A guess, such as a file-modified time.
        Low = 1,
        /// Probably right: camera wall time without a timezone, or a filename date.
        Medium = 2,
        /// Recorded by the device with full timezone information, or set by the user.
        High = 3,
    }
);

impl DateSource {
    /// Plain-language description for the info panel.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Unknown => "No date found",
            Self::FileModified => "Guessed from the file's modified date",
            Self::FileName => "Read from the file name",
            Self::Container => "From the video's metadata",
            Self::Exif => "From the camera",
            Self::ExifWithOffset => "From the camera, with time zone",
            Self::Manual => "Set by you",
        }
    }
}

/// When a photo or video was captured.
///
/// The primary value is the *local wall-clock time* at the place of capture,
/// stored as milliseconds since the epoch as if that wall time were UTC. This
/// is what people expect to see and how the timeline groups days. When the
/// timezone offset is known, the absolute instant is also available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureTime {
    /// Wall-clock time at capture, encoded as if it were UTC.
    pub local_ms: Option<i64>,
    /// Offset of the wall clock from UTC, in minutes east.
    pub offset_minutes: Option<i32>,
    pub source: DateSource,
    pub confidence: Confidence,
}

impl CaptureTime {
    pub const UNKNOWN: Self = Self {
        local_ms: None,
        offset_minutes: None,
        source: DateSource::Unknown,
        confidence: Confidence::Unknown,
    };

    /// A wall-clock time without timezone.
    #[must_use]
    pub fn floating(local: NaiveDateTime, source: DateSource, confidence: Confidence) -> Self {
        Self {
            local_ms: Some(local_ms_from_naive(local)),
            offset_minutes: None,
            source,
            confidence,
        }
    }

    /// A wall-clock time with a known offset from UTC.
    #[must_use]
    pub fn zoned(
        local: NaiveDateTime,
        offset_minutes: i32,
        source: DateSource,
        confidence: Confidence,
    ) -> Self {
        Self {
            local_ms: Some(local_ms_from_naive(local)),
            offset_minutes: Some(offset_minutes),
            source,
            confidence,
        }
    }

    /// An absolute instant shown in the given timezone.
    #[must_use]
    pub fn from_instant<Tz: TimeZone>(
        instant: DateTime<Tz>,
        source: DateSource,
        confidence: Confidence,
    ) -> Self {
        let offset_seconds = instant.offset().fix().local_minus_utc();
        Self::zoned(
            instant.naive_local(),
            offset_seconds / 60,
            source,
            confidence,
        )
    }

    #[must_use]
    pub const fn is_known(&self) -> bool {
        self.local_ms.is_some()
    }

    /// Good enough to file a managed copy under a dated folder.
    #[must_use]
    pub fn is_trustworthy(&self) -> bool {
        self.local_ms.is_some() && self.confidence >= Confidence::Medium
    }

    /// The absolute instant, when the timezone offset is known.
    #[must_use]
    pub fn utc_ms(&self) -> Option<i64> {
        let local = self.local_ms?;
        let offset = self.offset_minutes?;
        Some(local - i64::from(offset) * 60_000)
    }

    #[must_use]
    pub fn local(&self) -> Option<NaiveDateTime> {
        self.local_ms.and_then(naive_from_local_ms)
    }

    #[must_use]
    pub fn day(&self) -> Option<Day> {
        self.local_ms.map(Day::from_local_ms)
    }

    /// Returns `self` shifted by a number of milliseconds, keeping its offset.
    #[must_use]
    pub fn shifted(self, delta_ms: i64) -> Self {
        Self {
            local_ms: self.local_ms.map(|value| value.saturating_add(delta_ms)),
            ..self
        }
    }
}

impl Default for CaptureTime {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

/// Encodes a wall-clock time as milliseconds, treating it as UTC.
#[must_use]
pub fn local_ms_from_naive(local: NaiveDateTime) -> i64 {
    local.and_utc().timestamp_millis()
}

#[must_use]
pub fn naive_from_local_ms(value: i64) -> Option<NaiveDateTime> {
    DateTime::<Utc>::from_timestamp_millis(value).map(|instant| instant.naive_utc())
}

/// A calendar day in local wall-clock time, counted from 1970-01-01.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Day(pub i32);

impl Day {
    const MS_PER_DAY: i64 = 86_400_000;

    #[must_use]
    pub fn from_local_ms(value: i64) -> Self {
        let days = value.div_euclid(Self::MS_PER_DAY);
        Self(i32::try_from(days).unwrap_or(if days < 0 { i32::MIN } else { i32::MAX }))
    }

    #[must_use]
    pub fn from_date(date: NaiveDate) -> Self {
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or(NaiveDate::MIN);
        let days = (date - epoch).num_days();
        Self(i32::try_from(days).unwrap_or(0))
    }

    #[must_use]
    pub fn date(self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(1970, 1, 1)
            .and_then(|epoch| epoch.checked_add_signed(chrono::TimeDelta::days(i64::from(self.0))))
    }

    /// First millisecond of the day, as local wall time.
    #[must_use]
    pub fn start_local_ms(self) -> i64 {
        i64::from(self.0) * Self::MS_PER_DAY
    }

    /// Year and month (1-12) of this day.
    #[must_use]
    pub fn year_month(self) -> Option<(i32, u32)> {
        self.date().map(|date| (date.year(), date.month()))
    }

    #[must_use]
    pub fn year(self) -> Option<i32> {
        self.date().map(|date| date.year())
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn naive(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("valid test date")
    }

    #[test]
    fn floating_time_keeps_wall_clock_day() {
        let time = CaptureTime::floating(
            naive("2023-07-01 23:30:00"),
            DateSource::Exif,
            Confidence::Medium,
        );
        assert_eq!(time.utc_ms(), None);
        assert_eq!(
            time.day().and_then(Day::date),
            NaiveDate::from_ymd_opt(2023, 7, 1)
        );
        assert!(time.is_trustworthy());
    }

    #[test]
    fn zoned_time_reports_instant() {
        let time = CaptureTime::zoned(
            naive("2023-07-01 12:00:00"),
            120,
            DateSource::ExifWithOffset,
            Confidence::High,
        );
        assert_eq!(
            time.utc_ms(),
            Some(local_ms_from_naive(naive("2023-07-01 10:00:00")))
        );
    }

    #[test]
    fn instants_convert_to_local_wall_time() {
        let offset = FixedOffset::east_opt(-5 * 3600).expect("valid offset");
        let instant = offset
            .from_local_datetime(&naive("2024-01-02 03:04:05"))
            .single()
            .expect("unambiguous");
        let time = CaptureTime::from_instant(instant, DateSource::Container, Confidence::High);
        assert_eq!(time.offset_minutes, Some(-300));
        assert_eq!(time.local(), Some(naive("2024-01-02 03:04:05")));
    }

    #[test]
    fn low_confidence_is_not_trustworthy() {
        let time = CaptureTime::floating(
            naive("2026-10-11 10:00:00"),
            DateSource::FileModified,
            Confidence::Low,
        );
        assert!(!time.is_trustworthy());
        assert!(!CaptureTime::UNKNOWN.is_trustworthy());
    }

    #[test]
    fn days_handle_dates_before_the_epoch() {
        let day = Day::from_local_ms(local_ms_from_naive(naive("1969-12-31 23:59:59")));
        assert_eq!(day, Day(-1));
        assert_eq!(day.date(), NaiveDate::from_ymd_opt(1969, 12, 31));
        assert_eq!(
            Day::from_date(NaiveDate::from_ymd_opt(1970, 1, 2).expect("date")),
            Day(1)
        );
    }
}
