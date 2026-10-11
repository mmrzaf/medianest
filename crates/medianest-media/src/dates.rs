//! Capture dates from EXIF text, GPS timestamps and file names.

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike};

/// Parses an EXIF date such as `2023:07:01 10:15:00`, plus optional sub-seconds.
#[must_use]
pub fn parse_exif_datetime(value: &str, subseconds: Option<&str>) -> Option<NaiveDateTime> {
    let cleaned: String = value
        .trim_matches(|ch: char| ch == '"' || ch == '\0' || ch.is_whitespace())
        .chars()
        .take(19)
        .collect();
    if cleaned.len() < 19 {
        return None;
    }
    let normalized = cleaned
        .char_indices()
        .map(|(index, ch)| match (index, ch) {
            (4 | 7, ':' | '-' | '/') => '-',
            (10, 'T' | ' ') => ' ',
            (13 | 16, ':' | '.') => ':',
            (_, other) => other,
        })
        .collect::<String>();
    let parsed = NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M:%S").ok()?;
    if parsed.year() < 1826 {
        return None;
    }
    let millis = subseconds
        .map(|text| text.trim_matches(|ch: char| ch == '"' || ch == '\0' || ch.is_whitespace()))
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|text| {
            let mut digits: String = text.chars().take(3).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            digits.parse::<i64>().unwrap_or(0)
        })
        .unwrap_or(0);
    parsed.checked_add_signed(TimeDelta::milliseconds(millis))
}

/// Parses an EXIF offset such as `+02:00`, `-0530` or `Z` into minutes east of UTC.
#[must_use]
pub fn parse_offset(value: &str) -> Option<i32> {
    let cleaned = value.trim_matches(|ch: char| ch == '"' || ch == '\0' || ch.is_whitespace());
    if cleaned.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match cleaned.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = cleaned[1..].chars().filter(char::is_ascii_digit).collect();
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i32>().ok()?, 0),
        4 => (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    if hours > 14 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

/// Infers the timezone offset of a camera's wall clock from the GPS UTC
/// timestamp recorded alongside it. Offsets are rounded to 15 minutes and
/// only accepted when the clocks agree within five minutes.
#[must_use]
pub fn offset_from_gps(local: NaiveDateTime, gps_utc: NaiveDateTime) -> Option<i32> {
    let difference = (local - gps_utc).num_seconds();
    let quarter_hours = (difference as f64 / 900.0).round();
    #[allow(clippy::cast_possible_truncation)]
    let offset_minutes = (quarter_hours * 15.0) as i64;
    let residual = difference - offset_minutes * 60;
    (offset_minutes.abs() <= 14 * 60 && residual.abs() <= 300)
        .then(|| i32::try_from(offset_minutes).ok())
        .flatten()
}

fn digits_at(bytes: &[u8], pos: usize, count: usize) -> Option<u32> {
    let slice = bytes.get(pos..pos + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(slice).ok()?.parse().ok()
}

fn skip_separator(bytes: &[u8], pos: usize, allowed: &[u8]) -> usize {
    if bytes.get(pos).is_some_and(|byte| allowed.contains(byte)) {
        pos + 1
    } else {
        pos
    }
}

/// Finds a capture date written into a file name, as phones and many apps do:
/// `IMG_20230701_101500.jpg`, `PXL_20230701_101500123.jpg`,
/// `Screenshot 2023-07-01 at 10.15.00.png`, `2023-07-01 10.15.00.jpg`,
/// `signal-2023-07-01-101500.jpg`, or a Unix timestamp in milliseconds.
#[must_use]
pub fn date_from_file_name(name: &str) -> Option<NaiveDateTime> {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let bytes = stem.as_bytes();
    let latest_year = Local::now().year() + 1;

    if (bytes.len() == 13 || bytes.len() == 10) && bytes.iter().all(u8::is_ascii_digit) {
        let value: i64 = stem.parse().ok()?;
        let millis = if bytes.len() == 13 {
            value
        } else {
            value * 1000
        };
        let instant = DateTime::from_timestamp_millis(millis)?;
        let local = instant.with_timezone(&Local).naive_local();
        return (2000..=latest_year)
            .contains(&local.year())
            .then_some(local);
    }

    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() || (start > 0 && bytes[start - 1].is_ascii_digit()) {
            continue;
        }
        let Some(year) = digits_at(bytes, start, 4) else {
            continue;
        };
        if !(1990..=latest_year).contains(&i32::try_from(year).unwrap_or(0)) {
            continue;
        }
        let mut pos = skip_separator(bytes, start + 4, b"-_.: ");
        let Some(month) = digits_at(bytes, pos, 2) else {
            continue;
        };
        pos = skip_separator(bytes, pos + 2, b"-_.: ");
        let Some(day) = digits_at(bytes, pos, 2) else {
            continue;
        };
        pos += 2;
        let Some(date) = NaiveDate::from_ymd_opt(i32::try_from(year).unwrap_or(0), month, day)
        else {
            continue;
        };
        let compact = pos - start == 8;
        if bytes.get(pos).is_some_and(u8::is_ascii_digit) && !compact {
            continue;
        }
        // Optional time after a separator such as "_", "-", " ", "T" or " at ".
        let time = {
            let rest = &stem[pos..];
            let lower = rest.to_ascii_lowercase();
            let skip = ["_at_", " at ", "_", "-", " ", "t", "."]
                .iter()
                .find(|prefix| lower.starts_with(*prefix))
                .map_or(0, |prefix| prefix.len());
            let mut cursor = pos + skip;
            let hour = digits_at(bytes, cursor, 2);
            cursor = skip_separator(bytes, cursor + 2, b":.-_");
            let minute = digits_at(bytes, cursor, 2);
            cursor = skip_separator(bytes, cursor + 2, b":.-_");
            let second = digits_at(bytes, cursor, 2);
            match (hour, minute, second) {
                (Some(h), Some(m), Some(s)) => NaiveTime::from_hms_opt(h, m, s),
                (Some(h), Some(m), None) => NaiveTime::from_hms_opt(h, m, 0),
                _ => None,
            }
        };
        if compact && time.is_none() && bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let datetime = date.and_time(time.unwrap_or(NaiveTime::MIN));
        if datetime.date() > Local::now().date_naive() + TimeDelta::days(1) {
            continue;
        }
        return Some(datetime);
    }
    None
}

/// Converts a GPS date and time-of-day (hours, minutes, seconds) into a timestamp.
#[must_use]
pub fn gps_datetime(date: &str, hours: f64, minutes: f64, seconds: f64) -> Option<NaiveDateTime> {
    let date = date.trim_matches(|ch: char| ch == '"' || ch == '\0' || ch.is_whitespace());
    let date = NaiveDate::parse_from_str(&date.replace(':', "-"), "%Y-%m-%d").ok()?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = (hours * 3600.0 + minutes * 60.0 + seconds).round() as u32;
    let time = NaiveTime::from_num_seconds_from_midnight_opt(total.min(86_399), 0)?;
    Some(
        date.and_time(time)
            .with_nanosecond(0)
            .unwrap_or_else(|| date.and_time(time)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("date")
    }

    #[test]
    fn exif_datetimes() {
        assert_eq!(
            parse_exif_datetime("2023:07:01 10:15:00", None),
            Some(dt("2023-07-01 10:15:00"))
        );
        assert_eq!(
            parse_exif_datetime("\"2023:07:01 10:15:00\"", Some("250"))
                .map(|d| d.and_utc().timestamp_subsec_millis()),
            Some(250)
        );
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00", None), None);
        assert_eq!(parse_exif_datetime("    :  :     :  :  ", None), None);
        assert_eq!(
            parse_exif_datetime("2023-07-01T10:15:00", None),
            Some(dt("2023-07-01 10:15:00"))
        );
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+02:00"), Some(120));
        assert_eq!(parse_offset("-0530"), Some(-330));
        assert_eq!(parse_offset("Z"), Some(0));
        assert_eq!(parse_offset("+25:00"), None);
        assert_eq!(parse_offset("garbage"), None);
    }

    #[test]
    fn gps_offsets_round_to_quarter_hours() {
        assert_eq!(
            offset_from_gps(dt("2023-07-01 14:15:20"), dt("2023-07-01 12:15:00")),
            Some(120)
        );
        assert_eq!(
            offset_from_gps(dt("2023-07-01 10:00:00"), dt("2023-07-01 15:31:00")),
            Some(-330)
        );
        assert_eq!(
            offset_from_gps(dt("2023-07-01 10:00:00"), dt("2023-07-01 10:07:30")),
            None
        );
        assert_eq!(
            gps_datetime("2023:07:01", 12.0, 15.0, 0.4),
            Some(dt("2023-07-01 12:15:00"))
        );
    }

    #[test]
    fn file_name_dates() {
        let cases = [
            ("IMG_20230701_101500.jpg", Some("2023-07-01 10:15:00")),
            ("PXL_20230701_101500123.MP.jpg", Some("2023-07-01 10:15:00")),
            ("VID_20211224_180000.mp4", Some("2021-12-24 18:00:00")),
            (
                "Screenshot 2023-07-01 at 10.15.00.png",
                Some("2023-07-01 10:15:00"),
            ),
            (
                "Screenshot_20230701-101500_Chrome.jpg",
                Some("2023-07-01 10:15:00"),
            ),
            (
                "WhatsApp Image 2022-03-04 at 21.07.33.jpeg",
                Some("2022-03-04 21:07:33"),
            ),
            ("2019-08-10 12.30.45.jpg", Some("2019-08-10 12:30:45")),
            ("signal-2023-07-01-101500.jpg", Some("2023-07-01 10:15:00")),
            ("20190810.jpg", Some("2019-08-10 00:00:00")),
            ("DSC_1234.JPG", None),
            ("IMG_2100.JPG", None),
            ("holiday 1999.jpg", None),
            ("photo_20231345_101500.jpg", None),
        ];
        for (name, expected) in cases {
            assert_eq!(date_from_file_name(name), expected.map(dt), "{name}");
        }
    }

    #[test]
    fn unix_timestamp_names() {
        let parsed = date_from_file_name("1690000000000.jpg").expect("timestamp");
        assert_eq!(parsed.year(), 2023);
        assert!(date_from_file_name("0000000000.jpg").is_none());
    }
}
