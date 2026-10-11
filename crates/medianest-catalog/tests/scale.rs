//! A catalog of 100,000 photos stays quick to browse and search.
//!
//! Ignored by default because building the catalog takes a while. Run with
//! `cargo test --release -p medianest-catalog --test scale -- --ignored`.
//! Set `MEDIANEST_SCALE_CATALOG` to the catalog of an existing library to
//! fill that library instead, for trying the application at scale.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use medianest_catalog::{NewFile, View, Writer};
use medianest_core::query::Query;
use medianest_core::{
    AssetMetadata, CaptureTime, Confidence, DateSource, FileStamp, FileStatus, Format, GeoPoint,
    LibraryUuid, PathBytes, Place, ThumbState,
};

const COUNT: u64 = 100_000;

const BATCH: u64 = 2_000;

/// A small deterministic pseudo-random generator, so runs are comparable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn metadata(rng: &mut Rng, index: u64) -> AssetMetadata {
    let mut meta = AssetMetadata::minimal(Format::Jpeg);
    let (width, height) = [(4032, 3024), (3024, 4032), (1920, 1080), (3000, 3000)]
        [usize::try_from(rng.below(4)).unwrap()];
    meta.width = Some(width);
    meta.height = Some(height);
    // Twelve years of photos, in bursts.
    let day = 16_000 + i64::try_from(index / 23).unwrap();
    let seconds = i64::try_from(rng.below(86_400)).unwrap();
    let naive = chrono::DateTime::from_timestamp(day * 86_400 + seconds, 0)
        .unwrap()
        .naive_utc();
    meta.taken = CaptureTime::floating(naive, DateSource::Exif, Confidence::Medium);
    meta.camera_make = Some(
        ["Apple", "Google", "Canon", "FUJIFILM"][usize::try_from(rng.below(4)).unwrap()].into(),
    );
    meta.camera_model = Some(format!("Model {}", rng.below(6)));
    if rng.below(3) > 0 {
        let cities = [
            ("Paris", "France", "FR", 48.85, 2.35),
            ("Tehran", "Iran", "IR", 35.69, 51.39),
            ("Tokyo", "Japan", "JP", 35.68, 139.69),
            ("New York City", "United States", "US", 40.71, -74.0),
            ("Rome", "Italy", "IT", 41.9, 12.5),
        ];
        let (city, country, code, lat, lon) = cities[usize::try_from(rng.below(5)).unwrap()];
        meta.location = GeoPoint::new(lat, lon, None);
        meta.place = Some(Place {
            city: city.into(),
            region: None,
            country: country.into(),
            country_code: code.into(),
        });
    }
    meta
}

fn catalog_path(dir: &Path) -> (PathBuf, bool) {
    match std::env::var_os("MEDIANEST_SCALE_CATALOG") {
        Some(path) => (PathBuf::from(path), true),
        None => (dir.join("catalog.sqlite3"), false),
    }
}

#[test]
#[ignore = "builds a 100,000 item catalog"]
fn a_hundred_thousand_photos() {
    let dir = tempfile::tempdir().unwrap();
    let (path, existing) = catalog_path(dir.path());
    let mut writer = if existing {
        Writer::open(&path).unwrap()
    } else {
        Writer::create(&path, LibraryUuid::new(), &dir.path().join("Media")).unwrap()
    };
    let source = writer
        .add_source(&dir.path().join("Scale"), "Scale test")
        .unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    let started = Instant::now();
    let mut inserting = std::time::Duration::ZERO;
    let mut index = 0;
    while index < COUNT {
        let relpaths: Vec<PathBytes> = (index..index + BATCH)
            .map(|i| PathBytes::from_vec(format!("{}/IMG_{i:06}.jpg", i / 1000).into_bytes()))
            .collect();
        let items: Vec<_> = relpaths
            .iter()
            .enumerate()
            .map(|(offset, relpath)| {
                let i = index + offset as u64;
                let mut hash = [0u8; 32];
                hash[..8].copy_from_slice(&i.to_le_bytes());
                hash[8..16].copy_from_slice(&rng.next().to_le_bytes());
                let file = NewFile {
                    source,
                    relpath,
                    stamp: FileStamp {
                        size: 2_000_000 + rng.below(3_000_000),
                        mtime_ns: 1,
                        ctime_ns: 1,
                        btime_ns: None,
                        device: 1,
                        inode: i + 1,
                    },
                    status: FileStatus::Verified,
                    hash: Some(hash),
                };
                (file, relpath.file_name_lossy(), metadata(&mut rng, i))
            })
            .collect();
        let batch_started = Instant::now();
        let added = writer.add_files(&items).unwrap();
        inserting += batch_started.elapsed();
        for item in added {
            let color = u32::try_from(rng.below(0x00FF_FFFF)).unwrap();
            writer
                .set_thumbnail(item.asset, ThumbState::Ready, Some(color), None)
                .unwrap();
        }
        index += BATCH;
    }
    eprintln!(
        "inserted {COUNT} items in {inserting:?} ({:?} with thumbnail states)",
        started.elapsed()
    );

    let db = medianest_catalog::Db::open_read_only(&path).unwrap();
    let time = |label: &str, run: &dyn Fn() -> usize| {
        let started = Instant::now();
        let count = run();
        let elapsed = started.elapsed();
        eprintln!("{label}: {count} in {elapsed:?}");
        elapsed
    };
    let timeline = time("timeline", &|| db.timeline(&View::library()).unwrap().len());
    let search = time("search place and camera", &|| {
        db.timeline(&View::library().with_query(Query::parse("place:paris camera:canon")))
            .unwrap()
            .len()
    });
    let text = time("full-text search", &|| {
        db.timeline(&View::library().with_query(Query::parse("tokyo")))
            .unwrap()
            .len()
    });
    let stats = time("statistics", &|| {
        usize::try_from(db.stats().unwrap().assets).unwrap()
    });
    let map = time("map points", &|| {
        db.map_points(&View::library()).unwrap().len()
    });

    // Generous bounds that still catch a missing index or a quadratic step.
    let limit = std::time::Duration::from_secs(2);
    for elapsed in [timeline, search, text, stats, map] {
        assert!(elapsed < limit, "a query took {elapsed:?}");
    }
}
