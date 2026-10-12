#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use medianest_core::{AssetUuid, Confidence, DateSource, Format, MediaKind};
use medianest_media::{FILMSTRIP_FRAMES, MediaToolkit, Size, ThumbnailCache, ThumbnailSource};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn toolkit() -> MediaToolkit {
    MediaToolkit::in_process()
}

#[test]
fn camera_jpeg_without_timezone_keeps_its_date_and_gains_one_from_gps() {
    let metadata = toolkit().probe(&fixture("camera.jpg")).expect("probe");
    assert_eq!(metadata.format, Format::Jpeg);
    assert_eq!((metadata.width, metadata.height), (Some(96), Some(64)));
    assert_eq!(metadata.orientation, 6);
    assert_eq!(metadata.oriented_size(), Some((64, 96)));
    let taken = metadata.taken;
    assert_eq!(
        taken
            .local()
            .map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .as_deref(),
        Some("2019-08-10 14:30:05.250")
    );
    // The GPS clock said 12:30 UTC, so the camera was on UTC+2.
    assert_eq!(taken.offset_minutes, Some(120));
    assert_eq!(taken.source, DateSource::ExifWithOffset);
    assert!(taken.is_trustworthy());
    assert_eq!(metadata.camera_name().as_deref(), Some("NIKON D750"));
    assert_eq!(metadata.iso, Some(200));
    assert!(metadata.aperture.is_some_and(|f| (f - 5.6).abs() < 0.01));
    assert!(
        metadata
            .exposure_seconds
            .is_some_and(|s| (s - 0.004).abs() < 1e-4)
    );
    let place = metadata.place.expect("place");
    assert_eq!(
        (place.city.as_str(), place.country.as_str()),
        ("Paris", "France")
    );
}

#[test]
fn phone_jpeg_with_offset_is_high_confidence() {
    let metadata = toolkit().probe(&fixture("phone.jpg")).expect("probe");
    assert_eq!(metadata.taken.offset_minutes, Some(120));
    assert_eq!(metadata.taken.confidence, Confidence::High);
    assert_eq!(metadata.camera_name().as_deref(), Some("Google Pixel 7"));
}

#[test]
fn heic_size_reflects_container_rotation() {
    let metadata = toolkit().probe(&fixture("rotated.heic")).expect("probe");
    assert_eq!(metadata.format, Format::Heic);
    assert_eq!((metadata.width, metadata.height), (Some(64), Some(96)));
    assert_eq!(metadata.orientation, 1);
    let image = toolkit()
        .decoder
        .decode(&fixture("rotated.heic"), Format::Heic, 1, Size::Full)
        .expect("decode");
    assert_eq!(image.dimensions(), (64, 96));
}

#[test]
fn jpeg_xl_is_sized_and_decoded_through_ffmpeg() {
    let tools = medianest_media::VideoTools::detect();
    if !tools.available() {
        return;
    }
    let metadata = toolkit().probe(&fixture("small.jxl")).expect("probe");
    assert_eq!((metadata.width, metadata.height), (Some(96), Some(64)));
    let image = toolkit()
        .decoder
        .decode(&fixture("small.jxl"), Format::Jxl, 1, Size::Full)
        .expect("decode");
    assert_eq!(image.dimensions(), (96, 64));
}

#[test]
fn video_metadata_comes_from_ffprobe() {
    let toolkit = toolkit();
    if !toolkit.video.available() {
        return;
    }
    let metadata = toolkit.probe(&fixture("clip.mp4")).expect("probe");
    assert_eq!(metadata.kind, MediaKind::Video);
    assert_eq!(metadata.duration_ms, Some(2000));
    assert_eq!(metadata.taken.source, DateSource::Container);
    assert!(metadata.location.is_some());

    let dir = tempfile::tempdir().expect("tempdir");
    let cache = ThumbnailCache::with_root(dir.path().to_path_buf());
    let uuid = AssetUuid::new();
    cache
        .generate(
            uuid,
            &ThumbnailSource {
                path: &fixture("clip.mp4"),
                format: Format::Mp4,
                orientation: 1,
                motion_video_length: None,
                duration_ms: metadata.duration_ms,
            },
            &toolkit.decoder,
            &toolkit.video,
        )
        .expect("video thumbnail");
    assert!(cache.thumbnail_path(uuid).is_file());
}

#[test]
fn video_filmstrip_holds_equal_frames_and_is_cached() {
    let toolkit = toolkit();
    if !toolkit.video.available() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = ThumbnailCache::with_root(dir.path().to_path_buf());
    let uuid = AssetUuid::new();
    let clip = fixture("clip.mp4");
    let source = ThumbnailSource {
        path: &clip,
        format: Format::Mp4,
        orientation: 1,
        motion_video_length: None,
        duration_ms: Some(2000),
    };
    let path = cache
        .filmstrip(uuid, &source, &toolkit.video)
        .expect("filmstrip");
    assert_eq!(path, cache.filmstrip_path(uuid));
    let (width, height) = image::image_dimensions(&path).expect("strip size");
    assert_eq!(width % FILMSTRIP_FRAMES, 0);
    assert!(
        width / FILMSTRIP_FRAMES >= height / 4,
        "cells are not slivers"
    );

    let again = cache
        .filmstrip(uuid, &source, &toolkit.video)
        .expect("cached filmstrip");
    assert_eq!(again, path);

    cache.remove(uuid);
    assert!(!path.exists());
}

#[test]
fn an_unreadable_video_leaves_no_filmstrip_behind() {
    let toolkit = toolkit();
    if !toolkit.video.available() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = ThumbnailCache::with_root(dir.path().join("cache"));
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, [0x5a; 4096]).expect("write");
    let uuid = AssetUuid::new();
    let source = ThumbnailSource {
        path: &broken,
        format: Format::Mp4,
        orientation: 1,
        motion_video_length: None,
        duration_ms: Some(8000),
    };
    assert!(cache.filmstrip(uuid, &source, &toolkit.video).is_err());
    assert!(!cache.filmstrip_path(uuid).exists());
}

#[test]
fn video_filmstrip_finds_the_duration_when_the_catalog_has_none() {
    let toolkit = toolkit();
    if !toolkit.video.available() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cache = ThumbnailCache::with_root(dir.path().to_path_buf());
    let clip = fixture("clip.mp4");
    let source = ThumbnailSource {
        path: &clip,
        format: Format::Mp4,
        orientation: 1,
        motion_video_length: None,
        duration_ms: None,
    };
    cache
        .filmstrip(AssetUuid::new(), &source, &toolkit.video)
        .expect("filmstrip");
}

#[test]
fn probing_never_modifies_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let copy = dir.path().join("camera.jpg");
    std::fs::copy(fixture("camera.jpg"), &copy).expect("copy");
    let before = std::fs::read(&copy).expect("read");
    let modified = std::fs::metadata(&copy)
        .expect("meta")
        .modified()
        .expect("mtime");
    toolkit().probe(&copy).expect("probe");
    assert_eq!(std::fs::read(&copy).expect("read"), before);
    assert_eq!(
        std::fs::metadata(&copy)
            .expect("meta")
            .modified()
            .expect("mtime"),
        modified
    );
}
