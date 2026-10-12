# Changelog

## Unreleased

- Move removes each original as soon as its copy is verified, instead of
  keeping every original until you approve removal. Originals that changed
  or cannot be removed safely are kept and shown in Review.
- Move on the library's own disk is a rename: nothing is copied and no extra
  space is needed. It is verified by file identity and undone if the catalog
  cannot be updated; files with other hard links are copied instead.
- Imports check free space before starting (keeping 512 MB in reserve), show
  the shortfall in the import dialog and the CLI, and stop with one message
  when a disk fills up instead of failing every remaining file.
- Faster imports: fewer journal commits and folder flushes per photo, about
  30% quicker for many small files on an SSD, and the verification re-read
  asks the kernel to skip its page cache.

## 1.0.0 — 2026-10-11

First release of the rewritten MediaNest.

- A GTK 4 and libadwaita application with a fast photo timeline, viewer,
  albums and smart albums, tags, places, folders, a review inbox, library
  health, import, export and preferences.
- Journaled, verified imports that never modify or overwrite files, with
  crash recovery at every step.
- Linked folders that are watched instead of copied.
- HEIC, AVIF, JPEG XL, RAW previews, videos, Live Photos and motion photos.
- Offline place names, periodic verification and catalog backups.
- `medianest-cli` for the command line.
