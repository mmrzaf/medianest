#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use medianest_catalog::{
    Db, NewFile, ReviewSubject, Scope, Sort, UserMetadata, View, Writer, flags,
};
use medianest_core::query::Query;
use medianest_core::{
    AssetId, AssetMetadata, CaptureTime, Confidence, DateSource, FileStamp, FileStatus, Format,
    GeoPoint, LibraryUuid, MediaKind, PathBytes, Place, ReviewKind, SourceId, local_ms_from_naive,
};

fn stamp(size: u64, inode: u64) -> FileStamp {
    FileStamp {
        size,
        mtime_ns: 1,
        ctime_ns: 1,
        btime_ns: Some(1),
        device: 1,
        inode,
    }
}

fn metadata(format: Format, taken: &str) -> AssetMetadata {
    let mut meta = AssetMetadata::minimal(format);
    meta.width = Some(4000);
    meta.height = Some(3000);
    if !taken.is_empty() {
        let naive = chrono::NaiveDateTime::parse_from_str(taken, "%Y-%m-%d %H:%M").expect("date");
        meta.taken = CaptureTime::floating(naive, DateSource::Exif, Confidence::Medium);
    }
    meta
}

struct Fixture {
    _dir: tempfile::TempDir,
    writer: Writer,
    linked: SourceId,
    next_inode: u64,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.sqlite3");
        let mut writer =
            Writer::create(&path, LibraryUuid::new(), &dir.path().join("Media")).expect("create");
        let linked = writer
            .add_source(Path::new("/home/me/Pictures"), "Pictures")
            .expect("source");
        Self {
            _dir: dir,
            writer,
            linked,
            next_inode: 1,
        }
    }

    fn add(&mut self, relpath: &str, meta: &AssetMetadata, hash: Option<[u8; 32]>) -> AssetId {
        self.next_inode += 1;
        let relpath = PathBytes::from_vec(relpath.as_bytes().to_vec());
        let name = relpath.file_name_lossy();
        self.writer
            .add_file(
                &NewFile {
                    source: self.linked,
                    relpath: &relpath,
                    stamp: stamp(100, self.next_inode),
                    status: if hash.is_some() {
                        FileStatus::Verified
                    } else {
                        FileStatus::Unverified
                    },
                    hash,
                },
                &name,
                meta,
            )
            .expect("add file")
            .asset
    }

    fn ids(&self, query: &str) -> Vec<AssetId> {
        self.writer
            .view_ids(&View::library().with_query(Query::parse(query)))
            .expect("view")
    }
}

#[test]
fn create_open_and_reject_foreign_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("catalog.sqlite3");
    let uuid = LibraryUuid::new();
    drop(Writer::create(&path, uuid, dir.path()).expect("create"));
    let writer = Writer::open(&path).expect("reopen");
    assert_eq!(writer.library_uuid().expect("uuid"), uuid);
    assert_eq!(writer.quick_check().expect("check"), "ok");
    assert!(Writer::create(&path, uuid, dir.path()).is_err());
    let reader = Db::open_read_only(&path).expect("reader");
    assert_eq!(reader.managed_source().expect("managed").name, "Library");

    let foreign = dir.path().join("other.sqlite3");
    let conn = rusqlite::Connection::open(&foreign).expect("open");
    conn.execute_batch("CREATE TABLE notes (x TEXT);")
        .expect("table");
    drop(conn);
    assert!(Writer::open(&foreign).is_err());
}

#[test]
fn timeline_orders_by_capture_time_and_puts_unknown_last() {
    let mut fx = Fixture::new();
    let old = fx.add(
        "a/old.jpg",
        &metadata(Format::Jpeg, "2019-05-01 10:00"),
        None,
    );
    let new = fx.add(
        "a/new.jpg",
        &metadata(Format::Jpeg, "2024-01-02 09:30"),
        None,
    );
    let unknown = fx.add("a/unknown.png", &metadata(Format::Png, ""), None);
    let items = fx.writer.timeline(&View::library()).expect("timeline");
    let order: Vec<AssetId> = items.iter().map(|item| item.id).collect();
    assert_eq!(order, vec![new, old, unknown]);
    assert!((items[0].aspect - 4.0 / 3.0).abs() < 1e-3);
    let oldest = fx
        .writer
        .view_ids(&View::library().sorted(Sort::OldestFirst))
        .expect("view");
    assert_eq!(oldest, vec![old, new, unknown]);
}

#[test]
fn exact_copies_become_one_asset_with_two_files() {
    let mut fx = Fixture::new();
    let hash = [7u8; 32];
    let first = fx.add(
        "one/IMG_1.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        Some(hash),
    );
    let second = fx.add(
        "two/IMG_1 copy.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        Some(hash),
    );
    assert_eq!(first, second);
    assert_eq!(fx.writer.files_of(first).expect("files").len(), 2);
    assert_eq!(fx.writer.stats().expect("stats").assets, 1);
}

#[test]
fn hashing_later_merges_duplicates_and_keeps_user_edits() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let b = fx.add("b.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    fx.writer.add_tag(&[a], "Family").expect("tag");
    fx.writer.set_favorite(&[b], true).expect("fav");
    let hash = [9u8; 32];
    let files_a = fx.writer.files_of(a).expect("files");
    let files_b = fx.writer.files_of(b).expect("files");
    assert_eq!(
        fx.writer
            .set_file_hash(files_a[0].file.id, &hash)
            .expect("hash a"),
        None
    );
    assert_eq!(
        fx.writer
            .set_file_hash(files_b[0].file.id, &hash)
            .expect("hash b"),
        Some(b)
    );
    let merged = fx.writer.asset(a).expect("asset");
    assert!(merged.favorite);
    assert_eq!(fx.writer.tags_of(a).expect("tags")[0].name, "Family");
    assert_eq!(fx.writer.files_of(a).expect("files").len(), 2);
    assert!(fx.writer.try_asset(b).expect("lookup").is_none());
    assert_eq!(merged.file_state, FileStatus::Verified);
}

#[test]
fn changed_content_on_a_hashed_file_is_flagged_for_review() {
    let mut fx = Fixture::new();
    let asset = fx.add(
        "a.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        Some([1u8; 32]),
    );
    let file = fx.writer.files_of(asset).expect("files")[0].file.id;
    fx.writer.set_file_hash(file, &[2u8; 32]).expect("rehash");
    assert_eq!(
        fx.writer.asset(asset).expect("asset").file_state,
        FileStatus::Changed
    );
    let reviews = fx
        .writer
        .reviews(Some(ReviewKind::ChangedFile), 10)
        .expect("reviews");
    assert_eq!(reviews.len(), 1);
    let mut meta = metadata(Format::Jpeg, "2020-01-01 00:00");
    meta.width = Some(10);
    fx.writer
        .accept_changed(file, &[2u8; 32], &stamp(120, 99), &meta)
        .expect("accept");
    let accepted = fx.writer.asset(asset).expect("asset");
    assert_eq!(accepted.file_state, FileStatus::Verified);
    assert_eq!(accepted.width, Some(10));
    assert!(
        fx.writer
            .reviews(Some(ReviewKind::ChangedFile), 10)
            .expect("reviews")
            .is_empty()
    );
}

#[test]
fn missing_files_open_one_review_item_and_resolve_when_found() {
    let mut fx = Fixture::new();
    let asset = fx.add(
        "gone.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        None,
    );
    let file = fx.writer.files_of(asset).expect("files")[0].file.id;
    fx.writer
        .set_file_status(file, FileStatus::Missing)
        .expect("missing");
    fx.writer
        .set_file_status(file, FileStatus::Missing)
        .expect("missing again");
    let counts = fx.writer.review_counts().expect("counts");
    assert_eq!(counts, vec![(ReviewKind::MissingFile, 1)]);
    assert_eq!(fx.ids("is:problem"), vec![asset]);
    let moved = PathBytes::from_vec(b"found/gone.jpg".to_vec());
    fx.writer
        .move_file(file, &moved, &stamp(100, 50))
        .expect("move");
    assert!(fx.writer.review_counts().expect("counts").is_empty());
    assert!(fx.ids("is:problem").is_empty());
}

#[test]
fn search_filters_compose() {
    let mut fx = Fixture::new();
    let mut beach = metadata(Format::Jpeg, "2023-07-14 12:00");
    beach.camera_make = Some("Google".into());
    beach.camera_model = Some("Pixel 7".into());
    beach.location = GeoPoint::new(41.38, 2.17, None);
    beach.place = Some(Place {
        city: "Barcelona".into(),
        region: Some("Catalonia".into()),
        country: "Spain".into(),
        country_code: "ES".into(),
    });
    let beach = fx.add("Trips/Spain 2023/beach_sunset.jpg", &beach, None);
    let mut video = metadata(Format::Mp4, "2021-12-24 18:00");
    video.kind = MediaKind::Video;
    let video = fx.add("Family/xmas.mp4", &video, None);
    let screenshot = {
        let mut meta = metadata(Format::Png, "2022-03-01 08:00");
        meta.is_screenshot = true;
        fx.add("Screenshots/Screenshot_1.png", &meta, None)
    };
    fx.writer.add_tag(&[beach, video], "holiday").expect("tag");
    fx.writer.set_favorite(&[video], true).expect("fav");
    fx.writer.set_rating(&[beach], 4).expect("rating");

    assert_eq!(fx.ids("sunset"), vec![beach]);
    assert_eq!(
        fx.ids("spain"),
        vec![beach],
        "folder and place words are searchable"
    );
    assert_eq!(fx.ids("tag:Holiday"), vec![beach, video]);
    assert_eq!(fx.ids("tag:holiday -is:video"), vec![beach]);
    assert_eq!(fx.ids("is:fav"), vec![video]);
    assert_eq!(fx.ids("rating>=3"), vec![beach]);
    assert_eq!(fx.ids("2023"), vec![beach]);
    assert_eq!(fx.ids("december 2021"), vec![video]);
    assert_eq!(fx.ids("month:july"), vec![beach]);
    assert_eq!(fx.ids("camera:pixel"), vec![beach]);
    assert_eq!(fx.ids("place:barcelona"), vec![beach]);
    assert_eq!(fx.ids("has:location"), vec![beach]);
    assert_eq!(fx.ids("-has:tags"), vec![screenshot]);
    assert_eq!(fx.ids("is:screenshot"), vec![screenshot]);
    assert_eq!(fx.ids("in:family"), vec![video]);
    assert_eq!(fx.ids("format:png"), vec![screenshot]);
    assert_eq!(fx.ids("-date:2023"), vec![screenshot, video]);
}

#[test]
fn hidden_and_trashed_assets_leave_the_library_view() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let b = fx.add("b.jpg", &metadata(Format::Jpeg, "2020-01-02 00:00"), None);
    fx.writer.set_hidden(&[a], true).expect("hide");
    fx.writer.set_trashed(&[b], true).expect("trash");
    assert!(fx.ids("").is_empty());
    assert_eq!(fx.ids("is:hidden"), vec![a]);
    let trash = View {
        scope: Scope::Trash,
        query: Query::default(),
        sort: Sort::NewestFirst,
    };
    let trashed = fx.writer.timeline(&trash).expect("trash");
    assert_eq!(trashed.len(), 1);
    assert!(trashed[0].has(flags::TRASHED));
    fx.writer.set_trashed(&[b], false).expect("restore");
    assert_eq!(fx.ids(""), vec![b]);
}

#[test]
fn tags_rename_merge_and_delete() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let b = fx.add("b.jpg", &metadata(Format::Jpeg, "2020-01-02 00:00"), None);
    let kid = fx.writer.add_tag(&[a], "kids").expect("tag");
    let family = fx.writer.add_tag(&[b], "Family").expect("tag");
    let renamed = fx.writer.rename_tag(kid, " children ").expect("rename");
    assert_eq!(renamed, kid);
    assert_eq!(fx.ids("children"), vec![a]);
    let merged = fx
        .writer
        .rename_tag(kid, "family")
        .expect("merge by rename");
    assert_eq!(merged, family);
    let tags = fx.writer.tags().expect("tags");
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].count, 2);
    assert_eq!(tags[0].name, "family");
    fx.writer.delete_tag(family).expect("delete");
    assert!(fx.writer.tags().expect("tags").is_empty());
    assert!(fx.ids("family").is_empty());
    assert_eq!(
        fx.ids(""),
        vec![b, a],
        "deleting a tag never deletes photos"
    );
}

#[test]
fn albums_keep_manual_order() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let b = fx.add("b.jpg", &metadata(Format::Jpeg, "2020-01-02 00:00"), None);
    let c = fx.add("c.jpg", &metadata(Format::Jpeg, "2020-01-03 00:00"), None);
    let album = fx.writer.create_album("Best of", None).expect("album");
    assert_eq!(
        fx.writer.add_to_album(album, &[a, b, c, a]).expect("add"),
        3
    );
    fx.writer
        .reorder_album(album, &[c], Some(a))
        .expect("reorder");
    let view = View {
        scope: Scope::Album(album),
        query: Query::default(),
        sort: Sort::AlbumOrder,
    };
    assert_eq!(fx.writer.view_ids(&view).expect("ids"), vec![c, a, b]);
    assert_eq!(fx.ids("album:\"best of\""), vec![c, b, a]);
    assert_eq!(fx.ids("best"), vec![c, b, a], "album names are searchable");
    fx.writer.remove_from_album(album, &[a]).expect("remove");
    assert_eq!(fx.writer.album(album).expect("album").count, 2);
    fx.writer.delete_album(album).expect("delete");
    assert_eq!(fx.ids("").len(), 3);
}

#[test]
fn manual_dates_survive_metadata_refresh_and_can_be_reset() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 10:00"), None);
    let chosen = chrono::NaiveDate::from_ymd_opt(1999, 12, 31)
        .and_then(|d| d.and_hms_opt(23, 0, 0))
        .expect("date");
    fx.writer
        .set_taken(
            &[a],
            CaptureTime::floating(chosen, DateSource::Manual, Confidence::High),
        )
        .expect("set");
    fx.writer
        .refresh_metadata(a, &metadata(Format::Jpeg, "2020-01-01 10:00"))
        .expect("refresh");
    let asset = fx.writer.asset(a).expect("asset");
    assert_eq!(asset.taken.local_ms, Some(local_ms_from_naive(chosen)));
    assert_eq!(asset.taken.source, DateSource::Manual);
    fx.writer.shift_taken(&[a], 3_600_000).expect("shift");
    fx.writer.reset_taken(&[a]).expect("reset");
    assert_eq!(
        fx.writer.asset(a).expect("asset").taken.source,
        DateSource::Exif
    );
}

#[test]
fn live_photos_pair_by_name_and_hide_the_video() {
    let mut fx = Fixture::new();
    let photo = fx.add(
        "dcim/IMG_0001.HEIC",
        &metadata(Format::Heic, "2024-06-01 10:00"),
        None,
    );
    let mut clip = metadata(Format::Mov, "2024-06-01 10:00");
    clip.kind = MediaKind::Video;
    clip.duration_ms = Some(2_800);
    let video = fx.add("dcim/IMG_0001.MOV", &clip, None);
    let asset = fx.writer.asset(photo).expect("asset");
    assert_eq!(asset.live_video, Some(video));
    assert_eq!(fx.ids(""), vec![photo]);
    assert_eq!(fx.ids("is:live"), vec![photo]);
}

#[test]
fn review_items_deduplicate_per_subject() {
    let mut fx = Fixture::new();
    let path = PathBytes::from_vec(b"/media/card/DSC_1.JPG".to_vec());
    let detail = serde_json::json!({ "reason": "unreadable" });
    let first = fx
        .writer
        .open_review(
            ReviewKind::FailedImport,
            ReviewSubject::Path(&path),
            &detail,
        )
        .expect("open");
    let second = fx
        .writer
        .open_review(
            ReviewKind::FailedImport,
            ReviewSubject::Path(&path),
            &detail,
        )
        .expect("again");
    assert_eq!(first, second);
    let items = fx.writer.reviews(None, 10).expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].subject, "DSC_1.JPG");
    fx.writer
        .close_path_reviews(ReviewKind::FailedImport, &path)
        .expect("close");
    assert!(fx.writer.reviews(None, 10).expect("items").is_empty());
}

#[test]
fn removing_a_source_forgets_only_its_assets() {
    let mut fx = Fixture::new();
    let a = fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let removed = fx.writer.remove_source(fx.linked).expect("remove");
    assert_eq!(removed.len(), 1);
    assert!(fx.writer.try_asset(a).expect("lookup").is_none());
    let managed = fx.writer.managed_source().expect("managed");
    assert!(fx.writer.remove_source(managed.id).is_err());
}

#[test]
fn metadata_export_round_trips_into_a_new_catalog() {
    let mut fx = Fixture::new();
    let hash = [5u8; 32];
    let a = fx.add(
        "a.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        Some(hash),
    );
    fx.writer.add_tag(&[a], "Trip").expect("tag");
    fx.writer.set_rating(&[a], 5).expect("rating");
    let album = fx.writer.create_album("Keepers", None).expect("album");
    fx.writer.add_to_album(album, &[a]).expect("add");
    let export = fx.writer.export_metadata().expect("export");
    let json = serde_json::to_string(&export).expect("json");
    let parsed: medianest_catalog::MetadataExport = serde_json::from_str(&json).expect("parse");
    assert_eq!(parsed.assets.len(), 1);

    let mut other = Fixture::new();
    let b = other.add(
        "elsewhere/a.jpg",
        &metadata(Format::Jpeg, "2020-01-01 00:00"),
        Some(hash),
    );
    let entry = &parsed.assets[0];
    let found = other
        .writer
        .asset_by_hash(&entry.hash_bytes().expect("hash"))
        .expect("lookup");
    assert_eq!(found, Some(b));
    other
        .writer
        .apply_user_metadata(b, &entry.metadata)
        .expect("apply");
    let asset = other.writer.asset(b).expect("asset");
    assert_eq!(asset.rating, 5);
    assert_eq!(other.ids("tag:trip album:keepers"), vec![b]);
    let _unused = UserMetadata::default();
}

#[test]
fn backups_are_standalone_catalogs() {
    let mut fx = Fixture::new();
    fx.add("a.jpg", &metadata(Format::Jpeg, "2020-01-01 00:00"), None);
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("backup.sqlite3");
    fx.writer.backup_to(&target).expect("backup");
    let restored = Db::open_read_only(&target).expect("open backup");
    assert_eq!(restored.stats().expect("stats").assets, 1);
}
