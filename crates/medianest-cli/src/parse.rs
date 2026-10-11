//! Parsing command-line values.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

/// Parses `2023-07-01`, `2023-07-01 10:15` or `2023-07-01T10:15:30`.
pub fn datetime(text: &str) -> Result<NaiveDateTime, String> {
    let text = text.trim().replace('T', " ");
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(&text, format) {
            return Ok(value);
        }
    }
    NaiveDate::parse_from_str(&text, "%Y-%m-%d")
        .map(|date| date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap_or(NaiveTime::MIN)))
        .map_err(|_| format!("“{text}” is not a date; use YYYY-MM-DD or YYYY-MM-DD HH:MM"))
}

/// Parses a signed duration such as `+1h`, `-30m`, `+2d`, `-1h30m` or `90s`
/// into milliseconds.
pub fn duration_ms(text: &str) -> Result<i64, String> {
    let trimmed = text.trim();
    let (sign, rest) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1, &trimmed[1..]),
        Some(b'+') => (1, &trimmed[1..]),
        _ => (1, trimmed),
    };
    if rest.is_empty() {
        return Err("empty duration".into());
    }
    let mut total: i64 = 0;
    let mut number = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        let unit = match ch {
            'd' => 86_400_000,
            'h' => 3_600_000,
            'm' => 60_000,
            's' => 1_000,
            _ => return Err(format!("unknown unit “{ch}” in “{text}”; use d, h, m or s")),
        };
        let value: i64 = number
            .parse()
            .map_err(|_| format!("missing number before “{ch}”"))?;
        total += value * unit;
        number.clear();
    }
    if !number.is_empty() {
        return Err(format!("“{text}” needs a unit: d, h, m or s"));
    }
    Ok(sign * total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration_ms("+1h"), Ok(3_600_000));
        assert_eq!(duration_ms("-1h30m"), Ok(-5_400_000));
        assert_eq!(duration_ms("2d"), Ok(172_800_000));
        assert!(duration_ms("5").is_err());
        assert!(duration_ms("1x").is_err());
    }

    #[test]
    fn dates() {
        assert_eq!(
            datetime("2023-07-01 10:15").map(|d| d.to_string()),
            Ok("2023-07-01 10:15:00".into())
        );
        assert_eq!(
            datetime("2023-07-01").map(|d| d.to_string()),
            Ok("2023-07-01 12:00:00".into())
        );
        assert!(datetime("yesterday").is_err());
    }
}
