//! The search language used by the search bar and smart albums.
//!
//! A query is a list of terms that must all match. Terms are plain words or
//! `key:value` filters, optionally negated with a leading `-`:
//!
//! ```text
//! beach tag:family -tag:work is:fav rating>=4 date:2023-07 place:"New York"
//! ```
//!
//! Bare years (`2023`) and month names (`july`, `july 2023`) are understood as
//! dates. Unknown keys are treated as plain text, so a query never fails to
//! parse.

use std::fmt;

use chrono::{Datelike, Months, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::{Format, MediaKind};

/// A parsed query. All terms must match.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub terms: Vec<Term>,
}

/// One condition, possibly negated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    pub negated: bool,
    pub filter: Filter,
}

/// A single condition on an asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    /// Words matched against names, tags, albums, cameras, places and captions.
    Text(String),
    Tag(String),
    Album(String),
    /// A folder path fragment, matched against file locations.
    Folder(String),
    Camera(String),
    Place(String),
    /// Local capture date within `[start, end)`.
    Date(DateRange),
    /// Captured in this month of any year (1-12).
    Month(u32),
    DateUnknown,
    /// The date was guessed and should be confirmed.
    DateUncertain,
    Kind(MediaKind),
    Format(Format),
    Favorite,
    Rating(Comparison, u8),
    HasLocation,
    HasTags,
    Live,
    Raw,
    Screenshot,
    Hidden,
    /// At least one file is missing or changed.
    Problem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Equal,
    AtLeast,
    AtMost,
    Greater,
    Less,
}

impl Comparison {
    #[must_use]
    pub const fn sql(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::AtLeast => ">=",
            Self::AtMost => "<=",
            Self::Greater => ">",
            Self::Less => "<",
        }
    }
}

/// A half-open range of calendar days.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl DateRange {
    #[must_use]
    pub fn year(year: i32) -> Option<Self> {
        Some(Self {
            start: NaiveDate::from_ymd_opt(year, 1, 1)?,
            end: NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?,
        })
    }

    #[must_use]
    pub fn month(year: i32, month: u32) -> Option<Self> {
        let start = NaiveDate::from_ymd_opt(year, month, 1)?;
        Some(Self {
            start,
            end: start.checked_add_months(Months::new(1))?,
        })
    }

    #[must_use]
    pub fn day(date: NaiveDate) -> Option<Self> {
        Some(Self {
            start: date,
            end: date.succ_opt()?,
        })
    }

    /// Canonical textual form, the inverse of [`parse_date_range`].
    #[must_use]
    pub fn to_query_value(&self) -> String {
        let one_day = self.start.succ_opt() == Some(self.end);
        let one_month = self.start.day() == 1
            && self.start.checked_add_months(Months::new(1)) == Some(self.end);
        let one_year = self.start.ordinal() == 1
            && self.end.ordinal() == 1
            && self.end.year() == self.start.year() + 1;
        if one_day {
            self.start.format("%Y-%m-%d").to_string()
        } else if one_month {
            self.start.format("%Y-%m").to_string()
        } else if one_year {
            self.start.year().to_string()
        } else {
            let last = self.end.pred_opt().unwrap_or(self.end);
            format!(
                "{}..{}",
                self.start.format("%Y-%m-%d"),
                last.format("%Y-%m-%d")
            )
        }
    }
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Month number for an English month name or three-letter abbreviation.
#[must_use]
pub fn month_from_name(word: &str) -> Option<u32> {
    let lower = word.to_lowercase();
    if lower.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|name| {
            *name == lower
                || (lower.len() == 3 && name.starts_with(&lower))
                || (lower == "sept" && *name == "september")
        })
        .and_then(|index| u32::try_from(index + 1).ok())
}

#[must_use]
pub fn month_name(month: u32) -> &'static str {
    const TITLES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    month
        .checked_sub(1)
        .and_then(|index| TITLES.get(index as usize))
        .copied()
        .unwrap_or("")
}

fn plausible_year(value: &str) -> Option<i32> {
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let year: i32 = value.parse().ok()?;
    (1826..=2200).contains(&year).then_some(year)
}

/// Parses a single date expression: `2023`, `2023-07`, `2023-07-14`,
/// `today`, `yesterday`, or a range `A..B` of those (inclusive of `B`).
#[must_use]
pub fn parse_date_range(value: &str, today: NaiveDate) -> Option<DateRange> {
    let value = value.trim().to_lowercase();
    if let Some((from, to)) = value.split_once("..") {
        let start = parse_date_range(from, today)?.start;
        let end = parse_date_range(to, today)?.end;
        return (start < end).then_some(DateRange { start, end });
    }
    match value.as_str() {
        "today" => return DateRange::day(today),
        "yesterday" => return DateRange::day(today.pred_opt()?),
        "this-year" | "thisyear" => return DateRange::year(today.year()),
        "last-year" | "lastyear" => return DateRange::year(today.year() - 1),
        _ => {}
    }
    let parts: Vec<&str> = value.split(['-', '/', '.']).collect();
    match parts.as_slice() {
        [year] => DateRange::year(plausible_year(year)?),
        [year, month] => {
            let month = month.parse().ok().or_else(|| month_from_name(month))?;
            DateRange::month(plausible_year(year)?, month)
        }
        [year, month, day] => {
            let date = NaiveDate::from_ymd_opt(
                plausible_year(year)?,
                month.parse().ok()?,
                day.parse().ok()?,
            )?;
            DateRange::day(date)
        }
        _ => None,
    }
}

impl Query {
    /// Parses a query using today's local date for relative expressions.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        Self::parse_with_today(input, chrono::Local::now().date_naive())
    }

    #[must_use]
    pub fn parse_with_today(input: &str, today: NaiveDate) -> Self {
        let tokens = tokenize(input);
        let mut terms = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            // "july 2023" and "2023 july" read as one month.
            if !token.negated && token.key.is_none() && !token.quoted {
                if let Some(next) = tokens
                    .get(index + 1)
                    .filter(|next| !next.negated && next.key.is_none() && !next.quoted)
                {
                    let pair = month_from_name(&token.value)
                        .zip(plausible_year(&next.value))
                        .or_else(|| {
                            plausible_year(&token.value)
                                .zip(month_from_name(&next.value))
                                .map(|(y, m)| (m, y))
                        });
                    if let Some(range) =
                        pair.and_then(|(month, year)| DateRange::month(year, month))
                    {
                        terms.push(Term {
                            negated: false,
                            filter: Filter::Date(range),
                        });
                        index += 2;
                        continue;
                    }
                }
            }
            if let Some(term) = token_to_term(token, today) {
                terms.push(term);
            }
            index += 1;
        }
        Self { terms }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Adds a term unless an identical one is already present.
    pub fn push(&mut self, term: Term) {
        if !self.terms.contains(&term) {
            self.terms.push(term);
        }
    }

    /// Removes every term equal to `term`.
    pub fn remove(&mut self, term: &Term) {
        self.terms.retain(|existing| existing != term);
    }

    #[must_use]
    pub fn contains_filter(&self, filter: &Filter) -> bool {
        self.terms
            .iter()
            .any(|term| !term.negated && &term.filter == filter)
    }

    /// Whether hidden assets are explicitly requested.
    #[must_use]
    pub fn wants_hidden(&self) -> bool {
        self.contains_filter(&Filter::Hidden)
    }

    /// Free-text words, for highlighting and full-text search.
    #[must_use]
    pub fn text_terms(&self) -> Vec<&str> {
        self.terms
            .iter()
            .filter_map(|term| match &term.filter {
                Filter::Text(text) if !term.negated => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, term) in self.terms.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{term}")?;
        }
        Ok(())
    }
}

fn quote_if_needed(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '"' || ch == ':')
    {
        format!("\"{}\"", value.replace('"', "'"))
    } else {
        value.to_owned()
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            f.write_str("-")?;
        }
        match &self.filter {
            Filter::Text(text) => {
                let looks_special = plausible_year(text).is_some()
                    || month_from_name(text).is_some()
                    || text.starts_with('-')
                    || text.contains(':');
                if looks_special {
                    write!(f, "\"{}\"", text.replace('"', "'"))
                } else {
                    f.write_str(&quote_if_needed(text))
                }
            }
            Filter::Tag(value) => write!(f, "tag:{}", quote_if_needed(value)),
            Filter::Album(value) => write!(f, "album:{}", quote_if_needed(value)),
            Filter::Folder(value) => write!(f, "in:{}", quote_if_needed(value)),
            Filter::Camera(value) => write!(f, "camera:{}", quote_if_needed(value)),
            Filter::Place(value) => write!(f, "place:{}", quote_if_needed(value)),
            Filter::Date(range) => write!(f, "date:{}", range.to_query_value()),
            Filter::Month(month) => {
                write!(f, "month:{}", MONTHS[(*month as usize).clamp(1, 12) - 1])
            }
            Filter::DateUnknown => f.write_str("date:unknown"),
            Filter::DateUncertain => f.write_str("date:uncertain"),
            Filter::Kind(MediaKind::Photo) => f.write_str("is:photo"),
            Filter::Kind(MediaKind::Video) => f.write_str("is:video"),
            Filter::Format(format) => write!(f, "format:{}", format.code()),
            Filter::Favorite => f.write_str("is:favorite"),
            Filter::Rating(comparison, value) => {
                let op = match comparison {
                    Comparison::Equal => ":",
                    Comparison::AtLeast => ">=",
                    Comparison::AtMost => "<=",
                    Comparison::Greater => ">",
                    Comparison::Less => "<",
                };
                write!(f, "rating{op}{value}")
            }
            Filter::HasLocation => f.write_str("has:location"),
            Filter::HasTags => f.write_str("has:tags"),
            Filter::Live => f.write_str("is:live"),
            Filter::Raw => f.write_str("is:raw"),
            Filter::Screenshot => f.write_str("is:screenshot"),
            Filter::Hidden => f.write_str("is:hidden"),
            Filter::Problem => f.write_str("is:problem"),
        }
    }
}

impl Term {
    /// Short label for a filter chip, such as `Tag: family`.
    #[must_use]
    pub fn label(&self) -> String {
        let base = match &self.filter {
            Filter::Text(text) => format!("“{text}”"),
            Filter::Tag(value) => format!("Tag: {value}"),
            Filter::Album(value) => format!("Album: {value}"),
            Filter::Folder(value) => format!("Folder: {value}"),
            Filter::Camera(value) => format!("Camera: {value}"),
            Filter::Place(value) => format!("Place: {value}"),
            Filter::Date(range) => describe_range(range),
            Filter::Month(month) => format!("Every {}", month_name(*month)),
            Filter::DateUnknown => "No date".to_owned(),
            Filter::DateUncertain => "Uncertain date".to_owned(),
            Filter::Kind(MediaKind::Photo) => "Photos".to_owned(),
            Filter::Kind(MediaKind::Video) => "Videos".to_owned(),
            Filter::Format(format) => format.label().to_owned(),
            Filter::Favorite => "Favorites".to_owned(),
            Filter::Rating(comparison, value) => match comparison {
                Comparison::AtLeast => format!("{value}+ stars"),
                Comparison::Equal => format!("{value} stars"),
                Comparison::AtMost => format!("Up to {value} stars"),
                Comparison::Greater => format!("More than {value} stars"),
                Comparison::Less => format!("Fewer than {value} stars"),
            },
            Filter::HasLocation => "Has location".to_owned(),
            Filter::HasTags => "Tagged".to_owned(),
            Filter::Live => "Live photos".to_owned(),
            Filter::Raw => "RAW".to_owned(),
            Filter::Screenshot => "Screenshots".to_owned(),
            Filter::Hidden => "Hidden".to_owned(),
            Filter::Problem => "Has problems".to_owned(),
        };
        if self.negated {
            format!("Not {base}")
        } else {
            base
        }
    }
}

fn describe_range(range: &DateRange) -> String {
    let value = range.to_query_value();
    if value.contains("..") {
        let last = range.end.pred_opt().unwrap_or(range.end);
        format!(
            "{} – {}",
            range.start.format("%-d %b %Y"),
            last.format("%-d %b %Y")
        )
    } else if value.len() == 4 {
        value
    } else if value.len() == 7 {
        format!("{} {}", month_name(range.start.month()), range.start.year())
    } else {
        range.start.format("%-d %B %Y").to_string()
    }
}

#[derive(Debug)]
struct Token {
    negated: bool,
    key: Option<String>,
    operator: Option<String>,
    value: String,
    quoted: bool,
}

fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let mut negated = false;
        if chars[index] == '-' && chars.get(index + 1).is_some_and(|ch| !ch.is_whitespace()) {
            negated = true;
            index += 1;
        }
        let mut raw = String::new();
        let mut quoted = false;
        let mut key = None;
        let mut operator = None;
        while index < chars.len() && !chars[index].is_whitespace() {
            let ch = chars[index];
            if ch == '"' {
                quoted = true;
                index += 1;
                while index < chars.len() && chars[index] != '"' {
                    raw.push(chars[index]);
                    index += 1;
                }
                index += 1;
                continue;
            }
            if key.is_none()
                && !quoted
                && (ch == ':' || ch == '>' || ch == '<' || ch == '=')
                && !raw.is_empty()
            {
                let mut op = String::from(ch);
                if ch != ':' && chars.get(index + 1) == Some(&'=') {
                    op.push('=');
                    index += 1;
                }
                key = Some(raw.to_lowercase());
                operator = Some(op);
                raw = String::new();
                index += 1;
                continue;
            }
            raw.push(ch);
            index += 1;
        }
        if raw.is_empty() && key.is_none() {
            continue;
        }
        tokens.push(Token {
            negated,
            key,
            operator,
            value: raw,
            quoted,
        });
    }
    tokens
}

fn token_to_term(token: &Token, today: NaiveDate) -> Option<Term> {
    let value = token.value.trim();
    let make = |filter| {
        Some(Term {
            negated: token.negated,
            filter,
        })
    };
    let Some(key) = token.key.as_deref() else {
        if value.is_empty() {
            return None;
        }
        if !token.quoted {
            if let Some(year) = plausible_year(value) {
                return make(Filter::Date(DateRange::year(year)?));
            }
            if let Some(month) = month_from_name(value) {
                return make(Filter::Month(month));
            }
        }
        return make(Filter::Text(value.to_owned()));
    };
    let operator = token.operator.as_deref().unwrap_or(":");
    let lower = value.to_lowercase();
    let as_text = || {
        let text = format!("{key}{operator}{value}");
        Some(Term {
            negated: token.negated,
            filter: Filter::Text(text),
        })
    };
    if key == "rating" || key == "stars" {
        let comparison = match operator {
            ":" | "=" => Comparison::Equal,
            ">=" => Comparison::AtLeast,
            "<=" => Comparison::AtMost,
            ">" => Comparison::Greater,
            "<" => Comparison::Less,
            _ => return as_text(),
        };
        let stars: u8 = lower
            .trim_end_matches('+')
            .parse()
            .ok()
            .filter(|v| *v <= 5)?;
        let comparison = if lower.ends_with('+') {
            Comparison::AtLeast
        } else {
            comparison
        };
        return make(Filter::Rating(comparison, stars));
    }
    if operator != ":" {
        return as_text();
    }
    if value.is_empty() {
        return None;
    }
    let filter = match key {
        "tag" | "tags" | "#" => Filter::Tag(value.to_owned()),
        "album" => Filter::Album(value.to_owned()),
        "in" | "folder" | "path" => Filter::Folder(value.to_owned()),
        "camera" | "device" => Filter::Camera(value.to_owned()),
        "place" | "location" | "near" => Filter::Place(value.to_owned()),
        "month" => Filter::Month(
            month_from_name(&lower)
                .or_else(|| lower.parse().ok().filter(|m| (1..=12).contains(m)))?,
        ),
        "date" | "year" | "taken" => match lower.as_str() {
            "unknown" | "none" | "missing" => Filter::DateUnknown,
            "uncertain" | "guessed" => Filter::DateUncertain,
            _ => Filter::Date(parse_date_range(&lower, today)?),
        },
        "format" | "ext" => Filter::Format(
            Format::from_code(&lower)
                .ok()
                .or_else(|| Format::from_extension(&lower))?,
        ),
        "is" | "type" | "kind" => match lower.as_str() {
            "fav" | "favs" | "favorite" | "favorites" | "favourite" | "favourites" | "starred" => {
                Filter::Favorite
            }
            "video" | "videos" | "movie" | "movies" => Filter::Kind(MediaKind::Video),
            "photo" | "photos" | "image" | "images" | "picture" | "pictures" => {
                Filter::Kind(MediaKind::Photo)
            }
            "live" | "motion" => Filter::Live,
            "raw" => Filter::Raw,
            "screenshot" | "screenshots" => Filter::Screenshot,
            "hidden" => Filter::Hidden,
            "problem" | "problems" | "missing" | "broken" => Filter::Problem,
            _ => return as_text(),
        },
        "has" => match lower.as_str() {
            "location" | "gps" | "place" | "geo" => Filter::HasLocation,
            "tag" | "tags" => Filter::HasTags,
            _ => return as_text(),
        },
        _ => return as_text(),
    };
    make(filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 11).expect("date")
    }

    fn parse(text: &str) -> Query {
        Query::parse_with_today(text, today())
    }

    fn filters(text: &str) -> Vec<(bool, Filter)> {
        parse(text)
            .terms
            .into_iter()
            .map(|term| (term.negated, term.filter))
            .collect()
    }

    #[test]
    fn parses_mixed_terms() {
        let parsed = filters(r#"beach tag:family -tag:work is:fav rating>=4 place:"New York""#);
        assert_eq!(
            parsed,
            vec![
                (false, Filter::Text("beach".into())),
                (false, Filter::Tag("family".into())),
                (true, Filter::Tag("work".into())),
                (false, Filter::Favorite),
                (false, Filter::Rating(Comparison::AtLeast, 4)),
                (false, Filter::Place("New York".into())),
            ]
        );
    }

    #[test]
    fn understands_dates() {
        assert_eq!(
            filters("2023"),
            vec![(false, Filter::Date(DateRange::year(2023).expect("year")))]
        );
        assert_eq!(
            filters("july 2021"),
            vec![(
                false,
                Filter::Date(DateRange::month(2021, 7).expect("month"))
            )]
        );
        assert_eq!(filters("July"), vec![(false, Filter::Month(7))]);
        assert_eq!(
            filters("date:2023-07-14"),
            vec![(
                false,
                Filter::Date(
                    DateRange::day(NaiveDate::from_ymd_opt(2023, 7, 14).expect("d")).expect("day")
                )
            )]
        );
        let range = parse_date_range("2021..2023-02", today()).expect("range");
        assert_eq!(range.start, NaiveDate::from_ymd_opt(2021, 1, 1).expect("d"));
        assert_eq!(range.end, NaiveDate::from_ymd_opt(2023, 3, 1).expect("d"));
        assert_eq!(filters("date:unknown"), vec![(false, Filter::DateUnknown)]);
        assert_eq!(
            filters("date:yesterday"),
            vec![(
                false,
                Filter::Date(
                    DateRange::day(NaiveDate::from_ymd_opt(2026, 10, 10).expect("d")).expect("day")
                )
            )]
        );
    }

    #[test]
    fn unknown_keys_and_bad_values_fall_back_to_text() {
        assert_eq!(
            filters("foo:bar"),
            vec![(false, Filter::Text("foo:bar".into()))]
        );
        assert_eq!(
            filters("is:banana"),
            vec![(false, Filter::Text("is:banana".into()))]
        );
        assert!(filters("rating:9").is_empty());
        assert_eq!(
            filters("\"2023\""),
            vec![(false, Filter::Text("2023".into()))]
        );
    }

    #[test]
    fn display_round_trips() {
        let samples = [
            r#"beach tag:family -tag:work is:favorite rating>=4 place:"New York""#,
            "date:2023 date:2021-07 date:2020-01-02 date:2019-01-01..2019-03-04",
            "is:video -is:live has:location -has:tags in:Pictures/Trips camera:Pixel",
            "month:july date:unknown date:uncertain is:raw format:heic is:hidden is:problem",
            "\"2023\" \"tag:literal\" album:\"Summer 2024\"",
        ];
        for sample in samples {
            let first = parse(sample);
            let second = parse(&first.to_string());
            assert_eq!(
                first,
                second,
                "round trip of {sample:?} via {:?}",
                first.to_string()
            );
        }
    }

    #[test]
    fn labels_are_human_readable() {
        let query = parse("-tag:work date:2021-07 rating:5+ is:video");
        let labels: Vec<String> = query.terms.iter().map(Term::label).collect();
        assert_eq!(
            labels,
            vec!["Not Tag: work", "July 2021", "5+ stars", "Videos"]
        );
    }

    #[test]
    fn empty_and_whitespace_queries_are_empty() {
        assert!(parse("   ").is_empty());
        assert!(parse("-").terms.len() <= 1);
    }
}
