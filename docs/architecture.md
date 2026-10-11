# Architecture

MediaNest is a Cargo workspace of seven crates. Each layer depends only on
the ones below it.

```
medianest-app (GTK)    medianest-cli
          \              /
        medianest-service           the running library
          /       |       \
 medianest-vault  |  medianest-media
          \       |       /
        medianest-catalog
                |
        medianest-core
```

## medianest-core

Domain types shared by everything: identifiers, kinds and states,
`CaptureTime` (a local wall-clock time, an optional UTC offset, where the
date came from and how much to trust it), metadata, paths stored as raw
bytes, and the search language (`Query::parse`) used by the search bar,
smart albums and the command line.

## medianest-catalog

The SQLite catalog. `Writer` owns the single writable connection; `Db` is a
read-only connection, pooled by the service. `View` (a scope, a query and a
sort order) compiles to SQL, so the timeline, search, smart albums, counts
and the map all share one implementation. Full-text search uses FTS5.

## medianest-media

Everything about reading media files: sniffing formats by content, reading
EXIF, XMP and HEIF/MP4 boxes, dates from file names, `ffprobe` for videos,
decoding images (with HEIC and AVIF decoded in a helper process so a
malformed file cannot take the application down), thumbnails and previews,
average colors and perceptual hashes, and offline reverse geocoding from a
compact GeoNames table compiled into the binary.

## medianest-vault

The only code that changes files. It creates and opens libraries, takes the
single-writer lock, imports with journaled and verified copies, removes
originals after approval, moves deleted items to the system trash and back,
scans and watches linked folders, verifies files, backs up the catalog, and
recovers interrupted operations. Its tests inject a crash at every
checkpoint and check that recovery always ends in a correct state.

## medianest-service

A `Library` is an open library for the length of a session: the lock, the
writer behind a mutex, a pool of readers, a worker pool, background jobs
(foreground jobs such as imports, and background ones such as hashing and
verification that yield to them), the thumbnailer, folder watching and
scheduled maintenance. Interfaces call blocking methods through
`Library::run`, which returns an awaitable task, and listen to change events.

## medianest-cli

A thin command-line front end over the service, with `--json` output.

## medianest-app

The GNOME application, written in Rust with GTK 4 and libadwaita, without
Blueprint or UI files.

- `grid/` is the photo grid: one custom widget that lays out and draws every
  tile with GTK snapshots instead of creating a widget per photo. Layout is
  pure computation, tested separately, and takes a few milliseconds for
  100,000 items. Thumbnails are decoded on worker threads; only what is on
  screen (and a screen ahead) is queued, and textures are kept within a
  memory budget.
- `viewer/` shows one item at a time: a zoomable image that starts from the
  grid thumbnail, sharpens to a cached preview, and loads the full
  resolution only when zoomed in; videos; Live Photos; and the information
  panel.
- `pages/` holds the sidebar sections, and `dialogs/` the import, date, tag,
  album, export and preferences dialogs.
- All library work runs off the main thread; the interface reacts to the
  service's change events.

## Performance budget

With 100,000 items (see `crates/medianest-catalog/tests/scale.rs`), on an
ordinary laptop: loading the whole timeline takes about 150 ms, searches
30–50 ms, statistics and map points under 150 ms, and the grid scrolls
without dropping frames.
