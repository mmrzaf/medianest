//! Schema definition and migrations.

use rusqlite::{Connection, params};

use crate::{CatalogError, Result};

/// `MNST` in ASCII. Identifies a MediaNest catalog file.
pub const APPLICATION_ID: i32 = 0x4D4E_5354;

/// Ordered migrations. Index `n` upgrades `user_version` from `n` to `n + 1`.
const MIGRATIONS: &[&str] = &[V1];

/// Current schema version written by this build.
#[must_use]
pub fn latest_version() -> i32 {
    i32::try_from(MIGRATIONS.len()).unwrap_or(i32::MAX)
}

const V1: &str = r#"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE sources (
    id         INTEGER PRIMARY KEY,
    kind       INTEGER NOT NULL CHECK (kind IN (1, 2)),
    path       BLOB NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    watch      INTEGER NOT NULL DEFAULT 1 CHECK (watch IN (0, 1)),
    status     INTEGER NOT NULL DEFAULT 1 CHECK (status IN (1, 2)),
    added_ms   INTEGER NOT NULL,
    scanned_ms INTEGER
) STRICT;

CREATE TABLE assets (
    id                 INTEGER PRIMARY KEY,
    uuid               BLOB NOT NULL UNIQUE CHECK (length(uuid) = 16),
    kind               INTEGER NOT NULL CHECK (kind IN (1, 2)),
    format             TEXT NOT NULL,
    name               TEXT NOT NULL,
    hash               BLOB CHECK (hash IS NULL OR length(hash) = 32),
    size               INTEGER NOT NULL CHECK (size >= 0),
    width              INTEGER,
    height             INTEGER,
    duration_ms        INTEGER,
    orientation        INTEGER NOT NULL DEFAULT 1 CHECK (orientation BETWEEN 1 AND 8),
    rotation           INTEGER NOT NULL DEFAULT 0 CHECK (rotation IN (0, 90, 180, 270)),
    taken_local        INTEGER,
    taken_offset       INTEGER,
    taken_source       INTEGER NOT NULL DEFAULT 0,
    taken_confidence   INTEGER NOT NULL DEFAULT 0,
    probed_local       INTEGER,
    probed_offset      INTEGER,
    probed_source      INTEGER NOT NULL DEFAULT 0,
    probed_confidence  INTEGER NOT NULL DEFAULT 0,
    camera_make        TEXT,
    camera_model       TEXT,
    lens               TEXT,
    focal_mm           REAL,
    focal35_mm         REAL,
    aperture           REAL,
    exposure_s         REAL,
    iso                INTEGER,
    latitude           REAL,
    longitude          REAL,
    altitude           REAL,
    place_city         TEXT,
    place_region       TEXT,
    place_country      TEXT,
    place_cc           TEXT,
    favorite           INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    rating             INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
    hidden             INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1)),
    caption            TEXT,
    trashed_ms         INTEGER,
    screenshot         INTEGER NOT NULL DEFAULT 0 CHECK (screenshot IN (0, 1)),
    content_id         TEXT,
    motion_offset      INTEGER,
    live_video_id      INTEGER REFERENCES assets (id) ON DELETE SET NULL,
    live_component     INTEGER NOT NULL DEFAULT 0 CHECK (live_component IN (0, 1)),
    color              INTEGER,
    phash              INTEGER,
    thumb_state        INTEGER NOT NULL DEFAULT 0 CHECK (thumb_state IN (0, 1, 2)),
    file_state         INTEGER NOT NULL DEFAULT 1 CHECK (file_state BETWEEN 1 AND 4),
    added_ms           INTEGER NOT NULL,
    updated_ms         INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX assets_hash ON assets (hash) WHERE hash IS NOT NULL;
CREATE INDEX assets_timeline ON assets (taken_local DESC, id DESC);
CREATE INDEX assets_added ON assets (added_ms DESC, id DESC);
CREATE INDEX assets_thumb_pending ON assets (thumb_state) WHERE thumb_state = 0;
CREATE INDEX assets_content_id ON assets (content_id) WHERE content_id IS NOT NULL;
CREATE INDEX assets_trashed ON assets (trashed_ms) WHERE trashed_ms IS NOT NULL;

CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    asset_id    INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    source_id   INTEGER NOT NULL REFERENCES sources (id) ON DELETE CASCADE,
    relpath     BLOB NOT NULL,
    size        INTEGER NOT NULL CHECK (size >= 0),
    mtime_ns    INTEGER NOT NULL,
    ctime_ns    INTEGER NOT NULL,
    btime_ns    INTEGER,
    device      INTEGER NOT NULL,
    inode       INTEGER NOT NULL,
    status      INTEGER NOT NULL CHECK (status BETWEEN 1 AND 4),
    hash        BLOB CHECK (hash IS NULL OR length(hash) = 32),
    checked_ms  INTEGER,
    trash_path  BLOB,
    added_ms    INTEGER NOT NULL,
    updated_ms  INTEGER NOT NULL,
    UNIQUE (source_id, relpath)
) STRICT;

CREATE INDEX files_asset ON files (asset_id);
CREATE INDEX files_status ON files (status);
CREATE INDEX files_size ON files (size);

CREATE TABLE exclusions (
    source_id INTEGER NOT NULL REFERENCES sources (id) ON DELETE CASCADE,
    relpath   BLOB NOT NULL,
    added_ms  INTEGER NOT NULL,
    PRIMARY KEY (source_id, relpath)
) STRICT, WITHOUT ROWID;

CREATE TABLE tags (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    norm       TEXT NOT NULL UNIQUE,
    created_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE asset_tags (
    asset_id INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (asset_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX asset_tags_tag ON asset_tags (tag_id, asset_id);

CREATE TABLE albums (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    query      TEXT,
    cover_id   INTEGER REFERENCES assets (id) ON DELETE SET NULL,
    sort       INTEGER NOT NULL DEFAULT 0 CHECK (sort IN (0, 1, 2)),
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE album_assets (
    album_id INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    asset_id INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    position REAL NOT NULL,
    added_ms INTEGER NOT NULL,
    PRIMARY KEY (album_id, asset_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX album_assets_asset ON album_assets (asset_id);

CREATE TABLE review_items (
    id           INTEGER PRIMARY KEY,
    kind         INTEGER NOT NULL,
    asset_id     INTEGER REFERENCES assets (id) ON DELETE CASCADE,
    file_id      INTEGER REFERENCES files (id) ON DELETE CASCADE,
    operation    BLOB,
    path         BLOB,
    detail       TEXT NOT NULL DEFAULT '{}',
    state        INTEGER NOT NULL DEFAULT 0 CHECK (state IN (0, 1, 2)),
    created_ms   INTEGER NOT NULL,
    updated_ms   INTEGER NOT NULL
) STRICT;

CREATE INDEX review_open ON review_items (state, kind);
CREATE UNIQUE INDEX review_open_file ON review_items (kind, file_id)
    WHERE state = 0 AND file_id IS NOT NULL;
CREATE UNIQUE INDEX review_open_asset ON review_items (kind, asset_id)
    WHERE state = 0 AND asset_id IS NOT NULL AND file_id IS NULL;
CREATE UNIQUE INDEX review_open_operation ON review_items (kind, operation)
    WHERE state = 0 AND operation IS NOT NULL;
CREATE UNIQUE INDEX review_open_path ON review_items (kind, path)
    WHERE state = 0 AND path IS NOT NULL AND file_id IS NULL AND asset_id IS NULL AND operation IS NULL;

CREATE TABLE operations (
    uuid            BLOB PRIMARY KEY NOT NULL CHECK (length(uuid) = 16),
    kind            INTEGER NOT NULL,
    state           INTEGER NOT NULL,
    source_path     BLOB,
    temp_path       BLOB,
    dest_relpath    BLOB,
    aside_path      BLOB,
    source_size     INTEGER,
    source_mtime_ns INTEGER,
    source_ctime_ns INTEGER,
    source_btime_ns INTEGER,
    source_device   INTEGER,
    source_inode    INTEGER,
    hash            BLOB CHECK (hash IS NULL OR length(hash) = 32),
    asset_id        INTEGER REFERENCES assets (id) ON DELETE SET NULL,
    file_id         INTEGER REFERENCES files (id) ON DELETE SET NULL,
    batch           TEXT,
    error           TEXT,
    created_ms      INTEGER NOT NULL,
    updated_ms      INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX operations_state ON operations (state);

CREATE TABLE jobs (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    title       TEXT NOT NULL,
    state       INTEGER NOT NULL,
    started_ms  INTEGER NOT NULL,
    finished_ms INTEGER,
    summary     TEXT NOT NULL DEFAULT ''
) STRICT;

CREATE TABLE similar_dismissed (
    a INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    b INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    PRIMARY KEY (a, b)
) STRICT, WITHOUT ROWID;

CREATE VIRTUAL TABLE assets_fts USING fts5 (
    name, tags, albums, camera, place, caption,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);
"#;

/// Applies pending migrations. Fails for catalogs written by a newer build.
pub fn migrate(connection: &mut Connection) -> Result<()> {
    let application_id: i32 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == 0 {
        let tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
            [],
            |row| row.get(0),
        )?;
        if tables > 0 && application_id != APPLICATION_ID {
            return Err(CatalogError::NotACatalog);
        }
    } else if application_id != APPLICATION_ID {
        return Err(CatalogError::NotACatalog);
    }
    if version > latest_version() {
        return Err(CatalogError::TooNew {
            found: version,
            supported: latest_version(),
        });
    }
    for (index, sql) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(version).unwrap_or(0))
    {
        let target = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", target)?;
        transaction.commit()?;
        tracing::info!(version = target, "catalog schema migrated");
    }
    Ok(())
}

/// Reads the schema version without changing anything.
pub fn version(connection: &Connection) -> Result<i32> {
    Ok(connection.query_row("PRAGMA user_version", params![], |row| row.get(0))?)
}
