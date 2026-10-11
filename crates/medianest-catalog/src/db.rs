//! Read access to a catalog.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use medianest_core::{
    AlbumId, AssetId, FileId, FileStatus, Format, LibraryUuid, OperationState, OperationUuid,
    PathBytes, ReviewId, ReviewKind, ReviewState, SourceId, SourceKind, TagId, ThumbState,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, params_from_iter};

use crate::convert::{
    ASSET_COLUMNS, FILE_COLUMNS, OPERATION_COLUMNS, album_sort, asset_from_row, file_from_row,
    operation_from_row, review_kind, review_state, source_kind, source_status, u64_of,
};
use crate::records::{
    Album, Asset, Facet, FileLocation, FileRecord, JobRecord, JobState, LibraryStats, MapPoint,
    Operation, ReviewItem, Source, Tag, ThumbnailWork, TimelineItem, flags,
};
use crate::view::{View, compile};
use crate::{CatalogError, Result};

/// A connection to a catalog. Readers open it read-only; the [`crate::Writer`]
/// wraps one opened for writing.
#[derive(Debug)]
pub struct Db {
    pub(crate) conn: Connection,
    path: PathBuf,
}

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(30))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "temp_store", "memory")?;
    conn.pragma_update(None, "cache_size", -32_000)?;
    conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
    Ok(())
}

impl Db {
    /// Opens an existing catalog for reading only.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        conn.pragma_update(None, "query_only", true)?;
        let version = crate::schema::version(&conn)?;
        if version == 0 {
            return Err(CatalogError::NotACatalog);
        }
        if version > crate::schema::latest_version() {
            return Err(CatalogError::TooNew {
                found: version,
                supported: crate::schema::latest_version(),
            });
        }
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn from_connection(conn: Connection, path: &Path) -> Self {
        Self {
            conn,
            path: path.to_path_buf(),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // ----- library ---------------------------------------------------------

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn library_uuid(&self) -> Result<LibraryUuid> {
        self.meta("library_uuid")?
            .and_then(|value| value.parse().ok())
            .ok_or(CatalogError::NotACatalog)
    }

    /// SQLite's own consistency check. Returns `ok` or a list of problems.
    pub fn quick_check(&self) -> Result<String> {
        let mut statement = self.conn.prepare("PRAGMA quick_check")?;
        let lines: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(lines.join("\n"))
    }

    // ----- sources ---------------------------------------------------------

    const SOURCE_SELECT: &'static str = "SELECT s.id, s.kind, s.path, s.name, s.watch, s.status, s.added_ms, s.scanned_ms, \
        (SELECT count(*) FROM files f WHERE f.source_id = s.id), \
        (SELECT coalesce(sum(f.size), 0) FROM files f WHERE f.source_id = s.id), \
        (SELECT count(*) FROM files f WHERE f.source_id = s.id AND f.status IN (3, 4)) \
        FROM sources s";

    fn source_from_row(row: &Row<'_>) -> rusqlite::Result<Source> {
        Ok(Source {
            id: SourceId(row.get(0)?),
            kind: source_kind(1, row.get(1)?)?,
            path: PathBytes::from_vec(row.get(2)?),
            name: row.get(3)?,
            watch: row.get::<_, i64>(4)? != 0,
            status: source_status(5, row.get(5)?)?,
            added_ms: row.get(6)?,
            scanned_ms: row.get(7)?,
            file_count: u64_of(row.get(8)?),
            total_bytes: u64_of(row.get(9)?),
            problem_count: u64_of(row.get(10)?),
        })
    }

    pub fn sources(&self) -> Result<Vec<Source>> {
        let sql = format!(
            "{} ORDER BY s.kind, s.name COLLATE NOCASE",
            Self::SOURCE_SELECT
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([], Self::source_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn source(&self, id: SourceId) -> Result<Source> {
        let sql = format!("{} WHERE s.id = ?", Self::SOURCE_SELECT);
        self.conn
            .query_row(&sql, [id.get()], Self::source_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("source"))
    }

    pub fn source_by_path(&self, path: &Path) -> Result<Option<Source>> {
        let sql = format!("{} WHERE s.path = ?", Self::SOURCE_SELECT);
        Ok(self
            .conn
            .query_row(
                &sql,
                [PathBytes::from_path(path).as_bytes()],
                Self::source_from_row,
            )
            .optional()?)
    }

    pub fn managed_source(&self) -> Result<Source> {
        let sql = format!("{} WHERE s.kind = ? LIMIT 1", Self::SOURCE_SELECT);
        self.conn
            .query_row(&sql, [SourceKind::Managed.code()], Self::source_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("managed source"))
    }

    /// Relative paths the user removed from the library; rescans skip them.
    pub fn exclusions(&self, source: SourceId) -> Result<HashSet<Vec<u8>>> {
        let mut statement = self
            .conn
            .prepare("SELECT relpath FROM exclusions WHERE source_id = ?")?;
        let rows = statement.query_map([source.get()], |row| row.get::<_, Vec<u8>>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- assets and files -----------------------------------------------

    pub fn asset(&self, id: AssetId) -> Result<Asset> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets a WHERE a.id = ?");
        self.conn
            .query_row(&sql, [id.get()], asset_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("asset"))
    }

    pub fn try_asset(&self, id: AssetId) -> Result<Option<Asset>> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets a WHERE a.id = ?");
        Ok(self
            .conn
            .query_row(&sql, [id.get()], asset_from_row)
            .optional()?)
    }

    pub fn assets(&self, ids: &[AssetId]) -> Result<Vec<Asset>> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets a WHERE a.id = ?");
        let mut statement = self.conn.prepare_cached(&sql)?;
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(asset) = statement.query_row([id.get()], asset_from_row).optional()? {
                result.push(asset);
            }
        }
        Ok(result)
    }

    pub fn asset_by_hash(&self, hash: &[u8; 32]) -> Result<Option<AssetId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM assets WHERE hash = ?",
                [hash.as_slice()],
                |row| row.get(0).map(AssetId),
            )
            .optional()?)
    }

    fn location_from_row(row: &Row<'_>) -> rusqlite::Result<FileLocation> {
        Ok(FileLocation {
            file: file_from_row(row)?,
            source_kind: source_kind(15, row.get(15)?)?,
            source_path: PathBytes::from_vec(row.get(16)?),
            source_name: row.get(17)?,
            source_status: source_status(18, row.get(18)?)?,
        })
    }

    fn location_sql(filter: &str) -> String {
        format!(
            "SELECT {FILE_COLUMNS}, s.kind, s.path, s.name, s.status FROM files f \
             JOIN sources s ON s.id = f.source_id {filter}"
        )
    }

    /// Every location of an asset, best first: present before missing, managed before linked.
    pub fn files_of(&self, asset: AssetId) -> Result<Vec<FileLocation>> {
        let sql = Self::location_sql(
            "WHERE f.asset_id = ? ORDER BY f.status IN (3, 4), s.status, s.kind, f.id",
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map([asset.get()], Self::location_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The location to read an asset from, if any is reachable.
    pub fn best_file(&self, asset: AssetId) -> Result<Option<FileLocation>> {
        Ok(self
            .files_of(asset)?
            .into_iter()
            .find(|location| !matches!(location.file.status, FileStatus::Missing)))
    }

    pub fn file(&self, id: FileId) -> Result<FileLocation> {
        let sql = Self::location_sql("WHERE f.id = ?");
        self.conn
            .query_row(&sql, [id.get()], Self::location_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("file"))
    }

    pub fn file_by_path(
        &self,
        source: SourceId,
        relpath: &PathBytes,
    ) -> Result<Option<FileRecord>> {
        let sql =
            format!("SELECT {FILE_COLUMNS} FROM files f WHERE f.source_id = ? AND f.relpath = ?");
        Ok(self
            .conn
            .query_row(
                &sql,
                params![source.get(), relpath.as_bytes()],
                file_from_row,
            )
            .optional()?)
    }

    /// All files of a source, for reconciliation during a rescan.
    pub fn source_files(&self, source: SourceId) -> Result<Vec<FileRecord>> {
        let sql = format!("SELECT {FILE_COLUMNS} FROM files f WHERE f.source_id = ?");
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([source.get()], file_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Files in a given status, in id order after `after`.
    pub fn files_with_status(
        &self,
        status: FileStatus,
        after: FileId,
        limit: u32,
    ) -> Result<Vec<FileLocation>> {
        let sql = Self::location_sql(
            "WHERE f.status = ? AND f.id > ? AND s.status = 1 AND f.trash_path IS NULL ORDER BY f.id LIMIT ?",
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map(
            params![status.code(), after.get(), limit],
            Self::location_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Files to re-verify: present files of the given source kind not checked since `before_ms`.
    pub fn files_to_verify(
        &self,
        kind: Option<SourceKind>,
        before_ms: i64,
        after: FileId,
        limit: u32,
    ) -> Result<Vec<FileLocation>> {
        let sql = Self::location_sql(
            "WHERE f.id > ?1 AND f.status IN (1, 2, 4) AND s.status = 1 AND f.trash_path IS NULL \
             AND (?2 IS NULL OR s.kind = ?2) AND coalesce(f.checked_ms, 0) < ?3 \
             ORDER BY f.id LIMIT ?4",
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map(
            params![after.get(), kind.map(SourceKind::code), before_ms, limit],
            Self::location_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Files of a given size, for rename detection and duplicate checks.
    pub fn files_with_size(&self, size: u64) -> Result<Vec<FileLocation>> {
        let sql = Self::location_sql("WHERE f.size = ?");
        let mut statement = self.conn.prepare_cached(&sql)?;
        let rows = statement.query_map([crate::convert::i64_of(size)], Self::location_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- timeline --------------------------------------------------------

    /// The items of a view, in display order.
    pub fn timeline(&self, view: &View) -> Result<Vec<TimelineItem>> {
        let compiled = compile(view);
        let sql = format!(
            "SELECT a.id, a.uuid, a.taken_local, a.width, a.height, a.orientation, a.rotation, a.color, \
             a.kind, a.favorite, a.format, (a.live_video_id IS NOT NULL OR a.motion_offset IS NOT NULL), \
             a.file_state, a.taken_confidence, a.hidden, a.trashed_ms IS NOT NULL, a.thumb_state, \
             a.rating, a.duration_ms {} {}",
            compiled.from_where, compiled.order_by
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(compiled.params.iter()), |row| {
            let uuid: Vec<u8> = row.get(1)?;
            let width: Option<i64> = row.get(3)?;
            let height: Option<i64> = row.get(4)?;
            let orientation: i64 = row.get(5)?;
            let rotation: i64 = row.get(6)?;
            let video = row.get::<_, i64>(8)? == 2;
            let format: String = row.get(10)?;
            let mut item_flags = 0u16;
            let mut set = |condition: bool, flag: u16| {
                if condition {
                    item_flags |= flag;
                }
            };
            set(row.get::<_, i64>(9)? != 0, flags::FAVORITE);
            set(video, flags::VIDEO);
            set(row.get::<_, i64>(11)? != 0, flags::LIVE);
            set(
                Format::from_code(&format).is_ok_and(Format::is_raw),
                flags::RAW,
            );
            let file_state: i64 = row.get(12)?;
            set(file_state == FileStatus::Missing.code(), flags::MISSING);
            set(file_state == FileStatus::Changed.code(), flags::CHANGED);
            set(
                file_state == FileStatus::Unverified.code(),
                flags::UNVERIFIED,
            );
            let local: Option<i64> = row.get(2)?;
            set(
                local.is_some() && row.get::<_, i64>(13)? <= 1,
                flags::UNCERTAIN_DATE,
            );
            set(row.get::<_, i64>(14)? != 0, flags::HIDDEN);
            set(row.get::<_, bool>(15)?, flags::TRASHED);
            let thumb: i64 = row.get(16)?;
            set(thumb == ThumbState::Failed.code(), flags::THUMB_FAILED);
            set(thumb == ThumbState::Ready.code(), flags::THUMB_READY);
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let aspect = match (width, height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => {
                    let swap = (5..=8).contains(&orientation) ^ (rotation % 180 == 90);
                    let ratio = if swap {
                        h as f32 / w as f32
                    } else {
                        w as f32 / h as f32
                    };
                    ratio.clamp(0.2, 5.0)
                }
                _ if video => 16.0 / 9.0,
                _ => 1.0,
            };
            Ok(TimelineItem {
                id: AssetId(row.get(0)?),
                uuid: crate::convert::uuid_of(1, &uuid, medianest_core::AssetUuid::from_slice)?,
                local_ms: local,
                aspect,
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                color: row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u32,
                flags: item_flags,
                rating: u8::try_from(row.get::<_, i64>(17)?).unwrap_or(0),
                duration_ms: row
                    .get::<_, Option<i64>>(18)?
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Asset ids of a view in display order.
    pub fn view_ids(&self, view: &View) -> Result<Vec<AssetId>> {
        let compiled = compile(view);
        let sql = format!("SELECT a.id {} {}", compiled.from_where, compiled.order_by);
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(compiled.params.iter()), |row| {
            row.get(0).map(AssetId)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn count(&self, view: &View) -> Result<u64> {
        let compiled = compile(view);
        let sql = format!("SELECT count(*) {}", compiled.from_where);
        let count: i64 =
            self.conn
                .query_row(&sql, params_from_iter(compiled.params.iter()), |row| {
                    row.get(0)
                })?;
        Ok(u64_of(count))
    }

    /// The first asset of a view, used as an album or place cover.
    pub fn first_of(&self, view: &View) -> Result<Option<AssetId>> {
        let compiled = compile(view);
        let sql = format!(
            "SELECT a.id {} {} LIMIT 1",
            compiled.from_where, compiled.order_by
        );
        Ok(self
            .conn
            .query_row(&sql, params_from_iter(compiled.params.iter()), |row| {
                row.get(0).map(AssetId)
            })
            .optional()?)
    }

    pub fn map_points(&self, view: &View) -> Result<Vec<MapPoint>> {
        let compiled = compile(view);
        let sql = format!(
            "SELECT a.id, a.latitude, a.longitude {} AND a.latitude IS NOT NULL {}",
            if compiled.from_where.contains(" WHERE ") {
                compiled.from_where.clone()
            } else {
                format!("{} WHERE 1", compiled.from_where)
            },
            compiled.order_by
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(compiled.params.iter()), |row| {
            Ok(MapPoint {
                id: AssetId(row.get(0)?),
                latitude: row.get(1)?,
                longitude: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- tags and albums -------------------------------------------------

    pub fn tags(&self) -> Result<Vec<Tag>> {
        let mut statement = self.conn.prepare(
            "SELECT t.id, t.name, (SELECT count(*) FROM asset_tags at JOIN assets a ON a.id = at.asset_id \
             WHERE at.tag_id = t.id AND a.trashed_ms IS NULL) FROM tags t ORDER BY t.norm",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Tag {
                id: TagId(row.get(0)?),
                name: row.get(1)?,
                count: u64_of(row.get(2)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tag_by_name(&self, name: &str) -> Result<Option<TagId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM tags WHERE norm = ?",
                [medianest_core::normalize_tag(name)],
                |row| row.get(0).map(TagId),
            )
            .optional()?)
    }

    pub fn tags_of(&self, asset: AssetId) -> Result<Vec<Tag>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT t.id, t.name, 0 FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
             WHERE at.asset_id = ? ORDER BY t.norm",
        )?;
        let rows = statement.query_map([asset.get()], |row| {
            Ok(Tag {
                id: TagId(row.get(0)?),
                name: row.get(1)?,
                count: 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Tags used by any of `assets`, with how many of them carry each tag.
    pub fn tags_in(&self, assets: &[AssetId]) -> Result<Vec<Tag>> {
        if assets.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; assets.len()].join(",");
        let sql = format!(
            "SELECT t.id, t.name, count(*) FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
             WHERE at.asset_id IN ({placeholders}) GROUP BY t.id ORDER BY t.norm"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows =
            statement.query_map(params_from_iter(assets.iter().map(|id| id.get())), |row| {
                Ok(Tag {
                    id: TagId(row.get(0)?),
                    name: row.get(1)?,
                    count: u64_of(row.get(2)?),
                })
            })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    const ALBUM_SELECT: &'static str = "SELECT al.id, al.name, al.query, al.cover_id, al.sort, \
        (SELECT count(*) FROM album_assets aa JOIN assets a ON a.id = aa.asset_id \
         WHERE aa.album_id = al.id AND a.trashed_ms IS NULL), al.created_ms, al.updated_ms FROM albums al";

    fn album_from_row(row: &Row<'_>) -> rusqlite::Result<Album> {
        Ok(Album {
            id: AlbumId(row.get(0)?),
            name: row.get(1)?,
            query: row.get(2)?,
            cover: row.get::<_, Option<i64>>(3)?.map(AssetId),
            sort: album_sort(4, row.get(4)?)?,
            count: u64_of(row.get(5)?),
            created_ms: row.get(6)?,
            updated_ms: row.get(7)?,
        })
    }

    pub fn albums(&self) -> Result<Vec<Album>> {
        let sql = format!("{} ORDER BY al.name COLLATE NOCASE", Self::ALBUM_SELECT);
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([], Self::album_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn album(&self, id: AlbumId) -> Result<Album> {
        let sql = format!("{} WHERE al.id = ?", Self::ALBUM_SELECT);
        self.conn
            .query_row(&sql, [id.get()], Self::album_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("album"))
    }

    pub fn albums_of(&self, asset: AssetId) -> Result<Vec<Album>> {
        let sql = format!(
            "{} WHERE al.id IN (SELECT album_id FROM album_assets WHERE asset_id = ?) \
             ORDER BY al.name COLLATE NOCASE",
            Self::ALBUM_SELECT
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([asset.get()], Self::album_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- facets ----------------------------------------------------------

    fn facets(&self, sql: &str, prefix: &str, limit: u32) -> Result<Vec<Facet>> {
        let pattern = format!("{}%", prefix.trim().replace(['%', '_'], ""));
        let mut statement = self.conn.prepare_cached(sql)?;
        let rows = statement.query_map(params![pattern, limit], |row| {
            Ok(Facet {
                value: row.get(0)?,
                detail: row.get(1)?,
                count: u64_of(row.get(2)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Cities and countries with photo counts, most photographed first.
    pub fn places(&self, prefix: &str, limit: u32) -> Result<Vec<Facet>> {
        self.facets(
            "SELECT place_city, place_country, count(*) AS n FROM assets \
             WHERE place_country IS NOT NULL AND trashed_ms IS NULL AND hidden = 0 AND live_component = 0 \
             AND (place_city LIKE ?1 OR place_country LIKE ?1) \
             GROUP BY place_city, place_country ORDER BY n DESC LIMIT ?2",
            prefix,
            limit,
        )
    }

    pub fn countries(&self) -> Result<Vec<Facet>> {
        self.facets(
            "SELECT place_country, place_cc, count(*) AS n FROM assets \
             WHERE place_country IS NOT NULL AND trashed_ms IS NULL AND hidden = 0 AND live_component = 0 \
             AND place_country LIKE ?1 GROUP BY place_country ORDER BY n DESC LIMIT ?2",
            "",
            1000,
        )
    }

    pub fn cameras(&self, prefix: &str, limit: u32) -> Result<Vec<Facet>> {
        let mut found = self.facets(
            "SELECT trim(coalesce(camera_make, '') || ' ' || coalesce(camera_model, '')) AS cam, \
             NULL, count(*) AS n FROM assets WHERE camera_model IS NOT NULL AND trashed_ms IS NULL \
             AND (camera_model LIKE ?1 OR camera_make LIKE ?1 OR ?1 = '%') \
             GROUP BY camera_make, camera_model ORDER BY n DESC LIMIT ?2",
            prefix,
            limit,
        )?;
        for facet in &mut found {
            let mut words = facet.value.splitn(2, ' ');
            let make = words.next();
            let model = words.next();
            if let Some(name) = medianest_core::camera_name(make, model) {
                facet.value = name;
            }
        }
        Ok(found)
    }

    pub fn tag_suggestions(&self, prefix: &str, limit: u32) -> Result<Vec<Facet>> {
        self.facets(
            "SELECT t.name, NULL, count(at.asset_id) AS n FROM tags t \
             LEFT JOIN asset_tags at ON at.tag_id = t.id WHERE t.norm LIKE lower(?1) \
             GROUP BY t.id ORDER BY n DESC, t.norm LIMIT ?2",
            prefix,
            limit,
        )
    }

    pub fn album_suggestions(&self, prefix: &str, limit: u32) -> Result<Vec<Facet>> {
        self.facets(
            "SELECT name, NULL, (SELECT count(*) FROM album_assets WHERE album_id = albums.id) AS n \
             FROM albums WHERE query IS NULL AND name LIKE ?1 ORDER BY name COLLATE NOCASE LIMIT ?2",
            prefix,
            limit,
        )
    }

    /// Number of visible assets per year, newest first.
    pub fn years(&self) -> Result<Vec<(i32, u64)>> {
        let mut statement = self.conn.prepare(
            "SELECT CAST(strftime('%Y', taken_local / 1000, 'unixepoch') AS INTEGER) AS y, count(*) \
             FROM assets WHERE taken_local IS NOT NULL AND trashed_ms IS NULL AND hidden = 0 \
             AND live_component = 0 GROUP BY y ORDER BY y DESC",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, u64_of(row.get(1)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- review, operations, jobs ----------------------------------------

    pub fn reviews(&self, kind: Option<ReviewKind>, limit: u32) -> Result<Vec<ReviewItem>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT r.id, r.kind, r.asset_id, r.file_id, r.operation, r.path, r.detail, r.state, \
             r.created_ms, r.updated_ms, coalesce(a.name, CAST(f.relpath AS TEXT), CAST(r.path AS TEXT), '') \
             FROM review_items r LEFT JOIN assets a ON a.id = r.asset_id LEFT JOIN files f ON f.id = r.file_id \
             WHERE r.state = 0 AND (?1 IS NULL OR r.kind = ?1) ORDER BY r.kind, r.created_ms DESC, r.id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![kind.map(ReviewKind::code), limit], |row| {
            let operation: Option<Vec<u8>> = row.get(4)?;
            let detail: String = row.get(6)?;
            let subject: String = row.get(10)?;
            Ok(ReviewItem {
                id: ReviewId(row.get(0)?),
                kind: review_kind(1, row.get(1)?)?,
                asset_id: row.get::<_, Option<i64>>(2)?.map(AssetId),
                file_id: row.get::<_, Option<i64>>(3)?.map(FileId),
                operation: operation.as_deref().and_then(OperationUuid::from_slice),
                path: row.get::<_, Option<Vec<u8>>>(5)?.map(PathBytes::from_vec),
                detail: serde_json::from_str(&detail).unwrap_or(serde_json::Value::Null),
                state: review_state(7, row.get(7)?)?,
                created_ms: row.get(8)?,
                updated_ms: row.get(9)?,
                subject: subject
                    .rsplit('/')
                    .next()
                    .map_or_else(String::new, str::to_owned),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn review(&self, id: ReviewId) -> Result<ReviewItem> {
        self.reviews(None, u32::MAX)?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or(CatalogError::NotFound("review item"))
    }

    /// Open review items per kind.
    pub fn review_counts(&self) -> Result<Vec<(ReviewKind, u64)>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT kind, count(*) FROM review_items WHERE state = ? GROUP BY kind ORDER BY kind",
        )?;
        let rows = statement.query_map([ReviewState::Open.code()], |row| {
            Ok((review_kind(0, row.get(0)?)?, u64_of(row.get(1)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn operation(&self, uuid: OperationUuid) -> Result<Operation> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE uuid = ?");
        self.conn
            .query_row(&sql, [uuid.as_bytes().as_slice()], operation_from_row)
            .optional()?
            .ok_or(CatalogError::NotFound("operation"))
    }

    /// Operations that have not reached a terminal state, oldest first.
    pub fn unfinished_operations(&self) -> Result<Vec<Operation>> {
        let sql = format!(
            "SELECT {OPERATION_COLUMNS} FROM operations WHERE state NOT IN (?, ?, ?) ORDER BY created_ms"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(
            params![
                OperationState::Completed.code(),
                OperationState::Failed.code(),
                OperationState::Cancelled.code()
            ],
            operation_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn operations_in_state(&self, state: OperationState) -> Result<Vec<Operation>> {
        let sql = format!(
            "SELECT {OPERATION_COLUMNS} FROM operations WHERE state = ? ORDER BY created_ms"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([state.code()], operation_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn jobs(&self, limit: u32) -> Result<Vec<JobRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT id, kind, title, state, started_ms, finished_ms, summary FROM jobs \
             ORDER BY started_ms DESC, id DESC LIMIT ?",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok(JobRecord {
                id: medianest_core::JobId(row.get(0)?),
                kind: row.get(1)?,
                title: row.get(2)?,
                state: JobState::from_code(row.get(3)?),
                started_ms: row.get(4)?,
                finished_ms: row.get(5)?,
                summary: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Most recent finished job of a kind.
    pub fn last_job(&self, kind: &str) -> Result<Option<JobRecord>> {
        Ok(self
            .jobs(200)?
            .into_iter()
            .find(|job| job.kind == kind && job.state == JobState::Completed))
    }

    // ----- thumbnails, similarity -----------------------------------------

    fn thumbnail_work_from(
        &self,
        id: AssetId,
        uuid: &[u8],
        format: &str,
        orientation: i64,
        motion: Option<i64>,
    ) -> Result<Option<ThumbnailWork>> {
        let Some(location) = self.best_file(id)? else {
            return Ok(None);
        };
        Ok(Some(ThumbnailWork {
            id,
            uuid: medianest_core::AssetUuid::from_slice(uuid)
                .ok_or(CatalogError::NotFound("uuid"))?,
            format: Format::from_code(format).map_err(|_| CatalogError::NotFound("format"))?,
            orientation: u8::try_from(orientation).unwrap_or(1),
            motion_video_offset: motion.map(u64_of),
            path: location.path(),
        }))
    }

    /// Assets still waiting for a thumbnail, newest first.
    pub fn thumbnails_pending(&self, limit: u32) -> Result<Vec<ThumbnailWork>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT id, uuid, format, orientation, motion_offset FROM assets WHERE thumb_state = 0 \
             ORDER BY taken_local IS NULL, taken_local DESC LIMIT ?",
        )?;
        type Row = (i64, Vec<u8>, String, i64, Option<i64>);
        let rows: Vec<Row> = statement
            .query_map([limit], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut work = Vec::with_capacity(rows.len());
        for (id, uuid, format, orientation, motion) in rows {
            if let Some(item) =
                self.thumbnail_work_from(AssetId(id), &uuid, &format, orientation, motion)?
            {
                work.push(item);
            }
        }
        Ok(work)
    }

    pub fn thumbnail_work(&self, id: AssetId) -> Result<Option<ThumbnailWork>> {
        let row: Option<(Vec<u8>, String, i64, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT uuid, format, orientation, motion_offset FROM assets WHERE id = ?",
                [id.get()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        match row {
            Some((uuid, format, orientation, motion)) => {
                self.thumbnail_work_from(id, &uuid, &format, orientation, motion)
            }
            None => Ok(None),
        }
    }

    /// Perceptual hashes of visible photos, with their capture time.
    pub fn perceptual_hashes(&self) -> Result<Vec<(AssetId, u64, Option<i64>)>> {
        let mut statement = self.conn.prepare(
            "SELECT id, phash, taken_local FROM assets WHERE phash IS NOT NULL AND kind = 1 \
             AND trashed_ms IS NULL AND hidden = 0 AND live_component = 0",
        )?;
        let rows = statement.query_map([], |row| {
            #[allow(clippy::cast_sign_loss)]
            Ok((
                AssetId(row.get(0)?),
                row.get::<_, i64>(1)? as u64,
                row.get(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn dismissed_similar(&self) -> Result<HashSet<(AssetId, AssetId)>> {
        let mut statement = self.conn.prepare("SELECT a, b FROM similar_dismissed")?;
        let rows =
            statement.query_map([], |row| Ok((AssetId(row.get(0)?), AssetId(row.get(1)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Assets stored in more than one place.
    pub fn assets_with_copies(&self) -> Result<Vec<(AssetId, u64)>> {
        let mut statement = self.conn.prepare(
            "SELECT f.asset_id, count(*) AS n FROM files f JOIN assets a ON a.id = f.asset_id \
             WHERE a.trashed_ms IS NULL GROUP BY f.asset_id HAVING n > 1 ORDER BY n DESC",
        )?;
        let rows =
            statement.query_map([], |row| Ok((AssetId(row.get(0)?), u64_of(row.get(1)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Assets without a place name but with coordinates.
    pub fn assets_needing_place(&self, limit: u32) -> Result<Vec<(AssetId, f64, f64)>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT id, latitude, longitude FROM assets WHERE latitude IS NOT NULL \
             AND place_country IS NULL LIMIT ?",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((AssetId(row.get(0)?), row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Trashed assets deleted before `before_ms`.
    pub fn trashed_before(&self, before_ms: i64) -> Result<Vec<AssetId>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM assets WHERE trashed_ms IS NOT NULL AND trashed_ms < ?")?;
        let rows = statement.query_map([before_ms], |row| row.get(0).map(AssetId))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Where a trashed file was moved, as recorded when it was trashed.
    pub fn trash_paths(&self, asset: AssetId) -> Result<Vec<(FileId, PathBytes)>> {
        let mut statement = self.conn.prepare(
            "SELECT id, trash_path FROM files WHERE asset_id = ? AND trash_path IS NOT NULL",
        )?;
        let rows = statement.query_map([asset.get()], |row| {
            Ok((FileId(row.get(0)?), PathBytes::from_vec(row.get(1)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- statistics ------------------------------------------------------

    pub fn stats(&self) -> Result<LibraryStats> {
        let count = |sql: &str| -> Result<u64> {
            Ok(u64_of(
                self.conn.query_row(sql, [], |row| row.get::<_, i64>(0))?,
            ))
        };
        let visible = "trashed_ms IS NULL AND live_component = 0";
        Ok(LibraryStats {
            assets: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND hidden = 0"
            ))?,
            photos: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND hidden = 0 AND kind = 1"
            ))?,
            videos: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND hidden = 0 AND kind = 2"
            ))?,
            favorites: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND favorite = 1"
            ))?,
            hidden: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND hidden = 1"
            ))?,
            trashed: count("SELECT count(*) FROM assets WHERE trashed_ms IS NOT NULL")?,
            unknown_dates: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND taken_local IS NULL"
            ))?,
            uncertain_dates: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND taken_local IS NOT NULL AND taken_confidence <= 1"
            ))?,
            with_location: count(&format!(
                "SELECT count(*) FROM assets WHERE {visible} AND latitude IS NOT NULL"
            ))?,
            files: count("SELECT count(*) FROM files")?,
            managed_files: count(
                "SELECT count(*) FROM files f JOIN sources s ON s.id = f.source_id WHERE s.kind = 1",
            )?,
            linked_files: count(
                "SELECT count(*) FROM files f JOIN sources s ON s.id = f.source_id WHERE s.kind = 2",
            )?,
            managed_bytes: count(
                "SELECT coalesce(sum(f.size), 0) FROM files f JOIN sources s ON s.id = f.source_id WHERE s.kind = 1",
            )?,
            linked_bytes: count(
                "SELECT coalesce(sum(f.size), 0) FROM files f JOIN sources s ON s.id = f.source_id WHERE s.kind = 2",
            )?,
            verified_files: count("SELECT count(*) FROM files WHERE status = 2")?,
            unverified_files: count("SELECT count(*) FROM files WHERE status = 1")?,
            missing_files: count("SELECT count(*) FROM files WHERE status = 3")?,
            changed_files: count("SELECT count(*) FROM files WHERE status = 4")?,
            thumbs_pending: count("SELECT count(*) FROM assets WHERE thumb_state = 0")?,
            thumbs_failed: count("SELECT count(*) FROM assets WHERE thumb_state = 2")?,
            tags: count("SELECT count(*) FROM tags")?,
            albums: count("SELECT count(*) FROM albums")?,
            open_reviews: count("SELECT count(*) FROM review_items WHERE state = 0")?,
            oldest_check_ms: self.conn.query_row(
                "SELECT min(checked_ms) FROM files WHERE status IN (2, 4)",
                [],
                |row| row.get(0),
            )?,
        })
    }
}
