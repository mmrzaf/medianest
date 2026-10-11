//! Write access to a catalog.
//!
//! All writes go through one [`Writer`]. Each public method is a single
//! transaction, so the catalog is always consistent on disk.

use std::ops::Deref;
use std::path::Path;

use medianest_core::{
    AlbumId, AlbumSort, AssetId, AssetMetadata, AssetUuid, CaptureTime, Confidence, DateSource,
    FileId, FileStamp, FileStatus, JobId, LibraryUuid, MediaKind, OperationUuid, PathBytes, Place,
    ReviewId, ReviewKind, ReviewState, SourceId, SourceKind, SourceStatus, TagId, ThumbState,
    normalize_tag, now_ms, tidy_tag,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::convert::{bits_i64, i64_of};
use crate::db::{Db, configure};
use crate::records::{JobState, Operation};
use crate::{CatalogError, Result};

/// The single writable connection to a catalog.
#[derive(Debug)]
pub struct Writer {
    db: Db,
}

impl Deref for Writer {
    type Target = Db;

    fn deref(&self) -> &Db {
        &self.db
    }
}

/// What happened when a newly found file was catalogued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Added {
    pub asset: AssetId,
    pub file: FileId,
    /// `false` when the file was an exact copy of an existing asset.
    pub new_asset: bool,
}

/// A file location to record for a newly catalogued asset.
#[derive(Clone, Debug)]
pub struct NewFile<'a> {
    pub source: SourceId,
    pub relpath: &'a PathBytes,
    pub stamp: FileStamp,
    pub status: FileStatus,
    pub hash: Option<[u8; 32]>,
}

fn open_writable(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure(&conn)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(CatalogError::Unsupported(format!(
            "the catalog's filesystem does not support WAL journaling (got {mode})"
        )));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

impl Writer {
    /// Creates a new, empty catalog. Fails if the file already holds data.
    pub fn create(path: &Path, library: LibraryUuid, managed_root: &Path) -> Result<Self> {
        let mut conn = open_writable(path)?;
        let existing: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if existing != 0 {
            return Err(CatalogError::AlreadyExists);
        }
        crate::schema::migrate(&mut conn)?;
        let now = now_ms();
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('library_uuid', ?), ('created_ms', ?), ('clean_shutdown', '1')",
            params![library.to_string(), now.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO sources (kind, path, name, watch, status, added_ms) VALUES (?, ?, ?, 0, 1, ?)",
            params![
                SourceKind::Managed.code(),
                PathBytes::from_path(managed_root).as_bytes(),
                "Library",
                now
            ],
        )?;
        transaction.commit()?;
        Ok(Self {
            db: Db::from_connection(conn, path),
        })
    }

    /// Opens an existing catalog, applying migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(CatalogError::NotFound("catalog file"));
        }
        let mut conn = open_writable(path)?;
        crate::schema::migrate(&mut conn)?;
        let db = Db::from_connection(conn, path);
        db.library_uuid()?;
        Ok(Self { db })
    }

    fn tx(&mut self) -> Result<Transaction<'_>> {
        Ok(self.db.conn.transaction()?)
    }

    /// Writes the WAL back into the main file.
    pub fn checkpoint(&self) -> Result<()> {
        self.db
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.db.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// Updates the managed root path, for a library moved as a whole.
    pub fn set_managed_root(&mut self, root: &Path) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET path = ? WHERE kind = ?",
            params![
                PathBytes::from_path(root).as_bytes(),
                SourceKind::Managed.code()
            ],
        )?;
        Ok(())
    }

    // ----- sources ---------------------------------------------------------

    pub fn add_source(&mut self, path: &Path, name: &str) -> Result<SourceId> {
        let bytes = PathBytes::from_path(path);
        if self.source_by_path(path)?.is_some() {
            return Err(CatalogError::AlreadyExists);
        }
        self.db.conn.execute(
            "INSERT INTO sources (kind, path, name, watch, status, added_ms) VALUES (?, ?, ?, 1, 1, ?)",
            params![SourceKind::Linked.code(), bytes.as_bytes(), name, now_ms()],
        )?;
        Ok(SourceId(self.db.conn.last_insert_rowid()))
    }

    pub fn rename_source(&mut self, id: SourceId, name: &str) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET name = ? WHERE id = ?",
            params![name.trim(), id.get()],
        )?;
        Ok(())
    }

    /// Points a linked source at a new location, keeping every file record.
    pub fn relocate_source(&mut self, id: SourceId, path: &Path) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET path = ? WHERE id = ? AND kind = ?",
            params![
                PathBytes::from_path(path).as_bytes(),
                id.get(),
                SourceKind::Linked.code()
            ],
        )?;
        Ok(())
    }

    pub fn set_source_watch(&mut self, id: SourceId, watch: bool) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET watch = ? WHERE id = ?",
            params![watch, id.get()],
        )?;
        Ok(())
    }

    pub fn set_source_status(&mut self, id: SourceId, status: SourceStatus) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET status = ? WHERE id = ?",
            params![status.code(), id.get()],
        )?;
        Ok(())
    }

    pub fn mark_scanned(&mut self, id: SourceId) -> Result<()> {
        self.db.conn.execute(
            "UPDATE sources SET scanned_ms = ?, status = 1 WHERE id = ?",
            params![now_ms(), id.get()],
        )?;
        Ok(())
    }

    /// Forgets a linked source. Assets left without any file are removed and
    /// returned so their cached thumbnails can be deleted. Files on disk are
    /// not touched.
    pub fn remove_source(&mut self, id: SourceId) -> Result<Vec<AssetUuid>> {
        let transaction = self.tx()?;
        let kind: i64 = transaction
            .query_row("SELECT kind FROM sources WHERE id = ?", [id.get()], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(CatalogError::NotFound("source"))?;
        if kind == SourceKind::Managed.code() {
            return Err(CatalogError::Unsupported(
                "the library folder cannot be removed".into(),
            ));
        }
        let affected: Vec<i64> = {
            let mut statement =
                transaction.prepare("SELECT DISTINCT asset_id FROM files WHERE source_id = ?")?;
            statement
                .query_map([id.get()], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        transaction.execute("DELETE FROM sources WHERE id = ?", [id.get()])?;
        let mut removed = Vec::new();
        for asset in affected {
            if let Some(uuid) = delete_if_orphan(&transaction, AssetId(asset))? {
                removed.push(uuid);
            } else {
                refresh_file_state(&transaction, AssetId(asset))?;
                fts_refresh(&transaction, AssetId(asset))?;
            }
        }
        transaction.commit()?;
        Ok(removed)
    }

    /// Records that a linked file was removed from the library on purpose.
    pub fn exclude(&mut self, source: SourceId, relpath: &PathBytes) -> Result<()> {
        self.db.conn.execute(
            "INSERT OR IGNORE INTO exclusions (source_id, relpath, added_ms) VALUES (?, ?, ?)",
            params![source.get(), relpath.as_bytes(), now_ms()],
        )?;
        Ok(())
    }

    pub fn clear_exclusions(&mut self, source: SourceId) -> Result<usize> {
        Ok(self
            .db
            .conn
            .execute("DELETE FROM exclusions WHERE source_id = ?", [source.get()])?)
    }

    // ----- assets and files -----------------------------------------------

    /// Catalogs a newly found file. When its content hash matches an existing
    /// asset, the file becomes another location of that asset.
    pub fn add_file(
        &mut self,
        file: &NewFile<'_>,
        name: &str,
        metadata: &AssetMetadata,
    ) -> Result<Added> {
        let transaction = self.tx()?;
        let added = add_file_in(&transaction, file, name, metadata)?;
        transaction.commit()?;
        Ok(added)
    }

    /// Catalogs many files in one transaction. Used by folder scans.
    pub fn add_files(
        &mut self,
        items: &[(NewFile<'_>, String, AssetMetadata)],
    ) -> Result<Vec<Added>> {
        let transaction = self.tx()?;
        let mut added = Vec::with_capacity(items.len());
        for (file, name, metadata) in items {
            added.push(add_file_in(&transaction, file, name, metadata)?);
        }
        transaction.commit()?;
        Ok(added)
    }

    /// Records a new identity stamp after a rescan noticed a modified file.
    pub fn update_file_stamp(
        &mut self,
        file: FileId,
        stamp: &FileStamp,
        status: FileStatus,
    ) -> Result<()> {
        let transaction = self.tx()?;
        transaction.execute(
            "UPDATE files SET size = ?, mtime_ns = ?, ctime_ns = ?, btime_ns = ?, device = ?, inode = ?, \
             status = ?, updated_ms = ? WHERE id = ?",
            params![
                i64_of(stamp.size),
                stamp.mtime_ns,
                stamp.ctime_ns,
                stamp.btime_ns,
                bits_i64(stamp.device),
                bits_i64(stamp.inode),
                status.code(),
                now_ms(),
                file.get()
            ],
        )?;
        let asset = asset_of_file(&transaction, file)?;
        refresh_file_state(&transaction, asset)?;
        transaction.commit()?;
        Ok(())
    }

    /// Changes a file's status, keeping the review inbox in step.
    pub fn set_file_status(&mut self, file: FileId, status: FileStatus) -> Result<()> {
        let transaction = self.tx()?;
        set_file_status_in(&transaction, file, status)?;
        transaction.commit()?;
        Ok(())
    }

    /// Records a verified content hash for a file.
    ///
    /// If the asset had no hash yet, it gets this one. If another asset already
    /// has it, the two are the same content and are merged; the id of the
    /// asset that was merged away is returned.
    pub fn set_file_hash(&mut self, file: FileId, hash: &[u8; 32]) -> Result<Option<AssetId>> {
        let transaction = self.tx()?;
        let now = now_ms();
        let asset = asset_of_file(&transaction, file)?;
        transaction.execute(
            "UPDATE files SET hash = ?, status = ?, checked_ms = ?, updated_ms = ? WHERE id = ?",
            params![
                hash.as_slice(),
                FileStatus::Verified.code(),
                now,
                now,
                file.get()
            ],
        )?;
        resolve_for_file(
            &transaction,
            file,
            &[ReviewKind::MissingFile, ReviewKind::ChangedFile],
        )?;
        let current: Option<Vec<u8>> = transaction.query_row(
            "SELECT hash FROM assets WHERE id = ?",
            [asset.get()],
            |row| row.get(0),
        )?;
        let mut merged_away = None;
        match current {
            None => {
                let owner: Option<i64> = transaction
                    .query_row(
                        "SELECT id FROM assets WHERE hash = ?",
                        [hash.as_slice()],
                        |row| row.get(0),
                    )
                    .optional()?;
                match owner {
                    Some(owner) if owner != asset.get() => {
                        merge_into(&transaction, AssetId(owner), asset)?;
                        merged_away = Some(asset);
                        refresh_file_state(&transaction, AssetId(owner))?;
                        fts_refresh(&transaction, AssetId(owner))?;
                    }
                    _ => {
                        transaction.execute(
                            "UPDATE assets SET hash = ?, updated_ms = ? WHERE id = ?",
                            params![hash.as_slice(), now, asset.get()],
                        )?;
                        refresh_file_state(&transaction, asset)?;
                    }
                }
            }
            Some(existing) if existing.as_slice() == hash.as_slice() => {
                refresh_file_state(&transaction, asset)?;
            }
            Some(_) => {
                // Same path, different bytes: the file was edited outside MediaNest.
                set_file_status_in(&transaction, file, FileStatus::Changed)?;
            }
        }
        transaction.commit()?;
        Ok(merged_away)
    }

    /// Marks a file as checked without changing its hash.
    pub fn touch_checked(&mut self, file: FileId) -> Result<()> {
        self.db.conn.execute(
            "UPDATE files SET checked_ms = ? WHERE id = ?",
            params![now_ms(), file.get()],
        )?;
        Ok(())
    }

    /// Accepts a changed file's new content: the asset's hash, size and
    /// metadata are replaced and its thumbnail regenerated.
    pub fn accept_changed(
        &mut self,
        file: FileId,
        hash: &[u8; 32],
        stamp: &FileStamp,
        metadata: &AssetMetadata,
    ) -> Result<()> {
        let transaction = self.tx()?;
        let asset = asset_of_file(&transaction, file)?;
        let now = now_ms();
        let copies: i64 = transaction.query_row(
            "SELECT count(*) FROM files WHERE asset_id = ?",
            [asset.get()],
            |row| row.get(0),
        )?;
        let owner: Option<i64> = transaction
            .query_row(
                "SELECT id FROM assets WHERE hash = ? AND id != ?",
                params![hash.as_slice(), asset.get()],
                |row| row.get(0),
            )
            .optional()?;
        if copies > 1 || owner.is_some() {
            // Other copies still hold the old content: split this file into its own asset.
            let target = match owner {
                Some(owner) => AssetId(owner),
                None => {
                    let name: String = transaction.query_row(
                        "SELECT name FROM assets WHERE id = ?",
                        [asset.get()],
                        |row| row.get(0),
                    )?;
                    insert_asset(&transaction, metadata, &name, Some(hash), stamp.size)?.0
                }
            };
            transaction.execute(
                "UPDATE files SET asset_id = ? WHERE id = ?",
                params![target.get(), file.get()],
            )?;
            refresh_file_state(&transaction, asset)?;
            fts_refresh(&transaction, asset)?;
            refresh_file_state(&transaction, target)?;
            fts_refresh(&transaction, target)?;
        } else {
            transaction.execute(
                "UPDATE assets SET hash = ?, size = ?, thumb_state = 0, color = NULL, phash = NULL, updated_ms = ? WHERE id = ?",
                params![hash.as_slice(), i64_of(stamp.size), now, asset.get()],
            )?;
            update_probed(&transaction, asset, metadata)?;
        }
        transaction.execute(
            "UPDATE files SET hash = ?, size = ?, mtime_ns = ?, ctime_ns = ?, btime_ns = ?, device = ?, inode = ?, \
             status = ?, checked_ms = ?, updated_ms = ? WHERE id = ?",
            params![
                hash.as_slice(),
                i64_of(stamp.size),
                stamp.mtime_ns,
                stamp.ctime_ns,
                stamp.btime_ns,
                bits_i64(stamp.device),
                bits_i64(stamp.inode),
                FileStatus::Verified.code(),
                now,
                now,
                file.get()
            ],
        )?;
        resolve_for_file(&transaction, file, &[ReviewKind::ChangedFile])?;
        let current = asset_of_file(&transaction, file)?;
        refresh_file_state(&transaction, current)?;
        transaction.commit()?;
        Ok(())
    }

    /// Re-reads metadata for an asset, keeping everything the user set.
    pub fn refresh_metadata(&mut self, asset: AssetId, metadata: &AssetMetadata) -> Result<()> {
        let transaction = self.tx()?;
        update_probed(&transaction, asset, metadata)?;
        transaction.commit()?;
        Ok(())
    }

    /// A file moved within its source (detected by a rescan).
    pub fn move_file(
        &mut self,
        file: FileId,
        relpath: &PathBytes,
        stamp: &FileStamp,
    ) -> Result<()> {
        let transaction = self.tx()?;
        transaction.execute(
            "DELETE FROM files WHERE source_id = (SELECT source_id FROM files WHERE id = ?1) AND relpath = ?2 AND id != ?1",
            params![file.get(), relpath.as_bytes()],
        )?;
        transaction.execute(
            "UPDATE files SET relpath = ?, size = ?, mtime_ns = ?, ctime_ns = ?, btime_ns = ?, device = ?, inode = ?, \
             status = CASE WHEN hash IS NULL THEN 1 ELSE 2 END, updated_ms = ? WHERE id = ?",
            params![
                relpath.as_bytes(),
                i64_of(stamp.size),
                stamp.mtime_ns,
                stamp.ctime_ns,
                stamp.btime_ns,
                bits_i64(stamp.device),
                bits_i64(stamp.inode),
                now_ms(),
                file.get()
            ],
        )?;
        resolve_for_file(&transaction, file, &[ReviewKind::MissingFile])?;
        let asset = asset_of_file(&transaction, file)?;
        refresh_file_state(&transaction, asset)?;
        fts_refresh(&transaction, asset)?;
        transaction.commit()?;
        Ok(())
    }

    /// Forgets one file location. Returns the asset's uuid if it had no other
    /// location and was removed too.
    pub fn remove_file(&mut self, file: FileId) -> Result<Option<AssetUuid>> {
        let transaction = self.tx()?;
        let asset = asset_of_file(&transaction, file)?;
        transaction.execute("DELETE FROM files WHERE id = ?", [file.get()])?;
        let removed = delete_if_orphan(&transaction, asset)?;
        if removed.is_none() {
            refresh_file_state(&transaction, asset)?;
            fts_refresh(&transaction, asset)?;
        }
        transaction.commit()?;
        Ok(removed)
    }

    /// Removes an asset and all its file records from the catalog only.
    pub fn forget_asset(&mut self, asset: AssetId) -> Result<AssetUuid> {
        let transaction = self.tx()?;
        let uuid: Vec<u8> = transaction
            .query_row(
                "SELECT uuid FROM assets WHERE id = ?",
                [asset.get()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CatalogError::NotFound("asset"))?;
        transaction.execute("DELETE FROM assets_fts WHERE rowid = ?", [asset.get()])?;
        transaction.execute("DELETE FROM assets WHERE id = ?", [asset.get()])?;
        transaction.commit()?;
        AssetUuid::from_slice(&uuid).ok_or(CatalogError::NotFound("uuid"))
    }

    // ----- user edits ------------------------------------------------------

    fn update_each(
        &mut self,
        ids: &[AssetId],
        sql: &str,
        value: rusqlite::types::Value,
    ) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction.prepare(sql)?;
            let now = now_ms();
            for id in ids {
                statement.execute(params![value, now, id.get()])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn set_favorite(&mut self, ids: &[AssetId], favorite: bool) -> Result<()> {
        self.update_each(
            ids,
            "UPDATE assets SET favorite = ?, updated_ms = ? WHERE id = ?",
            i64::from(favorite).into(),
        )
    }

    pub fn set_rating(&mut self, ids: &[AssetId], rating: u8) -> Result<()> {
        if rating > 5 {
            return Err(CatalogError::Invalid(
                "rating must be between 0 and 5".into(),
            ));
        }
        self.update_each(
            ids,
            "UPDATE assets SET rating = ?, updated_ms = ? WHERE id = ?",
            i64::from(rating).into(),
        )
    }

    pub fn set_hidden(&mut self, ids: &[AssetId], hidden: bool) -> Result<()> {
        self.update_each(
            ids,
            "UPDATE assets SET hidden = ?, updated_ms = ? WHERE id = ?",
            i64::from(hidden).into(),
        )
    }

    pub fn set_caption(&mut self, id: AssetId, caption: Option<&str>) -> Result<()> {
        let caption = caption.map(str::trim).filter(|value| !value.is_empty());
        let transaction = self.tx()?;
        transaction.execute(
            "UPDATE assets SET caption = ?, updated_ms = ? WHERE id = ?",
            params![caption, now_ms(), id.get()],
        )?;
        fts_refresh(&transaction, id)?;
        transaction.commit()?;
        Ok(())
    }

    /// Rotates clockwise by a multiple of 90 degrees.
    pub fn rotate(&mut self, ids: &[AssetId], degrees: i32) -> Result<()> {
        if degrees.rem_euclid(90) != 0 {
            return Err(CatalogError::Invalid(
                "rotation must be a multiple of 90".into(),
            ));
        }
        self.update_each(
            ids,
            "UPDATE assets SET rotation = (rotation + ?1 + 360) % 360, updated_ms = ?2 WHERE id = ?3",
            i64::from(degrees.rem_euclid(360)).into(),
        )
    }

    /// Sets the capture time of assets. The read value is kept so it can be restored.
    pub fn set_taken(&mut self, ids: &[AssetId], time: CaptureTime) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction.prepare(
                "UPDATE assets SET taken_local = ?, taken_offset = ?, taken_source = ?, taken_confidence = ?, \
                 updated_ms = ? WHERE id = ?",
            )?;
            let now = now_ms();
            for id in ids {
                statement.execute(params![
                    time.local_ms,
                    time.offset_minutes,
                    DateSource::Manual.code(),
                    Confidence::High.code(),
                    now,
                    id.get()
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Moves capture times by a fixed amount, keeping their differences.
    /// Assets without a date are left alone.
    pub fn shift_taken(&mut self, ids: &[AssetId], delta_ms: i64) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction.prepare(
                "UPDATE assets SET taken_local = taken_local + ?, taken_source = ?, taken_confidence = ?, \
                 updated_ms = ? WHERE id = ? AND taken_local IS NOT NULL",
            )?;
            let now = now_ms();
            for id in ids {
                statement.execute(params![
                    delta_ms,
                    DateSource::Manual.code(),
                    Confidence::High.code(),
                    now,
                    id.get()
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Restores the capture time read from the file.
    pub fn reset_taken(&mut self, ids: &[AssetId]) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction.prepare(
                "UPDATE assets SET taken_local = probed_local, taken_offset = probed_offset, \
                 taken_source = probed_source, taken_confidence = probed_confidence, updated_ms = ? WHERE id = ?",
            )?;
            let now = now_ms();
            for id in ids {
                statement.execute(params![now, id.get()])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Moves assets to or out of the library's trash.
    pub fn set_trashed(&mut self, ids: &[AssetId], trashed: bool) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction.prepare(
                "UPDATE assets SET trashed_ms = ?, updated_ms = ? WHERE id = ? OR live_component = 1 AND id = \
                 (SELECT live_video_id FROM assets WHERE id = ?3)",
            )?;
            let now = now_ms();
            for id in ids {
                statement.execute(params![trashed.then_some(now), now, id.get()])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Records where a file went in the system trash, or clears it on restore.
    pub fn set_trash_path(&mut self, file: FileId, path: Option<&PathBytes>) -> Result<()> {
        self.db.conn.execute(
            "UPDATE files SET trash_path = ?, updated_ms = ? WHERE id = ?",
            params![path.map(PathBytes::as_bytes), now_ms(), file.get()],
        )?;
        Ok(())
    }

    // ----- tags ------------------------------------------------------------

    /// Adds a tag (created if needed) to every asset in `ids`.
    pub fn add_tag(&mut self, ids: &[AssetId], name: &str) -> Result<TagId> {
        let display = tidy_tag(name);
        let norm = normalize_tag(name);
        if norm.is_empty() {
            return Err(CatalogError::Invalid("a tag needs a name".into()));
        }
        let transaction = self.tx()?;
        transaction.execute(
            "INSERT OR IGNORE INTO tags (name, norm, created_ms) VALUES (?, ?, ?)",
            params![display, norm, now_ms()],
        )?;
        let tag: i64 =
            transaction.query_row("SELECT id FROM tags WHERE norm = ?", [&norm], |row| {
                row.get(0)
            })?;
        {
            let mut statement = transaction
                .prepare("INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) VALUES (?, ?)")?;
            for id in ids {
                statement.execute(params![id.get(), tag])?;
            }
        }
        for id in ids {
            fts_refresh(&transaction, *id)?;
        }
        transaction.commit()?;
        Ok(TagId(tag))
    }

    pub fn remove_tag(&mut self, ids: &[AssetId], tag: TagId) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement =
                transaction.prepare("DELETE FROM asset_tags WHERE asset_id = ? AND tag_id = ?")?;
            for id in ids {
                statement.execute(params![id.get(), tag.get()])?;
            }
        }
        for id in ids {
            fts_refresh(&transaction, *id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Renames a tag. If the new name already exists the two tags are merged.
    pub fn rename_tag(&mut self, tag: TagId, name: &str) -> Result<TagId> {
        let display = tidy_tag(name);
        let norm = normalize_tag(name);
        if norm.is_empty() {
            return Err(CatalogError::Invalid("a tag needs a name".into()));
        }
        let existing = self.tag_by_name(name)?;
        match existing {
            Some(other) if other != tag => {
                self.merge_tags(tag, other)?;
                self.db.conn.execute(
                    "UPDATE tags SET name = ? WHERE id = ?",
                    params![display, other.get()],
                )?;
                Ok(other)
            }
            _ => {
                let transaction = self.tx()?;
                transaction.execute(
                    "UPDATE tags SET name = ?, norm = ? WHERE id = ?",
                    params![display, norm, tag.get()],
                )?;
                refresh_tagged(&transaction, tag)?;
                transaction.commit()?;
                Ok(tag)
            }
        }
    }

    /// Moves every use of `from` to `into` and deletes `from`.
    pub fn merge_tags(&mut self, from: TagId, into: TagId) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let transaction = self.tx()?;
        transaction.execute(
            "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) SELECT asset_id, ?2 FROM asset_tags WHERE tag_id = ?1",
            params![from.get(), into.get()],
        )?;
        transaction.execute("DELETE FROM tags WHERE id = ?", [from.get()])?;
        refresh_tagged(&transaction, into)?;
        transaction.commit()?;
        Ok(())
    }

    /// Deletes a tag. Only metadata is affected.
    pub fn delete_tag(&mut self, tag: TagId) -> Result<()> {
        let transaction = self.tx()?;
        let assets: Vec<i64> = {
            let mut statement =
                transaction.prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?")?;
            statement
                .query_map([tag.get()], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        transaction.execute("DELETE FROM tags WHERE id = ?", [tag.get()])?;
        for asset in assets {
            fts_refresh(&transaction, AssetId(asset))?;
        }
        transaction.commit()?;
        Ok(())
    }

    // ----- albums ----------------------------------------------------------

    pub fn create_album(&mut self, name: &str, query: Option<&str>) -> Result<AlbumId> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CatalogError::Invalid("an album needs a name".into()));
        }
        let now = now_ms();
        let sort = if query.is_some() {
            AlbumSort::NewestFirst
        } else {
            AlbumSort::OldestFirst
        };
        self.db.conn.execute(
            "INSERT INTO albums (name, query, sort, created_ms, updated_ms) VALUES (?, ?, ?, ?, ?)",
            params![name, query, sort.code(), now, now],
        )?;
        Ok(AlbumId(self.db.conn.last_insert_rowid()))
    }

    pub fn rename_album(&mut self, album: AlbumId, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CatalogError::Invalid("an album needs a name".into()));
        }
        let transaction = self.tx()?;
        transaction.execute(
            "UPDATE albums SET name = ?, updated_ms = ? WHERE id = ?",
            params![name, now_ms(), album.get()],
        )?;
        refresh_album_members(&transaction, album)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_album_query(&mut self, album: AlbumId, query: &str) -> Result<()> {
        self.db.conn.execute(
            "UPDATE albums SET query = ?, updated_ms = ? WHERE id = ? AND query IS NOT NULL",
            params![query, now_ms(), album.get()],
        )?;
        Ok(())
    }

    pub fn set_album_sort(&mut self, album: AlbumId, sort: AlbumSort) -> Result<()> {
        self.db.conn.execute(
            "UPDATE albums SET sort = ?, updated_ms = ? WHERE id = ?",
            params![sort.code(), now_ms(), album.get()],
        )?;
        Ok(())
    }

    pub fn set_album_cover(&mut self, album: AlbumId, cover: Option<AssetId>) -> Result<()> {
        self.db.conn.execute(
            "UPDATE albums SET cover_id = ?, updated_ms = ? WHERE id = ?",
            params![cover.map(AssetId::get), now_ms(), album.get()],
        )?;
        Ok(())
    }

    /// Deletes an album. The photos in it are not affected.
    pub fn delete_album(&mut self, album: AlbumId) -> Result<()> {
        let transaction = self.tx()?;
        let members: Vec<i64> = {
            let mut statement =
                transaction.prepare("SELECT asset_id FROM album_assets WHERE album_id = ?")?;
            statement
                .query_map([album.get()], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        transaction.execute("DELETE FROM albums WHERE id = ?", [album.get()])?;
        for asset in members {
            fts_refresh(&transaction, AssetId(asset))?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Appends assets to a manual album, skipping ones already in it.
    pub fn add_to_album(&mut self, album: AlbumId, ids: &[AssetId]) -> Result<usize> {
        let transaction = self.tx()?;
        let mut position: f64 = transaction.query_row(
            "SELECT coalesce(max(position), 0) FROM album_assets WHERE album_id = ?",
            [album.get()],
            |row| row.get(0),
        )?;
        let now = now_ms();
        let mut added = 0;
        {
            let mut statement = transaction.prepare(
                "INSERT OR IGNORE INTO album_assets (album_id, asset_id, position, added_ms) VALUES (?, ?, ?, ?)",
            )?;
            for id in ids {
                position += 1.0;
                added += statement.execute(params![album.get(), id.get(), position, now])?;
            }
        }
        transaction.execute(
            "UPDATE albums SET updated_ms = ? WHERE id = ?",
            params![now, album.get()],
        )?;
        for id in ids {
            fts_refresh(&transaction, *id)?;
        }
        transaction.commit()?;
        Ok(added)
    }

    pub fn remove_from_album(&mut self, album: AlbumId, ids: &[AssetId]) -> Result<()> {
        let transaction = self.tx()?;
        {
            let mut statement = transaction
                .prepare("DELETE FROM album_assets WHERE album_id = ? AND asset_id = ?")?;
            for id in ids {
                statement.execute(params![album.get(), id.get()])?;
            }
        }
        transaction.execute(
            "UPDATE albums SET cover_id = NULL WHERE id = ? AND cover_id NOT IN (SELECT asset_id FROM album_assets WHERE album_id = ?1)",
            [album.get()],
        )?;
        for id in ids {
            fts_refresh(&transaction, *id)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Moves assets in a manual album so they sit just before `before`, or at the end.
    pub fn reorder_album(
        &mut self,
        album: AlbumId,
        ids: &[AssetId],
        before: Option<AssetId>,
    ) -> Result<()> {
        let transaction = self.tx()?;
        let mut order: Vec<i64> = {
            let mut statement = transaction.prepare(
                "SELECT asset_id FROM album_assets WHERE album_id = ? ORDER BY position, asset_id",
            )?;
            statement
                .query_map([album.get()], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let moving: Vec<i64> = ids
            .iter()
            .map(|id| id.get())
            .filter(|id| order.contains(id))
            .collect();
        order.retain(|id| !moving.contains(id));
        let index = before
            .and_then(|anchor| order.iter().position(|id| *id == anchor.get()))
            .unwrap_or(order.len());
        for (offset, id) in moving.iter().enumerate() {
            order.insert(index + offset, *id);
        }
        {
            let mut statement = transaction.prepare(
                "UPDATE album_assets SET position = ? WHERE album_id = ? AND asset_id = ?",
            )?;
            for (position, id) in order.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                statement.execute(params![position as f64 + 1.0, album.get(), id])?;
            }
        }
        transaction.execute(
            "UPDATE albums SET sort = ?, updated_ms = ? WHERE id = ?",
            params![AlbumSort::Manual.code(), now_ms(), album.get()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    // ----- review ----------------------------------------------------------

    /// Opens a review item, or refreshes the existing open item for the same subject.
    pub fn open_review(
        &mut self,
        kind: ReviewKind,
        subject: ReviewSubject<'_>,
        detail: &serde_json::Value,
    ) -> Result<ReviewId> {
        let transaction = self.tx()?;
        let id = open_review_in(&transaction, kind, subject, detail)?;
        transaction.commit()?;
        Ok(id)
    }

    pub fn close_review(&mut self, id: ReviewId, state: ReviewState) -> Result<()> {
        self.db.conn.execute(
            "UPDATE review_items SET state = ?, updated_ms = ? WHERE id = ?",
            params![state.code(), now_ms(), id.get()],
        )?;
        Ok(())
    }

    /// Resolves every open item about an operation.
    pub fn close_operation_reviews(
        &mut self,
        operation: OperationUuid,
        state: ReviewState,
    ) -> Result<()> {
        self.db.conn.execute(
            "UPDATE review_items SET state = ?, updated_ms = ? WHERE operation = ? AND state = 0",
            params![state.code(), now_ms(), operation.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    /// Resolves open items of a kind that refer to a path.
    pub fn close_path_reviews(&mut self, kind: ReviewKind, path: &PathBytes) -> Result<()> {
        self.db.conn.execute(
            "UPDATE review_items SET state = 1, updated_ms = ? WHERE kind = ? AND path = ? AND state = 0",
            params![now_ms(), kind.code(), path.as_bytes()],
        )?;
        Ok(())
    }

    // ----- operations ------------------------------------------------------

    /// Inserts or replaces the journal row for an operation.
    pub fn save_operation(&mut self, operation: &Operation) -> Result<()> {
        save_operation_in(&self.db.conn, operation)
    }

    /// Records a new file installed by an operation, and the operation's new state, atomically.
    pub fn catalog_installed(
        &mut self,
        operation: &Operation,
        file: &NewFile<'_>,
        name: &str,
        metadata: &AssetMetadata,
    ) -> Result<Added> {
        let transaction = self.tx()?;
        let added = add_file_in(&transaction, file, name, metadata)?;
        let mut updated = operation.clone();
        updated.asset_id = Some(added.asset);
        updated.file_id = Some(added.file);
        save_operation_in(&transaction, &updated)?;
        transaction.commit()?;
        Ok(added)
    }

    /// Deletes finished journal rows older than `before_ms`.
    pub fn prune_operations(&mut self, before_ms: i64) -> Result<usize> {
        Ok(self.db.conn.execute(
            "DELETE FROM operations WHERE state IN (8, 9, 11) AND updated_ms < ?",
            [before_ms],
        )?)
    }

    // ----- jobs ------------------------------------------------------------

    pub fn start_job(&mut self, kind: &str, title: &str) -> Result<JobId> {
        self.db.conn.execute(
            "INSERT INTO jobs (kind, title, state, started_ms) VALUES (?, ?, ?, ?)",
            params![kind, title, JobState::Running.code(), now_ms()],
        )?;
        Ok(JobId(self.db.conn.last_insert_rowid()))
    }

    pub fn finish_job(&mut self, job: JobId, state: JobState, summary: &str) -> Result<()> {
        self.db.conn.execute(
            "UPDATE jobs SET state = ?, finished_ms = ?, summary = ? WHERE id = ?",
            params![state.code(), now_ms(), summary, job.get()],
        )?;
        self.db.conn.execute(
            "DELETE FROM jobs WHERE id NOT IN (SELECT id FROM jobs ORDER BY started_ms DESC LIMIT 500)",
            [],
        )?;
        Ok(())
    }

    /// Jobs left running by a previous session that ended unexpectedly.
    pub fn abandon_running_jobs(&mut self) -> Result<usize> {
        Ok(self.db.conn.execute(
            "UPDATE jobs SET state = ?, finished_ms = ?, summary = 'Interrupted when MediaNest closed' WHERE state = ?",
            params![JobState::Cancelled.code(), now_ms(), JobState::Running.code()],
        )?)
    }

    // ----- thumbnails, places, similarity ---------------------------------

    pub fn set_thumbnail(
        &mut self,
        asset: AssetId,
        state: ThumbState,
        color: Option<u32>,
        phash: Option<u64>,
    ) -> Result<()> {
        self.db.conn.execute(
            "UPDATE assets SET thumb_state = ?, color = coalesce(?, color), phash = coalesce(?, phash) WHERE id = ?",
            params![state.code(), color.map(i64::from), phash.map(bits_i64), asset.get()],
        )?;
        Ok(())
    }

    /// Queues every thumbnail, or only failed ones, for regeneration.
    pub fn reset_thumbnails(&mut self, failed_only: bool) -> Result<usize> {
        let sql = if failed_only {
            "UPDATE assets SET thumb_state = 0 WHERE thumb_state = 2"
        } else {
            "UPDATE assets SET thumb_state = 0"
        };
        Ok(self.db.conn.execute(sql, [])?)
    }

    pub fn set_place(&mut self, asset: AssetId, place: Option<&Place>) -> Result<()> {
        let transaction = self.tx()?;
        transaction.execute(
            "UPDATE assets SET place_city = ?, place_region = ?, place_country = ?, place_cc = ? WHERE id = ?",
            params![
                place.map(|p| p.city.as_str()),
                place.and_then(|p| p.region.as_deref()),
                place.map(|p| p.country.as_str()),
                place.map(|p| p.country_code.as_str()),
                asset.get()
            ],
        )?;
        fts_refresh(&transaction, asset)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn dismiss_similar(&mut self, a: AssetId, b: AssetId) -> Result<()> {
        let (low, high) = if a <= b { (a, b) } else { (b, a) };
        self.db.conn.execute(
            "INSERT OR IGNORE INTO similar_dismissed (a, b) VALUES (?, ?)",
            params![low.get(), high.get()],
        )?;
        Ok(())
    }

    /// Rebuilds the search index for every asset.
    pub fn rebuild_search_index(&mut self) -> Result<()> {
        let transaction = self.tx()?;
        transaction.execute("DELETE FROM assets_fts", [])?;
        let ids: Vec<i64> = {
            let mut statement = transaction.prepare("SELECT id FROM assets")?;
            statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in ids {
            fts_refresh(&transaction, AssetId(id))?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Imports user metadata for an asset found by content hash. Used by the
    /// metadata importer; files are never touched.
    pub fn apply_user_metadata(&mut self, asset: AssetId, edits: &UserMetadata) -> Result<()> {
        let transaction = self.tx()?;
        let now = now_ms();
        transaction.execute(
            "UPDATE assets SET favorite = ?, rating = ?, hidden = ?, caption = ?, rotation = ?, updated_ms = ? WHERE id = ?",
            params![
                edits.favorite,
                i64::from(edits.rating.min(5)),
                edits.hidden,
                edits.caption.as_deref(),
                i64::from(edits.rotation % 360 / 90 * 90),
                now,
                asset.get()
            ],
        )?;
        if let Some(time) = edits.manual_taken {
            transaction.execute(
                "UPDATE assets SET taken_local = ?, taken_offset = ?, taken_source = ?, taken_confidence = ? WHERE id = ?",
                params![
                    time.local_ms,
                    time.offset_minutes,
                    DateSource::Manual.code(),
                    Confidence::High.code(),
                    asset.get()
                ],
            )?;
        }
        for name in &edits.tags {
            let norm = normalize_tag(name);
            if norm.is_empty() {
                continue;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO tags (name, norm, created_ms) VALUES (?, ?, ?)",
                params![tidy_tag(name), norm, now],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) SELECT ?, id FROM tags WHERE norm = ?",
                params![asset.get(), norm],
            )?;
        }
        for album in &edits.albums {
            let id: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM albums WHERE name = ? COLLATE NOCASE AND query IS NULL",
                    [album],
                    |row| row.get(0),
                )
                .optional()?;
            let id = match id {
                Some(id) => id,
                None => {
                    transaction.execute(
                        "INSERT INTO albums (name, sort, created_ms, updated_ms) VALUES (?, 0, ?, ?)",
                        params![album, now, now],
                    )?;
                    transaction.last_insert_rowid()
                }
            };
            transaction.execute(
                "INSERT OR IGNORE INTO album_assets (album_id, asset_id, position, added_ms) \
                 VALUES (?1, ?2, (SELECT coalesce(max(position), 0) + 1 FROM album_assets WHERE album_id = ?1), ?3)",
                params![id, asset.get(), now],
            )?;
        }
        fts_refresh(&transaction, asset)?;
        transaction.commit()?;
        Ok(())
    }
}

/// User-authored metadata carried by metadata exports.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserMetadata {
    pub favorite: bool,
    pub rating: u8,
    pub hidden: bool,
    pub caption: Option<String>,
    pub rotation: u16,
    pub manual_taken: Option<CaptureTime>,
    pub tags: Vec<String>,
    pub albums: Vec<String>,
}

/// What a review item is about.
#[derive(Clone, Copy, Debug)]
pub enum ReviewSubject<'a> {
    Asset(AssetId),
    File(AssetId, FileId),
    Operation(OperationUuid, Option<&'a PathBytes>),
    Path(&'a PathBytes),
}

fn open_review_in(
    transaction: &Transaction<'_>,
    kind: ReviewKind,
    subject: ReviewSubject<'_>,
    detail: &serde_json::Value,
) -> Result<ReviewId> {
    type Keys = (Option<i64>, Option<i64>, Option<Vec<u8>>, Option<Vec<u8>>);
    let (asset, file, operation, path): Keys = match subject {
        ReviewSubject::Asset(asset) => (Some(asset.get()), None, None, None),
        ReviewSubject::File(asset, file) => (Some(asset.get()), Some(file.get()), None, None),
        ReviewSubject::Operation(operation, path) => (
            None,
            None,
            Some(operation.as_bytes().to_vec()),
            path.map(|p| p.as_bytes().to_vec()),
        ),
        ReviewSubject::Path(path) => (None, None, None, Some(path.as_bytes().to_vec())),
    };
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT id FROM review_items WHERE state = 0 AND kind = ?1 AND \
             ((?3 IS NOT NULL AND file_id = ?3) OR (?3 IS NULL AND ?2 IS NOT NULL AND asset_id = ?2 AND file_id IS NULL) \
              OR (?4 IS NOT NULL AND operation = ?4) \
              OR (?2 IS NULL AND ?3 IS NULL AND ?4 IS NULL AND path = ?5))",
            params![kind.code(), asset, file, operation, path],
            |row| row.get(0),
        )
        .optional()?;
    let now = now_ms();
    let detail = detail.to_string();
    if let Some(id) = existing {
        transaction.execute(
            "UPDATE review_items SET detail = ?, updated_ms = ? WHERE id = ?",
            params![detail, now, id],
        )?;
        return Ok(ReviewId(id));
    }
    transaction.execute(
        "INSERT INTO review_items (kind, asset_id, file_id, operation, path, detail, state, created_ms, updated_ms) \
         VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
        params![kind.code(), asset, file, operation, path, detail, now, now],
    )?;
    Ok(ReviewId(transaction.last_insert_rowid()))
}

fn resolve_for_file(
    transaction: &Transaction<'_>,
    file: FileId,
    kinds: &[ReviewKind],
) -> Result<()> {
    let mut statement = transaction.prepare_cached(
        "UPDATE review_items SET state = 1, updated_ms = ? WHERE file_id = ? AND kind = ? AND state = 0",
    )?;
    let now = now_ms();
    for kind in kinds {
        statement.execute(params![now, file.get(), kind.code()])?;
    }
    Ok(())
}

fn set_file_status_in(
    transaction: &Transaction<'_>,
    file: FileId,
    status: FileStatus,
) -> Result<()> {
    transaction.execute(
        "UPDATE files SET status = ?, updated_ms = ? WHERE id = ?",
        params![status.code(), now_ms(), file.get()],
    )?;
    let asset = asset_of_file(transaction, file)?;
    match status {
        FileStatus::Missing => {
            let (source, relpath): (Vec<u8>, Vec<u8>) = transaction.query_row(
                "SELECT s.path, f.relpath FROM files f JOIN sources s ON s.id = f.source_id WHERE f.id = ?",
                [file.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let path = PathBytes::from_vec(source)
                .as_path()
                .join(PathBytes::from_vec(relpath).as_path());
            open_review_in(
                transaction,
                ReviewKind::MissingFile,
                ReviewSubject::File(asset, file),
                &serde_json::json!({ "path": path.to_string_lossy() }),
            )?;
            resolve_for_file(transaction, file, &[ReviewKind::ChangedFile])?;
        }
        FileStatus::Changed => {
            open_review_in(
                transaction,
                ReviewKind::ChangedFile,
                ReviewSubject::File(asset, file),
                &serde_json::json!({}),
            )?;
            resolve_for_file(transaction, file, &[ReviewKind::MissingFile])?;
        }
        FileStatus::Verified | FileStatus::Unverified => {
            resolve_for_file(
                transaction,
                file,
                &[ReviewKind::MissingFile, ReviewKind::ChangedFile],
            )?;
        }
    }
    refresh_file_state(transaction, asset)
}

fn asset_of_file(connection: &Connection, file: FileId) -> Result<AssetId> {
    connection
        .query_row(
            "SELECT asset_id FROM files WHERE id = ?",
            [file.get()],
            |row| row.get(0).map(AssetId),
        )
        .optional()?
        .ok_or(CatalogError::NotFound("file"))
}

fn save_operation_in(connection: &Connection, operation: &Operation) -> Result<()> {
    let stamp = operation.source_stamp;
    connection.execute(
        "INSERT INTO operations (uuid, kind, state, source_path, temp_path, dest_relpath, aside_path, \
         source_size, source_mtime_ns, source_ctime_ns, source_btime_ns, source_device, source_inode, \
         hash, asset_id, file_id, batch, error, created_ms, updated_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20) \
         ON CONFLICT (uuid) DO UPDATE SET state = ?3, source_path = ?4, temp_path = ?5, dest_relpath = ?6, \
         aside_path = ?7, source_size = ?8, source_mtime_ns = ?9, source_ctime_ns = ?10, source_btime_ns = ?11, \
         source_device = ?12, source_inode = ?13, hash = ?14, asset_id = ?15, file_id = ?16, batch = ?17, \
         error = ?18, updated_ms = ?20",
        params![
            operation.uuid.as_bytes().as_slice(),
            operation.kind.code(),
            operation.state.code(),
            operation.source_path.as_ref().map(PathBytes::as_bytes),
            operation.temp_path.as_ref().map(PathBytes::as_bytes),
            operation.dest_relpath.as_ref().map(PathBytes::as_bytes),
            operation.aside_path.as_ref().map(PathBytes::as_bytes),
            stamp.map(|s| i64_of(s.size)),
            stamp.map(|s| s.mtime_ns),
            stamp.map(|s| s.ctime_ns),
            stamp.and_then(|s| s.btime_ns),
            stamp.map(|s| bits_i64(s.device)),
            stamp.map(|s| bits_i64(s.inode)),
            operation.hash.as_ref().map(<[u8; 32]>::as_slice),
            operation.asset_id.map(AssetId::get),
            operation.file_id.map(FileId::get),
            operation.batch.as_deref(),
            operation.error.as_deref(),
            operation.created_ms,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn insert_asset(
    transaction: &Transaction<'_>,
    metadata: &AssetMetadata,
    name: &str,
    hash: Option<&[u8; 32]>,
    size: u64,
) -> Result<(AssetId, AssetUuid)> {
    let uuid = AssetUuid::new();
    let now = now_ms();
    let taken = metadata.taken;
    let location = metadata.location;
    let place = metadata.place.as_ref();
    transaction.execute(
        "INSERT INTO assets (uuid, kind, format, name, hash, size, width, height, duration_ms, orientation, \
         taken_local, taken_offset, taken_source, taken_confidence, \
         probed_local, probed_offset, probed_source, probed_confidence, \
         camera_make, camera_model, lens, focal_mm, focal35_mm, aperture, exposure_s, iso, \
         latitude, longitude, altitude, place_city, place_region, place_country, place_cc, \
         screenshot, content_id, motion_offset, file_state, added_ms, updated_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?11, ?12, ?13, ?14, \
         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, 1, ?33, ?33)",
        params![
            uuid.as_bytes().as_slice(),
            metadata.kind.code(),
            metadata.format.code(),
            name,
            hash.map(<[u8; 32]>::as_slice),
            i64_of(size),
            metadata.width,
            metadata.height,
            metadata.duration_ms.map(i64_of),
            i64::from(metadata.orientation.clamp(1, 8)),
            taken.local_ms,
            taken.offset_minutes,
            taken.source.code(),
            taken.confidence.code(),
            metadata.camera_make.as_deref(),
            metadata.camera_model.as_deref(),
            metadata.lens.as_deref(),
            metadata.focal_length_mm.map(f64::from),
            metadata.focal_length_35mm.map(f64::from),
            metadata.aperture.map(f64::from),
            metadata.exposure_seconds.map(f64::from),
            metadata.iso,
            location.map(|l| l.latitude),
            location.map(|l| l.longitude),
            location.and_then(|l| l.altitude),
            place.map(|p| p.city.as_str()),
            place.and_then(|p| p.region.as_deref()),
            place.map(|p| p.country.as_str()),
            place.map(|p| p.country_code.as_str()),
            metadata.is_screenshot,
            metadata.content_identifier.as_deref(),
            metadata.motion_video_offset.map(i64_of),
            now,
        ],
    )?;
    Ok((AssetId(transaction.last_insert_rowid()), uuid))
}

/// Replaces probed metadata. A manual date and every user edit are kept.
fn update_probed(
    transaction: &Transaction<'_>,
    asset: AssetId,
    metadata: &AssetMetadata,
) -> Result<()> {
    let taken = metadata.taken;
    let location = metadata.location;
    let place = metadata.place.as_ref();
    transaction.execute(
        "UPDATE assets SET kind = ?2, format = ?3, width = ?4, height = ?5, duration_ms = ?6, orientation = ?7, \
         probed_local = ?8, probed_offset = ?9, probed_source = ?10, probed_confidence = ?11, \
         taken_local = CASE WHEN taken_source = 6 THEN taken_local ELSE ?8 END, \
         taken_offset = CASE WHEN taken_source = 6 THEN taken_offset ELSE ?9 END, \
         taken_confidence = CASE WHEN taken_source = 6 THEN taken_confidence ELSE ?11 END, \
         taken_source = CASE WHEN taken_source = 6 THEN taken_source ELSE ?10 END, \
         camera_make = ?12, camera_model = ?13, lens = ?14, focal_mm = ?15, focal35_mm = ?16, aperture = ?17, \
         exposure_s = ?18, iso = ?19, latitude = ?20, longitude = ?21, altitude = ?22, \
         place_city = coalesce(?23, place_city), place_region = coalesce(?24, place_region), \
         place_country = coalesce(?25, place_country), place_cc = coalesce(?26, place_cc), \
         screenshot = ?27, content_id = ?28, motion_offset = ?29, updated_ms = ?30 WHERE id = ?1",
        params![
            asset.get(),
            metadata.kind.code(),
            metadata.format.code(),
            metadata.width,
            metadata.height,
            metadata.duration_ms.map(i64_of),
            i64::from(metadata.orientation.clamp(1, 8)),
            taken.local_ms,
            taken.offset_minutes,
            taken.source.code(),
            taken.confidence.code(),
            metadata.camera_make.as_deref(),
            metadata.camera_model.as_deref(),
            metadata.lens.as_deref(),
            metadata.focal_length_mm.map(f64::from),
            metadata.focal_length_35mm.map(f64::from),
            metadata.aperture.map(f64::from),
            metadata.exposure_seconds.map(f64::from),
            metadata.iso,
            location.map(|l| l.latitude),
            location.map(|l| l.longitude),
            location.and_then(|l| l.altitude),
            place.map(|p| p.city.as_str()),
            place.and_then(|p| p.region.as_deref()),
            place.map(|p| p.country.as_str()),
            place.map(|p| p.country_code.as_str()),
            metadata.is_screenshot,
            metadata.content_identifier.as_deref(),
            metadata.motion_video_offset.map(i64_of),
            now_ms(),
        ],
    )?;
    fts_refresh(transaction, asset)
}

fn add_file_in(
    transaction: &Transaction<'_>,
    file: &NewFile<'_>,
    name: &str,
    metadata: &AssetMetadata,
) -> Result<Added> {
    let existing = match &file.hash {
        Some(hash) => transaction
            .query_row(
                "SELECT id FROM assets WHERE hash = ?",
                [hash.as_slice()],
                |row| row.get(0).map(AssetId),
            )
            .optional()?,
        None => None,
    };
    let (asset, new_asset) = match existing {
        Some(asset) => (asset, false),
        None => (
            insert_asset(
                transaction,
                metadata,
                name,
                file.hash.as_ref(),
                file.stamp.size,
            )?
            .0,
            true,
        ),
    };
    let now = now_ms();
    transaction.execute(
        "INSERT INTO files (asset_id, source_id, relpath, size, mtime_ns, ctime_ns, btime_ns, device, inode, \
         status, hash, checked_ms, added_ms, updated_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            asset.get(),
            file.source.get(),
            file.relpath.as_bytes(),
            i64_of(file.stamp.size),
            file.stamp.mtime_ns,
            file.stamp.ctime_ns,
            file.stamp.btime_ns,
            bits_i64(file.stamp.device),
            bits_i64(file.stamp.inode),
            file.status.code(),
            file.hash.as_ref().map(<[u8; 32]>::as_slice),
            file.hash.map(|_| now),
            now,
            now
        ],
    )?;
    let file_id = FileId(transaction.last_insert_rowid());
    if new_asset {
        pair_live(transaction, asset)?;
    }
    refresh_file_state(transaction, asset)?;
    fts_refresh(transaction, asset)?;
    Ok(Added {
        asset,
        file: file_id,
        new_asset,
    })
}

/// Links a Live Photo still with its short video, by Apple content identifier
/// or by sharing a folder and base name.
fn pair_live(transaction: &Transaction<'_>, asset: AssetId) -> Result<()> {
    let (kind, content_id, duration): (i64, Option<String>, Option<i64>) = transaction.query_row(
        "SELECT kind, content_id, duration_ms FROM assets WHERE id = ?",
        [asset.get()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let is_photo = kind == MediaKind::Photo.code();
    let mut partner: Option<i64> = None;
    if let Some(content_id) = &content_id {
        partner = transaction
            .query_row(
                "SELECT id FROM assets WHERE content_id = ? AND kind != ? AND id != ? LIMIT 1",
                params![content_id, kind, asset.get()],
                |row| row.get(0),
            )
            .optional()?;
    }
    if partner.is_none() && (is_photo || duration.is_some_and(|d| d <= 4_000)) {
        // Same folder and file stem, e.g. IMG_1234.HEIC and IMG_1234.MOV.
        let file: Option<(i64, Vec<u8>)> = transaction
            .query_row(
                "SELECT source_id, relpath FROM files WHERE asset_id = ? LIMIT 1",
                [asset.get()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((source, relpath)) = file {
            if let Some(stem_end) = relpath.iter().rposition(|byte| *byte == b'.') {
                let stem = &relpath[..=stem_end];
                let mut statement = transaction.prepare_cached(
                    "SELECT a.id, a.duration_ms, f.relpath FROM files f JOIN assets a ON a.id = f.asset_id \
                     WHERE f.source_id = ? AND substr(f.relpath, 1, ?) = ? AND a.kind != ? AND a.id != ?",
                )?;
                let candidates: Vec<(i64, Option<i64>, Vec<u8>)> = statement
                    .query_map(
                        params![source, i64_of(stem.len() as u64), stem, kind, asset.get()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?
                    .collect::<rusqlite::Result<_>>()?;
                partner = candidates
                    .into_iter()
                    .filter(|(_, _, path)| {
                        !path[stem.len()..].contains(&b'/') && !path[stem.len()..].contains(&b'.')
                    })
                    .find(|(_, other_duration, _)| {
                        !is_photo || other_duration.is_none_or(|d| d <= 4_000)
                    })
                    .map(|(id, _, _)| id);
            }
        }
    }
    if let Some(partner) = partner {
        let (photo, video) = if is_photo {
            (asset.get(), partner)
        } else {
            (partner, asset.get())
        };
        transaction.execute(
            "UPDATE assets SET live_video_id = ? WHERE id = ? AND live_video_id IS NULL",
            params![video, photo],
        )?;
        transaction.execute("UPDATE assets SET live_component = 1 WHERE id = ?", [video])?;
    }
    Ok(())
}

/// Recomputes the summary file state of an asset from its files.
fn refresh_file_state(connection: &Connection, asset: AssetId) -> Result<()> {
    let statuses: Vec<i64> = {
        let mut statement =
            connection.prepare_cached("SELECT status FROM files WHERE asset_id = ?")?;
        statement
            .query_map([asset.get()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let present: Vec<i64> = statuses
        .iter()
        .copied()
        .filter(|status| *status != FileStatus::Missing.code())
        .collect();
    let state = if statuses.is_empty() || present.is_empty() {
        FileStatus::Missing
    } else if present.contains(&FileStatus::Changed.code()) {
        FileStatus::Changed
    } else if present.contains(&FileStatus::Verified.code()) {
        FileStatus::Verified
    } else {
        FileStatus::Unverified
    };
    connection.execute(
        "UPDATE assets SET file_state = ? WHERE id = ?",
        params![state.code(), asset.get()],
    )?;
    Ok(())
}

/// Deletes an asset with no files left. Returns its uuid if it was deleted.
fn delete_if_orphan(transaction: &Transaction<'_>, asset: AssetId) -> Result<Option<AssetUuid>> {
    let remaining: i64 = transaction.query_row(
        "SELECT count(*) FROM files WHERE asset_id = ?",
        [asset.get()],
        |row| row.get(0),
    )?;
    if remaining > 0 {
        return Ok(None);
    }
    let uuid: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT uuid FROM assets WHERE id = ?",
            [asset.get()],
            |row| row.get(0),
        )
        .optional()?;
    transaction.execute("DELETE FROM assets_fts WHERE rowid = ?", [asset.get()])?;
    transaction.execute("DELETE FROM assets WHERE id = ?", [asset.get()])?;
    Ok(uuid.as_deref().and_then(AssetUuid::from_slice))
}

/// Folds `other` into `keep`: files, tags, albums and user edits move over.
fn merge_into(transaction: &Transaction<'_>, keep: AssetId, other: AssetId) -> Result<()> {
    let (k, o) = (keep.get(), other.get());
    transaction.execute(
        "DELETE FROM files WHERE asset_id = ?2 AND EXISTS \
         (SELECT 1 FROM files k WHERE k.asset_id = ?1 AND k.source_id = files.source_id AND k.relpath = files.relpath)",
        params![k, o],
    )?;
    transaction.execute(
        "UPDATE files SET asset_id = ? WHERE asset_id = ?",
        params![k, o],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) SELECT ?1, tag_id FROM asset_tags WHERE asset_id = ?2",
        params![k, o],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO album_assets (album_id, asset_id, position, added_ms) \
         SELECT album_id, ?1, position, added_ms FROM album_assets WHERE asset_id = ?2",
        params![k, o],
    )?;
    transaction.execute(
        "UPDATE assets SET \
         favorite = max(favorite, (SELECT favorite FROM assets WHERE id = ?2)), \
         rating = max(rating, (SELECT rating FROM assets WHERE id = ?2)), \
         hidden = min(hidden, (SELECT hidden FROM assets WHERE id = ?2)), \
         caption = coalesce(caption, (SELECT caption FROM assets WHERE id = ?2)), \
         added_ms = min(added_ms, (SELECT added_ms FROM assets WHERE id = ?2)), \
         updated_ms = ?3 WHERE id = ?1",
        params![k, o, now_ms()],
    )?;
    let other_manual: Option<(Option<i64>, Option<i64>)> = transaction
        .query_row(
            "SELECT taken_local, taken_offset FROM assets WHERE id = ? AND taken_source = 6",
            [o],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((local, offset)) = other_manual {
        transaction.execute(
            "UPDATE assets SET taken_local = ?, taken_offset = ?, taken_source = 6, taken_confidence = 3 \
             WHERE id = ? AND taken_source != 6",
            params![local, offset, k],
        )?;
    }
    transaction.execute(
        "UPDATE assets SET live_video_id = (SELECT live_video_id FROM assets WHERE id = ?2) \
         WHERE id = ?1 AND live_video_id IS NULL",
        params![k, o],
    )?;
    transaction.execute(
        "UPDATE review_items SET asset_id = ?1 WHERE asset_id = ?2 AND state = 0",
        params![k, o],
    )?;
    transaction.execute(
        "UPDATE operations SET asset_id = ?1 WHERE asset_id = ?2",
        params![k, o],
    )?;
    transaction.execute(
        "UPDATE albums SET cover_id = ?1 WHERE cover_id = ?2",
        params![k, o],
    )?;
    transaction.execute("DELETE FROM assets_fts WHERE rowid = ?", [o])?;
    transaction.execute("DELETE FROM assets WHERE id = ?", [o])?;
    Ok(())
}

fn refresh_tagged(transaction: &Transaction<'_>, tag: TagId) -> Result<()> {
    let assets: Vec<i64> = {
        let mut statement =
            transaction.prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?")?;
        statement
            .query_map([tag.get()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for asset in assets {
        fts_refresh(transaction, AssetId(asset))?;
    }
    Ok(())
}

fn refresh_album_members(transaction: &Transaction<'_>, album: AlbumId) -> Result<()> {
    let assets: Vec<i64> = {
        let mut statement =
            transaction.prepare("SELECT asset_id FROM album_assets WHERE album_id = ?")?;
        statement
            .query_map([album.get()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for asset in assets {
        fts_refresh(transaction, AssetId(asset))?;
    }
    Ok(())
}

/// Rewrites the full-text index row of one asset.
pub(crate) fn fts_refresh(connection: &Connection, asset: AssetId) -> Result<()> {
    connection
        .prepare_cached("DELETE FROM assets_fts WHERE rowid = ?")?
        .execute([asset.get()])?;
    connection
        .prepare_cached(
            "INSERT INTO assets_fts (rowid, name, tags, albums, camera, place, caption) \
             SELECT a.id, \
               a.name || ' ' || coalesce((SELECT group_concat(CAST(f.relpath AS TEXT), ' ') FROM files f WHERE f.asset_id = a.id), ''), \
               coalesce((SELECT group_concat(t.name, ' ') FROM asset_tags at JOIN tags t ON t.id = at.tag_id WHERE at.asset_id = a.id), ''), \
               coalesce((SELECT group_concat(al.name, ' ') FROM album_assets aa JOIN albums al ON al.id = aa.album_id WHERE aa.asset_id = a.id), ''), \
               trim(coalesce(a.camera_make, '') || ' ' || coalesce(a.camera_model, '') || ' ' || coalesce(a.lens, '')), \
               trim(coalesce(a.place_city, '') || ' ' || coalesce(a.place_region, '') || ' ' || coalesce(a.place_country, '')), \
               coalesce(a.caption, '') \
             FROM assets a WHERE a.id = ?",
        )?
        .execute([asset.get()])?;
    Ok(())
}
