#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use medianest_catalog::View;
use medianest_core::query::Query;
use medianest_core::{ReviewKind, SourceKind};
use medianest_media::MediaToolkit;
use medianest_service::{
    Event, ExportOptions, ImportItem, ImportItemState, ImportOptions, JobState, Lane, Library,
    OpenOptions, ReviewAction, Settings, SpaceCheck,
};
use medianest_vault::{Control, ImportMode};

struct Session {
    dir: tempfile::TempDir,
    library: Library,
}

fn quiet_settings() -> Settings {
    Settings {
        watch_folders: false,
        backup_every_hours: 0,
        verify_every_days: 0,
        ..Settings::default()
    }
}

fn options(dir: &Path) -> OpenOptions {
    OpenOptions {
        settings: quiet_settings(),
        toolkit: Some(MediaToolkit::in_process()),
        cache_root: Some(dir.join("cache")),
    }
}

impl Session {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let library =
            Library::create_with(&dir.path().join("Library"), options(dir.path())).expect("create");
        Self { dir, library }
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::create_dir_all(&path).expect("folder");
        path
    }

    fn timeline(&self, query: &str) -> Vec<medianest_catalog::TimelineItem> {
        self.library
            .read(|db| db.timeline(&View::library().with_query(Query::parse(query))))
            .expect("timeline")
    }
}

fn photo(path: &Path, seed: u8) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("dir");
    }
    image::RgbImage::from_fn(120, 80, |x, y| {
        image::Rgb([
            seed.wrapping_add((x * 2) as u8),
            (y * 3) as u8,
            seed.wrapping_mul(7),
        ])
    })
    .save_with_format(path, image::ImageFormat::Png)
    .expect("save");
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn linking_a_folder_indexes_it_and_makes_thumbnails() {
    let session = Session::new();
    let pictures = session.folder("Pictures");
    photo(&pictures.join("IMG_20230701_101500.png"), 1);
    photo(&pictures.join("trip/IMG_20230702_090000.png"), 2);
    let events = session.library.subscribe();
    let (_, job) = session.library.add_folder(&pictures).expect("add folder");
    let finished = session.library.wait_for_job(job).expect("job");
    assert_eq!(finished.state, JobState::Completed, "{finished:?}");
    assert!(
        finished.summary.starts_with("2 new items"),
        "{}",
        finished.summary
    );

    let items = session.timeline("");
    assert_eq!(items.len(), 2);
    let ids: Vec<_> = items.iter().map(|item| item.id).collect();
    session.library.request_thumbnails(&ids);
    wait_until("thumbnails", || {
        items
            .iter()
            .all(|item| session.library.thumbnail_path(item.uuid).is_file())
    });
    let mut saw_ready = false;
    while let Ok(event) = events.try_recv() {
        saw_ready |= matches!(event, Event::ThumbnailReady(..));
    }
    assert!(saw_ready);
    // Hashing the new linked files happens in the background.
    wait_until("background hashing", || {
        session
            .library
            .read(|db| db.stats())
            .unwrap()
            .unverified_files
            == 0
    });
    let tree = session
        .library
        .folders(session.library.sources().unwrap()[1].id)
        .expect("folders");
    assert_eq!(tree.total, 2);
    assert_eq!(tree.children[0].name, "trip");
}

#[test]
fn import_plans_show_what_will_happen_and_imports_follow_them() {
    let session = Session::new();
    let card = session.folder("Card/DCIM");
    photo(&card.join("IMG_20240510_180000.png"), 10);
    photo(&card.join("IMG_20240511_120000.png"), 11);
    fs::copy(card.join("IMG_20240510_180000.png"), card.join("copy.png")).expect("copy");
    photo(&card.join("undated.png"), 12);
    fs::write(card.join("broken.png"), b"\x89PNG\r\n\x1a\nnot really").expect("broken");
    fs::write(card.join("notes.txt"), b"hello").expect("text");

    let plan = session
        .library
        .plan_import(
            &[session.dir.path().join("Card")],
            &Control::new(),
            &|_, _| {},
        )
        .expect("plan");
    assert_eq!(plan.new_count, 3, "{plan:#?}");
    assert_eq!(plan.duplicates_in_selection, 1);
    assert_eq!(plan.unreadable, 1);
    assert_eq!(plan.other_files, 1);
    assert_eq!(plan.uncertain_dates, 1);
    assert_eq!(plan.months, vec![(2024, 5, 2)]);

    let job = session.library.import(
        plan,
        ImportOptions {
            mode: ImportMode::Copy,
        },
    );
    let finished = session.library.wait_for_job(job).expect("job");
    assert_eq!(finished.state, JobState::Completed);
    assert_eq!(finished.summary, "Imported 3 items");
    let media = session.library.layout().media();
    assert!(media.join("2024/05/10/IMG_20240510_180000.png").is_file());
    assert!(media.join("Unsorted/undated.png").is_file());
    assert!(
        card.join("IMG_20240510_180000.png").is_file(),
        "originals stay"
    );

    let again = session
        .library
        .plan_import(std::slice::from_ref(&card), &Control::new(), &|_, _| {})
        .expect("plan again");
    assert_eq!(again.new_count, 0);
    // Including the copy that was a duplicate within the first selection.
    assert_eq!(again.already_in_library, 4);
    assert!(
        again
            .items
            .iter()
            .any(|item| item.state == ImportItemState::AlreadyInLibrary)
    );
}

#[test]
fn moves_are_approved_through_the_review_inbox() {
    let session = Session::new();
    let card = session.folder("Card");
    photo(&card.join("a.png"), 20);
    photo(&card.join("b.png"), 21);
    let plan = session
        .library
        .plan_import(std::slice::from_ref(&card), &Control::new(), &|_, _| {})
        .expect("plan");
    let job = session.library.import(
        plan,
        ImportOptions {
            mode: ImportMode::Move,
        },
    );
    let finished = session.library.wait_for_job(job).expect("job");
    assert!(
        !finished.summary.contains("kept for your review"),
        "{}",
        finished.summary
    );
    let counts = session.library.review_counts().expect("counts");
    assert!(
        !counts
            .iter()
            .any(|(kind, _)| *kind == ReviewKind::PendingRemoval)
    );
    assert!(!card.join("a.png").exists() && !card.join("b.png").exists());
    assert_eq!(session.timeline("").len(), 2);
}

#[test]
fn edits_search_and_albums_work_end_to_end() {
    let session = Session::new();
    let pictures = session.folder("Pictures");
    photo(&pictures.join("beach/IMG_20230701_101500.png"), 30);
    photo(&pictures.join("city/IMG_20230801_101500.png"), 31);
    let (_, job) = session.library.add_folder(&pictures).expect("add");
    session.library.wait_for_job(job);
    let items = session.timeline("");
    let (city, beach) = (items[0].id, items[1].id);
    session.library.add_tag(&[beach], "Summer").expect("tag");
    session.library.set_favorite(&[city], true).expect("fav");
    session.library.set_rating(&[beach], 5).expect("rating");
    assert_eq!(session.timeline("summer").len(), 1);
    assert_eq!(session.timeline("is:fav")[0].id, city);
    assert_eq!(
        session.timeline("beach")[0].id,
        beach,
        "folder names are searchable"
    );
    let album = session.library.create_album("Best", None).expect("album");
    session
        .library
        .add_to_album(album, &[city, beach])
        .expect("add");
    assert_eq!(session.timeline("album:best").len(), 2);
    let smart = session
        .library
        .create_album("Five stars", Some("rating:5"))
        .expect("smart");
    let albums = session.library.read(|db| db.albums()).expect("albums");
    let smart_album = albums.iter().find(|a| a.id == smart).expect("smart album");
    assert_eq!(
        session.library.album_cover(smart_album).expect("cover"),
        Some(beach)
    );
    session.library.set_hidden(&[city], true).expect("hide");
    assert_eq!(session.timeline("").len(), 1);
    assert_eq!(session.timeline("is:hidden").len(), 1);
}

#[test]
fn missing_files_are_reviewed_and_can_be_removed_from_the_library() {
    let session = Session::new();
    let pictures = session.folder("Pictures");
    photo(&pictures.join("keep.png"), 40);
    photo(&pictures.join("gone.png"), 41);
    let (source, job) = session.library.add_folder(&pictures).expect("add");
    session.library.wait_for_job(job);
    fs::remove_file(pictures.join("gone.png")).expect("delete");
    let job = session.library.scan(Some(source), Lane::Foreground);
    let finished = session.library.wait_for_job(job).expect("scan");
    assert!(
        finished.summary.contains("1 missing"),
        "{}",
        finished.summary
    );
    let items = session
        .library
        .review_items(Some(ReviewKind::MissingFile))
        .expect("items");
    assert_eq!(items.len(), 1);
    session
        .library
        .resolve_review(items[0].id, ReviewAction::Primary)
        .expect("remove");
    assert_eq!(session.timeline("").len(), 1);
    let job = session.library.scan(Some(source), Lane::Foreground);
    session.library.wait_for_job(job);
    assert!(
        session
            .library
            .review_items(None)
            .expect("items")
            .is_empty()
    );
}

#[test]
fn similar_photos_are_grouped() {
    let session = Session::new();
    let pictures = session.folder("Pictures");
    let base = image::RgbImage::from_fn(200, 150, |x, y| {
        let value = ((x / 20) * 37 + (y / 15) * 91) % 256;
        image::Rgb([value as u8, (255 - value) as u8, 80])
    });
    base.save(pictures.join("original.png")).expect("save");
    image::imageops::resize(&base, 100, 75, image::imageops::FilterType::Triangle)
        .save(pictures.join("smaller.png"))
        .expect("save");
    photo(&pictures.join("different.png"), 99);
    let (_, job) = session.library.add_folder(&pictures).expect("add");
    session.library.wait_for_job(job);
    let ids: Vec<_> = session.timeline("").iter().map(|item| item.id).collect();
    session.library.request_thumbnails(&ids);
    wait_until("hashes", || {
        session
            .library
            .read(|db| db.perceptual_hashes())
            .unwrap()
            .len()
            == 3
    });
    let groups = session.library.similar_groups().expect("groups");
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].assets.len(), 2);
    let detail = session
        .library
        .asset_detail(groups[0].assets[0])
        .expect("detail");
    assert_eq!(detail.asset.width, Some(200), "the larger copy ranks first");
    session
        .library
        .dismiss_similar(groups[0].assets[0], groups[0].assets[1])
        .expect("dismiss");
    assert!(session.library.similar_groups().expect("groups").is_empty());
}

#[test]
fn exports_and_metadata_round_trip() {
    let session = Session::new();
    let card = session.folder("Card");
    photo(&card.join("IMG_20230701_101500.png"), 50);
    let plan = session
        .library
        .plan_import(std::slice::from_ref(&card), &Control::new(), &|_, _| {})
        .expect("plan");
    session.library.wait_for_job(session.library.import(
        plan,
        ImportOptions {
            mode: ImportMode::Copy,
        },
    ));
    let id = session.timeline("")[0].id;
    session.library.add_tag(&[id], "trip").expect("tag");
    session.library.set_rating(&[id], 4).expect("rating");

    let out = session.folder("Export");
    fs::write(out.join("IMG_20230701_101500.png"), b"existing").expect("existing");
    let job = session.library.export_files(
        vec![id],
        ExportOptions {
            destination: out.clone(),
            folders_by_date: false,
            include_motion: false,
        },
    );
    let finished = session.library.wait_for_job(job).expect("export");
    assert_eq!(finished.state, JobState::Completed, "{finished:?}");
    assert_eq!(
        fs::read(out.join("IMG_20230701_101500.png")).expect("read"),
        b"existing",
        "never overwrites"
    );
    assert!(out.join("IMG_20230701_101500 (2).png").is_file());

    let json = session.dir.path().join("metadata.json");
    assert_eq!(session.library.export_metadata(&json).expect("export"), 1);

    // A fresh library with the same photo picks the metadata up by content.
    let other_dir = tempfile::tempdir().expect("tempdir");
    let other = Library::create_with(&other_dir.path().join("Library"), options(other_dir.path()))
        .expect("create");
    let plan = other
        .plan_import(&[card], &Control::new(), &|_, _| {})
        .expect("plan");
    other.wait_for_job(other.import(
        plan,
        ImportOptions {
            mode: ImportMode::Copy,
        },
    ));
    let report = other.import_metadata(&json, true).expect("dry run");
    assert_eq!((report.matched, report.applied), (1, false));
    let report = other.import_metadata(&json, false).expect("apply");
    assert!(report.applied);
    let found = other
        .read(|db| db.timeline(&View::library().with_query(Query::parse("tag:trip rating:4"))))
        .expect("search");
    assert_eq!(found.len(), 1);
    other.close();
}

#[test]
fn libraries_reopen_and_refuse_a_second_session() {
    let session = Session::new();
    let root = session.library.root().to_path_buf();
    assert!(
        Library::open_with(&root, options(session.dir.path())).is_err(),
        "locked while open"
    );
    session.library.backup_now().expect("backup");
    assert_eq!(session.library.backups().expect("backups").len(), 1);
    let health = session.library.health().expect("health");
    assert!(health.database_ok);
    session.library.close();
    assert!(
        session.library.read(|db| db.stats()).is_err(),
        "closed libraries refuse work"
    );
    let reopened = Library::open_with(&root, options(session.dir.path())).expect("reopen");
    assert_eq!(
        reopened.sources().expect("sources")[0].kind,
        SourceKind::Managed
    );
    reopened.close();
}

#[test]
fn awaitable_tasks_run_on_worker_threads() {
    let session = Session::new();
    let task = session
        .library
        .run(|library| library.read(|db| db.stats()).map(|stats| stats.assets));
    assert_eq!(task.wait().expect("ran").expect("stats"), 0);
}

fn item(
    size: u64,
    state: ImportItemState,
    on_library_volume: bool,
    source_removable: bool,
) -> ImportItem {
    ImportItem {
        path: PathBuf::from("/card/IMG.jpg"),
        size,
        metadata: None,
        state,
        destination: None,
        on_library_volume,
        source_removable,
    }
}

#[test]
fn space_needs_follow_the_import_mode() {
    const MB: u64 = 1 << 20;
    let reserve = medianest_vault::SPACE_RESERVE;
    let items = [
        item(100 * MB, ImportItemState::New, true, true),
        item(300 * MB, ImportItemState::New, true, true),
        item(50 * MB, ImportItemState::New, true, false),
        item(70 * MB, ImportItemState::New, false, true),
        item(900 * MB, ImportItemState::AlreadyInLibrary, true, true),
        item(900 * MB, ImportItemState::DuplicateInSelection, true, true),
    ];
    // A copy keeps every original, so every new file needs room.
    let copy = SpaceCheck::for_items(&items, ImportMode::Copy, 0);
    assert_eq!(
        (copy.required, copy.unremovable_bytes),
        (520 * MB + reserve, 0)
    );
    // A move frees each original on the library's own disk as it goes, so
    // only the largest is there twice. Originals that cannot be removed, and
    // originals on another disk, still take their full space here.
    let moved = SpaceCheck::for_items(&items, ImportMode::Move, 0);
    assert_eq!(
        (moved.required, moved.unremovable_bytes),
        (420 * MB + reserve, 50 * MB)
    );
    let fits = SpaceCheck::for_items(&items, ImportMode::Move, 420 * MB + reserve);
    assert!(!fits.is_short());
    assert!(SpaceCheck::for_items(&items, ImportMode::Move, 420 * MB + reserve - 1).is_short());
}
