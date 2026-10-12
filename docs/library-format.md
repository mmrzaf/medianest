# The library format

A MediaNest library is a folder you choose. Everything MediaNest needs is
inside it, so a library can be moved to another disk or computer by moving
the folder.

```
<library>/
├── Media/                     managed photos and videos
│   ├── 2023/07/14/IMG_1234.HEIC
│   ├── 2023/07/14/IMG_1234.MOV
│   └── Unsorted/              items without a trustworthy date
└── .medianest/
    ├── library.json           format name, version and the library's UUID
    ├── catalog.sqlite3        the catalog (with -wal and -shm while open)
    ├── backups/               catalog backups, newest kept
    └── lock                   held while the library is open
```

## Media

Imported files are placed at `Media/YYYY/MM/DD/<original name>` using the
local date the photo was taken. A date counts as trustworthy when it comes
from the camera (EXIF, with or without a time zone), from a video's own
metadata, from a date in the file name, or from you. Files whose only date is
the file system's modification time go to `Media/Unsorted/` and are listed in
the review inbox; correcting the date in MediaNest does not move them, so
paths you may have shared stay valid.

If a different file already has the same name in a day folder, the new file
gets a short content hash before its extension, for example
`IMG_1234~3f9a2c1b.jpg`. Files are never overwritten.

Original files are copied unchanged, metadata included. MediaNest never
writes into them: rotation, dates, tags, ratings, captions and albums live
in the catalog. Use **Export** (or `medianest-cli export metadata`) to take
that information elsewhere as JSON.

## Linked folders

A linked folder is shown in the library without being copied. Its files are
cataloged in place; MediaNest notices new, changed, moved and missing files
while it runs, and touches them only when you delete an item and choose to
move the file to the trash. When a linked folder's disk is not connected,
its items stay in the library and show as unavailable.

## The catalog

`catalog.sqlite3` is a SQLite database in WAL mode with strict tables. It
records sources (the managed folder and linked folders), assets (one per
distinct photo or video) and files (one per place an asset is stored, with
size, inode, modification and change times and a BLAKE3 hash), plus tags,
albums, review items and a journal of file operations.

An asset is identified by the hash of its content. Importing a file whose
content is already in the library adds nothing; finding the same content in
a linked folder adds another location to the existing asset.

## Operations and recovery

Every change to files on disk is journaled in the catalog before it happens:

| State          | Meaning                                                        |
|----------------|----------------------------------------------------------------|
| Planned        | The operation is recorded; nothing has been written yet. A copy also records the name of its temporary `.medianest-<id>.part` file. |
| Verified       | The temporary copy was synced, re-read and matched the hash. A rename move records the file's hash and its new name instead. |
| Catalogued     | The catalog knows the new file. A copy or rename move is completed by the same commit. |
| AwaitingRemoval| A move could not remove the original and waits for you.         |
| Removing       | The original is being moved aside and checked once more.       |
| Completed      | Done.                                                          |

The states Copying and Installed also exist. Version 1.0.0 wrote them; they
are still read, and recovery treats them like the states around them. Newer
versions leave them out because the file system shows whether a temporary
copy was written or renamed into place, and each journal entry costs a
durable write per photo.

An operation has a kind: `CopyIn` (copy), `MoveIn` (copy, then remove the
original) or `RenameIn` (move by renaming). A move of a file on the library's
own filesystem is a rename: nothing is copied and no extra space is needed.
The file is hashed once before it is renamed and must still be the same file
(device, inode, size, modification time) afterwards. If the catalog cannot be
updated, the file is renamed back. A file with other hard links, or on another
filesystem, is copied and its original removed instead.

When a library opens, unfinished operations are reconciled with what is on
disk before anything else runs. Recovery only moves an operation forward
when the bytes on disk prove the journaled state (for example, an installed
file must hash to the expected value) and cleans up temporary files that
never became anything. Anything it
cannot decide goes to the review inbox. An interrupted rename is decided by
file identity: the original untouched means nothing happened, and the same
file at its new name means the rename completed. Only the original of a move
is ever removed, and only after the checks described above.

## Verification

MediaNest re-reads files periodically (every 30 days by default) and
compares them with the recorded hash. A file that no longer matches is
marked *changed* and listed for review; it is never replaced automatically.

## Backups

Backups contain the catalog only. They are written atomically into
`.medianest/backups/` once a day by default, and the newest fourteen are kept.
Restoring a backup saves the current catalog next to it first. Back up the
`Media/` folder itself with your usual backup tool.

## Thumbnails

Thumbnails and viewer previews are cached outside the library, in
`$XDG_CACHE_HOME/medianest/<library uuid>/`. They can be deleted at any time
and are made again as needed.
