//! Conversions between catalog rows and domain types.

use medianest_core::{
    AlbumSort, AssetId, AssetUuid, CaptureTime, Confidence, DateSource, FileId, FileStamp,
    FileStatus, Format, GeoPoint, MediaKind, OperationKind, OperationState, OperationUuid,
    PathBytes, Place, ReviewKind, ReviewState, SourceId, SourceKind, SourceStatus, ThumbState,
    UnknownValue,
};
use rusqlite::Row;
use rusqlite::types::Type;

use crate::records::{Asset, FileRecord, Operation};

pub(crate) fn bad<E: std::error::Error + Send + Sync + 'static>(
    index: usize,
    error: E,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
}

pub(crate) fn code<T>(index: usize, result: Result<T, UnknownValue>) -> rusqlite::Result<T> {
    result.map_err(|error| bad(index, error))
}

pub(crate) fn hash32(value: Option<Vec<u8>>) -> Option<[u8; 32]> {
    value.and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
}

pub(crate) fn u64_of(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(crate) fn i64_of(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn uuid_of<T>(
    index: usize,
    bytes: &[u8],
    make: fn(&[u8]) -> Option<T>,
) -> rusqlite::Result<T> {
    make(bytes).ok_or_else(|| {
        bad(
            index,
            UnknownValue {
                kind: "uuid",
                value: format!("{} bytes", bytes.len()),
            },
        )
    })
}

pub(crate) fn capture_time(
    local: Option<i64>,
    offset: Option<i64>,
    source: i64,
    confidence: i64,
) -> CaptureTime {
    CaptureTime {
        local_ms: local,
        offset_minutes: offset.and_then(|value| i32::try_from(value).ok()),
        source: DateSource::from_code(source).unwrap_or(DateSource::Unknown),
        confidence: Confidence::from_code(confidence).unwrap_or(Confidence::Unknown),
    }
}

/// Column list matching [`asset_from_row`].
pub(crate) const ASSET_COLUMNS: &str = "a.id, a.uuid, a.kind, a.format, a.name, a.hash, a.size, \
    a.width, a.height, a.duration_ms, a.orientation, a.rotation, \
    a.taken_local, a.taken_offset, a.taken_source, a.taken_confidence, \
    a.probed_local, a.probed_offset, a.probed_source, a.probed_confidence, \
    a.camera_make, a.camera_model, a.lens, a.focal_mm, a.focal35_mm, a.aperture, a.exposure_s, a.iso, \
    a.latitude, a.longitude, a.altitude, a.place_city, a.place_region, a.place_country, a.place_cc, \
    a.favorite, a.rating, a.hidden, a.caption, a.trashed_ms, a.screenshot, a.content_id, a.motion_offset, \
    a.live_video_id, a.live_component, a.color, a.phash, a.thumb_state, a.file_state, a.added_ms, a.updated_ms";

fn opt_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

fn opt_f32(value: Option<f64>) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation)]
    value.map(|value| value as f32)
}

pub(crate) fn asset_from_row(row: &Row<'_>) -> rusqlite::Result<Asset> {
    let uuid: Vec<u8> = row.get(1)?;
    let latitude: Option<f64> = row.get(28)?;
    let longitude: Option<f64> = row.get(29)?;
    let altitude: Option<f64> = row.get(30)?;
    let city: Option<String> = row.get(31)?;
    let region: Option<String> = row.get(32)?;
    let country: Option<String> = row.get(33)?;
    let country_code: Option<String> = row.get(34)?;
    let format: String = row.get(3)?;
    Ok(Asset {
        id: AssetId(row.get(0)?),
        uuid: uuid_of(1, &uuid, AssetUuid::from_slice)?,
        kind: code(2, MediaKind::from_code(row.get(2)?))?,
        format: code(3, Format::from_code(&format))?,
        name: row.get(4)?,
        hash: hash32(row.get(5)?),
        size: u64_of(row.get(6)?),
        width: opt_u32(row.get(7)?),
        height: opt_u32(row.get(8)?),
        duration_ms: row.get::<_, Option<i64>>(9)?.map(u64_of),
        orientation: u8::try_from(row.get::<_, i64>(10)?).unwrap_or(1),
        rotation: u16::try_from(row.get::<_, i64>(11)?).unwrap_or(0),
        taken: capture_time(row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?),
        probed_taken: capture_time(row.get(16)?, row.get(17)?, row.get(18)?, row.get(19)?),
        camera_make: row.get(20)?,
        camera_model: row.get(21)?,
        lens: row.get(22)?,
        focal_length_mm: opt_f32(row.get(23)?),
        focal_length_35mm: opt_f32(row.get(24)?),
        aperture: opt_f32(row.get(25)?),
        exposure_seconds: opt_f32(row.get(26)?),
        iso: opt_u32(row.get(27)?),
        location: latitude
            .zip(longitude)
            .and_then(|(lat, lon)| GeoPoint::new(lat, lon, altitude)),
        place: country.map(|country| Place {
            city: city.unwrap_or_default(),
            region,
            country,
            country_code: country_code.unwrap_or_default(),
        }),
        favorite: row.get::<_, i64>(35)? != 0,
        rating: u8::try_from(row.get::<_, i64>(36)?).unwrap_or(0),
        hidden: row.get::<_, i64>(37)? != 0,
        caption: row.get(38)?,
        trashed_ms: row.get(39)?,
        screenshot: row.get::<_, i64>(40)? != 0,
        content_identifier: row.get(41)?,
        motion_video_offset: row.get::<_, Option<i64>>(42)?.map(u64_of),
        live_video: row.get::<_, Option<i64>>(43)?.map(AssetId),
        live_component: row.get::<_, i64>(44)? != 0,
        color: opt_u32(row.get(45)?),
        #[allow(clippy::cast_sign_loss)]
        phash: row.get::<_, Option<i64>>(46)?.map(|value| value as u64),
        thumb_state: code(47, ThumbState::from_code(row.get(47)?))?,
        file_state: code(48, FileStatus::from_code(row.get(48)?))?,
        added_ms: row.get(49)?,
        updated_ms: row.get(50)?,
    })
}

/// Column list matching [`file_from_row`], for a table aliased `f`.
pub(crate) const FILE_COLUMNS: &str = "f.id, f.asset_id, f.source_id, f.relpath, f.size, f.mtime_ns, \
    f.ctime_ns, f.btime_ns, f.device, f.inode, f.status, f.hash, f.checked_ms, f.added_ms, f.trash_path";

pub(crate) fn file_from_row(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        id: FileId(row.get(0)?),
        asset_id: AssetId(row.get(1)?),
        source_id: SourceId(row.get(2)?),
        relpath: PathBytes::from_vec(row.get(3)?),
        stamp: FileStamp {
            size: u64_of(row.get(4)?),
            mtime_ns: row.get(5)?,
            ctime_ns: row.get(6)?,
            btime_ns: row.get(7)?,
            #[allow(clippy::cast_sign_loss)]
            device: row.get::<_, i64>(8)? as u64,
            #[allow(clippy::cast_sign_loss)]
            inode: row.get::<_, i64>(9)? as u64,
        },
        status: code(10, FileStatus::from_code(row.get(10)?))?,
        hash: hash32(row.get(11)?),
        checked_ms: row.get(12)?,
        added_ms: row.get(13)?,
        trash_path: row.get::<_, Option<Vec<u8>>>(14)?.map(PathBytes::from_vec),
    })
}

/// Stores unsigned 64-bit values that may exceed `i64::MAX` by bit pattern.
#[allow(clippy::cast_possible_wrap)]
pub(crate) const fn bits_i64(value: u64) -> i64 {
    value as i64
}

pub(crate) const OPERATION_COLUMNS: &str = "uuid, kind, state, source_path, temp_path, dest_relpath, \
    aside_path, source_size, source_mtime_ns, source_ctime_ns, source_btime_ns, source_device, \
    source_inode, hash, asset_id, file_id, batch, error, created_ms, updated_ms";

pub(crate) fn operation_from_row(row: &Row<'_>) -> rusqlite::Result<Operation> {
    let uuid: Vec<u8> = row.get(0)?;
    let size: Option<i64> = row.get(7)?;
    let stamp = match (
        size,
        row.get::<_, Option<i64>>(8)?,
        row.get::<_, Option<i64>>(9)?,
        row.get::<_, Option<i64>>(11)?,
        row.get::<_, Option<i64>>(12)?,
    ) {
        (Some(size), Some(mtime_ns), Some(ctime_ns), Some(device), Some(inode)) => {
            Some(FileStamp {
                size: u64_of(size),
                mtime_ns,
                ctime_ns,
                btime_ns: row.get(10)?,
                #[allow(clippy::cast_sign_loss)]
                device: device as u64,
                #[allow(clippy::cast_sign_loss)]
                inode: inode as u64,
            })
        }
        _ => None,
    };
    let path = |index: usize| -> rusqlite::Result<Option<PathBytes>> {
        Ok(row
            .get::<_, Option<Vec<u8>>>(index)?
            .map(PathBytes::from_vec))
    };
    Ok(Operation {
        uuid: uuid_of(0, &uuid, OperationUuid::from_slice)?,
        kind: code(1, OperationKind::from_code(row.get(1)?))?,
        state: code(2, OperationState::from_code(row.get(2)?))?,
        source_path: path(3)?,
        temp_path: path(4)?,
        dest_relpath: path(5)?,
        aside_path: path(6)?,
        source_stamp: stamp,
        hash: hash32(row.get(13)?),
        asset_id: row.get::<_, Option<i64>>(14)?.map(AssetId),
        file_id: row.get::<_, Option<i64>>(15)?.map(FileId),
        batch: row.get(16)?,
        error: row.get(17)?,
        created_ms: row.get(18)?,
        updated_ms: row.get(19)?,
    })
}

pub(crate) fn source_kind(index: usize, value: i64) -> rusqlite::Result<SourceKind> {
    code(index, SourceKind::from_code(value))
}

pub(crate) fn source_status(index: usize, value: i64) -> rusqlite::Result<SourceStatus> {
    code(index, SourceStatus::from_code(value))
}

pub(crate) fn review_kind(index: usize, value: i64) -> rusqlite::Result<ReviewKind> {
    code(index, ReviewKind::from_code(value))
}

pub(crate) fn review_state(index: usize, value: i64) -> rusqlite::Result<ReviewState> {
    code(index, ReviewState::from_code(value))
}

pub(crate) fn album_sort(index: usize, value: i64) -> rusqlite::Result<AlbumSort> {
    code(index, AlbumSort::from_code(value))
}
