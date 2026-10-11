//! EXIF metadata extraction.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use chrono::NaiveDateTime;
use exif::{Exif, Field, In, Reader, Tag, Value};
use medianest_core::GeoPoint;

use crate::dates::{gps_datetime, offset_from_gps, parse_exif_datetime, parse_offset};

/// The EXIF facts MediaNest uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExifData {
    pub taken: Option<NaiveDateTime>,
    pub taken_offset_minutes: Option<i32>,
    /// `true` when `taken` came from DateTimeOriginal or DateTimeDigitized.
    pub taken_is_original: bool,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub aperture: Option<f32>,
    pub exposure_seconds: Option<f32>,
    pub iso: Option<u32>,
    pub focal_length_mm: Option<f32>,
    pub focal_length_35mm: Option<f32>,
    pub orientation: Option<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub location: Option<GeoPoint>,
    pub software: Option<String>,
}

/// Reads EXIF from any container kamadak-exif understands: JPEG, TIFF and
/// TIFF-based RAW, HEIF, PNG and WebP. Returns `None` when there is none.
#[must_use]
pub fn read(path: &Path) -> Option<ExifData> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif = Reader::new().read_from_container(&mut reader).ok()?;
    Some(extract(&exif))
}

/// Reads EXIF from an in-memory TIFF structure (for example a JPEG APP1 payload).
#[must_use]
pub fn read_raw(data: Vec<u8>) -> Option<ExifData> {
    let exif = Reader::new().read_raw(data).ok()?;
    Some(extract(&exif))
}

fn field(exif: &Exif, tag: Tag) -> Option<&Field> {
    exif.get_field(tag, In::PRIMARY)
}

fn ascii(exif: &Exif, tag: Tag) -> Option<String> {
    match &field(exif, tag)?.value {
        Value::Ascii(parts) => {
            let text = parts
                .iter()
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let trimmed = text
                .trim_matches(|ch: char| ch == '\0' || ch.is_whitespace())
                .to_owned();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        _ => None,
    }
}

fn rationals(exif: &Exif, tag: Tag) -> Option<Vec<f64>> {
    match &field(exif, tag)?.value {
        Value::Rational(values) => Some(
            values
                .iter()
                .filter(|value| value.denom != 0)
                .map(exif::Rational::to_f64)
                .collect(),
        ),
        Value::SRational(values) => Some(
            values
                .iter()
                .filter(|value| value.denom != 0)
                .map(exif::SRational::to_f64)
                .collect(),
        ),
        _ => None,
    }
}

fn first_rational(exif: &Exif, tag: Tag) -> Option<f64> {
    rationals(exif, tag)?
        .first()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn uint(exif: &Exif, tag: Tag) -> Option<u32> {
    field(exif, tag)?.value.get_uint(0)
}

#[allow(clippy::cast_possible_truncation)]
fn as_f32(value: f64) -> f32 {
    value as f32
}

fn coordinate(exif: &Exif, value: Tag, reference: Tag, negative: &str) -> Option<f64> {
    let parts = rationals(exif, value)?;
    let degrees = parts.first().copied()?
        + parts.get(1).copied().unwrap_or(0.0) / 60.0
        + parts.get(2).copied().unwrap_or(0.0) / 3600.0;
    let sign = if ascii(exif, reference).is_some_and(|text| text.eq_ignore_ascii_case(negative)) {
        -1.0
    } else {
        1.0
    };
    Some(sign * degrees)
}

fn date_with(
    exif: &Exif,
    date: Tag,
    offset: Tag,
    subsec: Tag,
) -> Option<(NaiveDateTime, Option<i32>)> {
    let text = ascii(exif, date)?;
    let subsec = ascii(exif, subsec);
    let parsed = parse_exif_datetime(&text, subsec.as_deref())?;
    Some((
        parsed,
        ascii(exif, offset).as_deref().and_then(parse_offset),
    ))
}

fn extract(exif: &Exif) -> ExifData {
    let original = date_with(
        exif,
        Tag::DateTimeOriginal,
        Tag::OffsetTimeOriginal,
        Tag::SubSecTimeOriginal,
    )
    .or_else(|| {
        date_with(
            exif,
            Tag::DateTimeDigitized,
            Tag::OffsetTimeDigitized,
            Tag::SubSecTimeDigitized,
        )
    });
    let taken_is_original = original.is_some();
    let taken =
        original.or_else(|| date_with(exif, Tag::DateTime, Tag::OffsetTime, Tag::SubSecTime));

    let latitude = coordinate(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, "S");
    let longitude = coordinate(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, "W");
    let altitude = first_rational(exif, Tag::GPSAltitude).map(|value| {
        let below_sea = field(exif, Tag::GPSAltitudeRef)
            .and_then(|f| f.value.get_uint(0))
            .is_some_and(|reference| reference == 1);
        if below_sea { -value } else { value }
    });
    let location = latitude
        .zip(longitude)
        .and_then(|(lat, lon)| GeoPoint::new(lat, lon, altitude));

    let mut offset = taken.and_then(|(_, offset)| offset);
    if offset.is_none() {
        if let (Some((local, _)), Some(date), Some(time)) = (
            taken,
            ascii(exif, Tag::GPSDateStamp),
            rationals(exif, Tag::GPSTimeStamp),
        ) {
            if time.len() == 3 {
                offset = gps_datetime(&date, time[0], time[1], time[2])
                    .and_then(|utc| offset_from_gps(local, utc));
            }
        }
    }

    let width = uint(exif, Tag::PixelXDimension).or_else(|| uint(exif, Tag::ImageWidth));
    let height = uint(exif, Tag::PixelYDimension).or_else(|| uint(exif, Tag::ImageLength));
    let iso = uint(exif, Tag::PhotographicSensitivity).filter(|value| *value > 0);
    let lens = ascii(exif, Tag::LensModel).filter(|lens| lens != "----");

    ExifData {
        taken: taken.map(|(value, _)| value),
        taken_offset_minutes: offset,
        taken_is_original,
        make: ascii(exif, Tag::Make),
        model: ascii(exif, Tag::Model),
        lens,
        aperture: first_rational(exif, Tag::FNumber).map(as_f32),
        exposure_seconds: first_rational(exif, Tag::ExposureTime).map(as_f32),
        iso,
        focal_length_mm: first_rational(exif, Tag::FocalLength).map(as_f32),
        focal_length_35mm: uint(exif, Tag::FocalLengthIn35mmFilm)
            .filter(|value| *value > 0)
            .map(|value| as_f32(f64::from(value))),
        orientation: uint(exif, Tag::Orientation)
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| (1..=8).contains(value)),
        width: width.filter(|value| *value > 0),
        height: height.filter(|value| *value > 0),
        location,
        software: ascii(exif, Tag::Software),
    }
}
