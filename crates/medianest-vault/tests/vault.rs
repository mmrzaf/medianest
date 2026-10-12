#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use medianest_catalog::{Writer, flags};
use medianest_core::{FileStatus, OperationState, OperationUuid, ReviewKind, SourceKind};
use medianest_media::MediaToolkit;
use medianest_vault::{
    Checkpoint, Control, CrashAt, CrossDevice, ImportMode, ImportOutcome, Layout, LinkedPolicy,
    RecoveryReport, Vault, VaultError,
};

struct Library {
    _lock: medianest_vault::LibraryLock,
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    layout: Layout,
    writer: Mutex<Writer>,
    toolkit: MediaToolkit,
}

impl Library {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("Library");
        let outside = dir.path().join("Camera");
        fs::create_dir_all(&outside).expect("outside");
        let opened = medianest_vault::create(&root).expect("create");
        Self {
            _lock: opened.lock,
            _dir: dir,
            root,
            outside,
            layout: opened.layout,
            writer: Mutex::new(opened.writer),
            toolkit: MediaToolkit::in_process(),
        }
    }

    fn vault(&self) -> Vault<'_> {
        Vault::new(&self.layout, &self.writer)
    }

    fn photo(&self, name: &str, seed: u8) -> PathBuf {
        let path = self.outside.join(name);
        write_photo(&path, seed);
        path
    }

    /// A photo with a second hard link next to it. A move cannot rename such a
    /// file into the library, so it is copied and its original removed.
    fn linked_photo(&self, name: &str, seed: u8) -> PathBuf {
        let path = self.photo(name, seed);
        fs::hard_link(&path, self.outside.join(format!("{name}.link"))).expect("link");
        path
    }

    fn import(&self, path: &Path, mode: ImportMode) -> medianest_vault::Result<ImportOutcome> {
        let metadata = self.toolkit.probe(path).expect("probe");
        self.vault()
            .import_file(path, &metadata, mode, Some("test"), &Control::new())
    }

    fn managed_files(&self) -> Vec<PathBuf> {
        walk(&self.layout.media())
    }

    fn probe_fn(
        &self,
    ) -> impl Fn(&Path) -> Result<medianest_core::AssetMetadata, medianest_media::ProbeError> + Sync + '_
    {
        move |path: &Path| self.toolkit.probe(path)
    }
}

fn write_photo(path: &Path, seed: u8) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("dir");
    }
    let image = image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([seed.wrapping_add(x as u8), y as u8, seed.wrapping_mul(3)])
    });
    image
        .save_with_format(path, image::ImageFormat::Png)
        .expect("save");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn copy_import_installs_a_verified_copy_and_keeps_the_original() {
    let lib = Library::new();
    let original = lib.photo("IMG_20230701_101500.png", 1);
    let bytes = fs::read(&original).expect("read");
    let outcome = lib.import(&original, ImportMode::Copy).expect("import");
    let ImportOutcome::Imported { relpath, asset, .. } = outcome else {
        panic!("expected a new import, got {outcome:?}");
    };
    assert_eq!(relpath.display(), "2023/07/01/IMG_20230701_101500.png");
    assert_eq!(
        fs::read(&original).expect("original"),
        bytes,
        "original untouched"
    );
    assert_eq!(
        fs::read(lib.layout.media().join(relpath.as_path())).expect("copy"),
        bytes
    );
    let writer = lib.writer.lock().unwrap();
    let files = writer.files_of(asset).expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file.status, FileStatus::Verified);
    assert_eq!(files[0].source_kind, SourceKind::Managed);
    assert!(writer.unfinished_operations().expect("ops").is_empty());
}

#[test]
fn guessed_dates_go_to_unsorted() {
    let lib = Library::new();
    let original = lib.photo("photo.png", 2);
    let ImportOutcome::Imported { relpath, .. } =
        lib.import(&original, ImportMode::Copy).expect("import")
    else {
        panic!("expected import");
    };
    assert_eq!(relpath.display(), "Unsorted/photo.png");
}

#[test]
fn identical_content_is_imported_only_once() {
    let lib = Library::new();
    let first = lib.photo("a.png", 3);
    let second = lib.outside.join("copy-of-a.png");
    fs::copy(&first, &second).expect("copy");
    lib.import(&first, ImportMode::Copy).expect("first");
    let outcome = lib.import(&second, ImportMode::Copy).expect("second");
    assert!(matches!(outcome, ImportOutcome::AlreadyInLibrary { .. }));
    assert_eq!(lib.managed_files().len(), 1);
}

#[test]
fn different_files_with_the_same_name_both_survive() {
    let lib = Library::new();
    let first = lib.photo("one/IMG_1.png", 4);
    let second = lib.photo("two/IMG_1.png", 5);
    lib.import(&first, ImportMode::Copy).expect("first");
    let ImportOutcome::Imported { relpath, .. } =
        lib.import(&second, ImportMode::Copy).expect("second")
    else {
        panic!("expected import");
    };
    assert!(
        relpath.display().starts_with("Unsorted/IMG_1~"),
        "{}",
        relpath.display()
    );
    assert_eq!(lib.managed_files().len(), 2);
}

/// Crashes a move after the copy is catalogued, then lets `tamper` change the
/// original before recovery tries to remove it.
fn stage_move(lib: &Library, original: &Path, tamper: impl FnOnce()) -> RecoveryReport {
    let metadata = lib.toolkit.probe(original).expect("probe");
    let crashing = Vault {
        layout: &lib.layout,
        catalog: &lib.writer,
        faults: &CrashAt(Checkpoint::AfterCatalogued),
    };
    let result = crashing.import_file(original, &metadata, ImportMode::Move, None, &Control::new());
    assert!(
        matches!(result, Err(VaultError::Interrupted(_))),
        "{result:?}"
    );
    tamper();
    lib.vault().recover(&lib.probe_fn()).expect("recover")
}

fn assert_original_kept_for_review(lib: &Library, original: &Path) -> OperationUuid {
    assert!(original.exists(), "the original is kept");
    let writer = lib.writer.lock().unwrap();
    assert_eq!(
        writer
            .reviews(Some(ReviewKind::PendingRemoval), 10)
            .expect("reviews")
            .len(),
        1
    );
    let unfinished = writer.unfinished_operations().expect("ops");
    assert_eq!(unfinished.len(), 1);
    assert_eq!(unfinished[0].state, OperationState::AwaitingRemoval);
    unfinished[0].uuid
}

#[test]
fn moves_rename_the_original_into_the_library() {
    use std::os::unix::fs::MetadataExt;
    let lib = Library::new();
    let original = lib.photo("rename.png", 6);
    let inode = fs::metadata(&original).expect("stat").ino();
    let bytes = fs::read(&original).expect("read");
    let outcome = lib.import(&original, ImportMode::Move).expect("import");
    let ImportOutcome::Imported {
        awaiting_removal: None,
        ..
    } = outcome
    else {
        panic!("expected a finished move, got {outcome:?}");
    };
    assert!(!original.exists());
    let files = lib.managed_files();
    assert_eq!(files.len(), 1);
    assert_eq!(fs::metadata(&files[0]).expect("stat").ino(), inode);
    assert_eq!(fs::read(&files[0]).expect("read"), bytes);
    let writer = lib.writer.lock().unwrap();
    assert_eq!(writer.stats().expect("stats").managed_files, 1);
    assert!(writer.reviews(None, 10).expect("reviews").is_empty());
    assert!(writer.unfinished_operations().expect("ops").is_empty());
    drop(writer);
    assert!(walk(&lib.outside).is_empty(), "nothing is left behind");
}

#[test]
fn moves_copy_originals_that_have_other_links() {
    use std::os::unix::fs::MetadataExt;
    let lib = Library::new();
    let original = lib.linked_photo("linked.png", 16);
    let inode = fs::metadata(&original).expect("stat").ino();
    let bytes = fs::read(&original).expect("read");
    lib.import(&original, ImportMode::Move).expect("import");
    assert!(!original.exists());
    let files = lib.managed_files();
    assert_eq!(files.len(), 1);
    assert_ne!(
        fs::metadata(&files[0]).expect("stat").ino(),
        inode,
        "the library file must not share its content with a file outside"
    );
    assert_eq!(fs::read(&files[0]).expect("read"), bytes);
    assert_eq!(
        fs::read(lib.outside.join("linked.png.link")).expect("other link"),
        bytes
    );
    assert!(
        lib.writer
            .lock()
            .unwrap()
            .unfinished_operations()
            .expect("ops")
            .is_empty()
    );
}

#[test]
fn moves_fall_back_to_copying_when_the_rename_crosses_filesystems() {
    use std::os::unix::fs::MetadataExt;
    let lib = Library::new();
    let original = lib.photo("far.png", 17);
    let inode = fs::metadata(&original).expect("stat").ino();
    let bytes = fs::read(&original).expect("read");
    let metadata = lib.toolkit.probe(&original).expect("probe");
    let vault = Vault {
        layout: &lib.layout,
        catalog: &lib.writer,
        faults: &CrossDevice,
    };
    let outcome = vault
        .import_file(
            &original,
            &metadata,
            ImportMode::Move,
            None,
            &Control::new(),
        )
        .expect("import");
    assert!(matches!(
        outcome,
        ImportOutcome::Imported {
            awaiting_removal: None,
            ..
        }
    ));
    assert!(!original.exists());
    let files = lib.managed_files();
    assert_eq!(files.len(), 1);
    assert_ne!(fs::metadata(&files[0]).expect("stat").ino(), inode);
    assert_eq!(fs::read(&files[0]).expect("read"), bytes);
    let writer = lib.writer.lock().unwrap();
    assert!(writer.unfinished_operations().expect("ops").is_empty());
    assert!(writer.reviews(None, 10).expect("reviews").is_empty());
}

#[test]
fn recovery_finishes_a_rename_that_stopped_between_link_and_unlink() {
    let lib = Library::new();
    let original = lib.photo("both.png", 18);
    let bytes = fs::read(&original).expect("read");
    let metadata = lib.toolkit.probe(&original).expect("probe");
    let crashing = Vault {
        layout: &lib.layout,
        catalog: &lib.writer,
        faults: &CrashAt(Checkpoint::AfterRename),
    };
    crashing
        .import_file(
            &original,
            &metadata,
            ImportMode::Move,
            None,
            &Control::new(),
        )
        .expect_err("crash");
    // Rebuild what a link-then-unlink rename leaves when it stops in between:
    // the destination exists and the original has its old name back.
    let placed = lib.managed_files().remove(0);
    fs::hard_link(&placed, &original).expect("second name");
    let report = lib.vault().recover(&lib.probe_fn()).expect("recover");
    assert_eq!(report.completed, 1);
    assert!(!original.exists(), "the leftover name is removed");
    let files = lib.managed_files();
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).expect("read"), bytes);
    assert_eq!(
        lib.writer
            .lock()
            .unwrap()
            .stats()
            .expect("stats")
            .managed_files,
        1
    );
}

#[test]
fn moving_a_duplicate_removes_the_original_too() {
    let lib = Library::new();
    let first = lib.photo("first.png", 15);
    lib.import(&first, ImportMode::Copy).expect("copy");
    let second = lib.photo("second.png", 15);
    let outcome = lib.import(&second, ImportMode::Move).expect("move");
    assert!(matches!(
        outcome,
        ImportOutcome::AlreadyInLibrary {
            awaiting_removal: None,
            ..
        }
    ));
    assert!(!second.exists());
    assert!(first.exists());
    assert_eq!(lib.managed_files().len(), 1);
}

#[test]
fn a_replaced_original_is_kept_even_with_identical_bytes() {
    let lib = Library::new();
    let original = lib.linked_photo("replace.png", 7);
    let report = stage_move(&lib, &original, || {
        let bytes = fs::read(&original).expect("read");
        fs::remove_file(&original).expect("remove");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&original, &bytes).expect("recreate");
    });
    assert_eq!(report.awaiting_removal, 1);
    assert_original_kept_for_review(&lib, &original);
}

#[test]
fn an_edited_original_is_kept_until_the_user_decides() {
    let lib = Library::new();
    let original = lib.linked_photo("edit.png", 8);
    let report = stage_move(&lib, &original, || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_photo(&original, 9);
    });
    assert_eq!(report.awaiting_removal, 1);
    let operation = assert_original_kept_for_review(&lib, &original);
    assert!(
        lib.vault()
            .approve_removal(operation, &Control::new())
            .is_err()
    );
    assert!(original.exists());
    lib.vault().keep_original(operation).expect("keep");
    assert!(
        lib.writer
            .lock()
            .unwrap()
            .reviews(None, 10)
            .expect("reviews")
            .is_empty()
    );
}

#[test]
fn imports_refuse_symlinks_and_files_inside_the_library() {
    let lib = Library::new();
    let real = lib.photo("real.png", 10);
    let link = lib.outside.join("link.png");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let metadata = lib.toolkit.probe(&real).expect("probe");
    let result = lib
        .vault()
        .import_file(&link, &metadata, ImportMode::Copy, None, &Control::new());
    assert!(matches!(result, Err(VaultError::Symlink(_))));
    let inside = lib.root.join("inside.png");
    write_photo(&inside, 11);
    let result =
        lib.vault()
            .import_file(&inside, &metadata, ImportMode::Copy, None, &Control::new());
    assert!(matches!(result, Err(VaultError::InsideLibrary(_))));
}

#[test]
fn cancelled_imports_leave_nothing_behind() {
    let lib = Library::new();
    let original = lib.photo("cancel.png", 12);
    let metadata = lib.toolkit.probe(&original).expect("probe");
    let control = Control::new();
    control.cancel();
    let result = lib
        .vault()
        .import_file(&original, &metadata, ImportMode::Copy, None, &control);
    assert!(matches!(result, Err(VaultError::Cancelled)));
    assert!(lib.managed_files().is_empty());
}

#[test]
fn recovery_after_a_crash_at_any_checkpoint_keeps_the_invariant() {
    // A copy, a rename move, and a move that has to copy because the
    // original has another hard link.
    let mut reached = std::collections::HashSet::new();
    for (mode, linked) in [
        (ImportMode::Copy, false),
        (ImportMode::Move, false),
        (ImportMode::Move, true),
    ] {
        for &checkpoint in Checkpoint::ALL {
            let lib = Library::new();
            let original = if linked {
                lib.linked_photo("crash.png", 13)
            } else {
                lib.photo("crash.png", 13)
            };
            let bytes = fs::read(&original).expect("read");
            let metadata = lib.toolkit.probe(&original).expect("probe");
            let crashing = Vault {
                layout: &lib.layout,
                catalog: &lib.writer,
                faults: &CrashAt(checkpoint),
            };
            let result = crashing.import_file(&original, &metadata, mode, None, &Control::new());
            if result.is_ok() {
                // This kind of import never passes through the checkpoint.
                continue;
            }
            assert!(
                matches!(result, Err(VaultError::Interrupted(_))),
                "{checkpoint:?}: {result:?}"
            );
            reached.insert(checkpoint);

            let probe = lib.probe_fn();
            let first = lib.vault().recover(&probe).expect("recover");
            let second = lib.vault().recover(&probe).expect("recover again");
            assert!(
                second.completed == 0 && second.rolled_back == 0,
                "{checkpoint:?}: recovery is idempotent"
            );

            let files = lib.managed_files();
            if original.exists() {
                assert_eq!(
                    fs::read(&original).expect("original survives"),
                    bytes,
                    "{mode:?} {checkpoint:?}"
                );
            } else {
                assert_eq!(mode, ImportMode::Move, "{checkpoint:?}");
                assert_eq!(
                    files.len(),
                    1,
                    "{checkpoint:?}: a removed original has a copy"
                );
                assert_eq!(fs::read(&files[0]).expect("copy"), bytes, "{checkpoint:?}");
            }
            assert!(
                files.iter().all(|path| !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with(".part")),
                "{checkpoint:?}: no temporary files remain: {files:?}"
            );
            let writer = lib.writer.lock().unwrap();
            let stats = writer.stats().expect("stats");
            match files.len() {
                0 => assert_eq!(stats.assets, 0, "{checkpoint:?}"),
                1 => {
                    assert_eq!(fs::read(&files[0]).expect("copy"), bytes, "{checkpoint:?}");
                    assert_eq!(
                        stats.managed_files, 1,
                        "{checkpoint:?}: the copy is catalogued"
                    );
                }
                n => panic!("{checkpoint:?}: {n} managed files"),
            }
            let unfinished = writer.unfinished_operations().expect("ops");
            assert!(
                unfinished.is_empty(),
                "{mode:?} {checkpoint:?}: {unfinished:?}"
            );
            drop(writer);
            let _ = first;
        }
    }
    assert_eq!(
        reached.len(),
        Checkpoint::ALL.len(),
        "every checkpoint is exercised by some kind of import"
    );
}

/// Crashes a copy at `checkpoint` and rewrites its journal row the way
/// version 1.0.0 would have left it, in `state`.
fn crash_with_legacy_state(
    lib: &Library,
    original: &Path,
    checkpoint: Checkpoint,
    state: OperationState,
) {
    let metadata = lib.toolkit.probe(original).expect("probe");
    let crashing = Vault {
        layout: &lib.layout,
        catalog: &lib.writer,
        faults: &CrashAt(checkpoint),
    };
    crashing
        .import_file(original, &metadata, ImportMode::Copy, None, &Control::new())
        .expect_err("crash");
    let mut writer = lib.writer.lock().unwrap();
    // A copy that reached the catalog is already completed in the new journal.
    let mut operation = [
        OperationState::Planned,
        OperationState::Verified,
        OperationState::Completed,
    ]
    .into_iter()
    .find_map(|state| {
        writer
            .operations_in_state(state)
            .expect("ops")
            .into_iter()
            .next()
    })
    .expect("the interrupted operation");
    operation.state = state;
    writer.save_operation(&operation).expect("save");
}

#[test]
fn journals_written_by_1_0_0_still_recover() {
    // Interrupted while writing the temporary copy: rolled back, original intact.
    let lib = Library::new();
    let original = lib.photo("legacy-a.png", 21);
    crash_with_legacy_state(
        &lib,
        &original,
        Checkpoint::AfterTempCreated,
        OperationState::Copying,
    );
    let report = lib.vault().recover(&lib.probe_fn()).expect("recover");
    assert_eq!(report.rolled_back, 1);
    assert!(original.exists());
    assert!(lib.managed_files().is_empty(), "the temporary copy is gone");

    // Renamed into place but not yet catalogued.
    let lib = Library::new();
    let original = lib.photo("legacy-b.png", 22);
    crash_with_legacy_state(
        &lib,
        &original,
        Checkpoint::AfterInstall,
        OperationState::Installed,
    );
    let report = lib.vault().recover(&lib.probe_fn()).expect("recover");
    assert_eq!(report.completed, 1);
    assert_eq!(lib.managed_files().len(), 1);
    assert_eq!(
        lib.writer
            .lock()
            .unwrap()
            .stats()
            .expect("stats")
            .managed_files,
        1
    );

    // Catalogued but never marked completed.
    let lib = Library::new();
    let original = lib.photo("legacy-c.png", 23);
    crash_with_legacy_state(
        &lib,
        &original,
        Checkpoint::AfterCatalogued,
        OperationState::Catalogued,
    );
    let report = lib.vault().recover(&lib.probe_fn()).expect("recover");
    assert_eq!(report.completed, 1);
    assert!(original.exists(), "a copy never touches its original");
    assert!(
        lib.writer
            .lock()
            .unwrap()
            .unfinished_operations()
            .expect("ops")
            .is_empty()
    );
}

#[test]
fn recovery_finishes_a_removal_interrupted_after_the_rename() {
    let lib = Library::new();
    let original = lib.linked_photo("aside.png", 14);
    let metadata = lib.toolkit.probe(&original).expect("probe");
    let crashing = Vault {
        layout: &lib.layout,
        catalog: &lib.writer,
        faults: &CrashAt(Checkpoint::AfterAside),
    };
    let result = crashing.import_file(
        &original,
        &metadata,
        ImportMode::Move,
        None,
        &Control::new(),
    );
    assert!(
        matches!(result, Err(VaultError::Interrupted(_))),
        "{result:?}"
    );
    assert!(!original.exists());
    assert_eq!(
        walk(&lib.outside).len(),
        2,
        "the original is set aside next to its other link, not yet removed"
    );
    let probe = lib.probe_fn();
    let report = lib.vault().recover(&probe).expect("recover");
    assert_eq!(report.completed, 1);
    assert_eq!(walk(&lib.outside), vec![lib.outside.join("aside.png.link")]);
    assert_eq!(lib.managed_files().len(), 1);
}

#[test]
fn scans_track_additions_edits_moves_and_disappearances() {
    let lib = Library::new();
    let folder = lib.outside.join("Pictures");
    write_photo(&folder.join("2023/a.png"), 20);
    write_photo(&folder.join("2023/b.png"), 21);
    write_photo(&folder.join("2023/c.png"), 22);
    fs::write(folder.join("notes.txt"), b"not media").expect("write");
    write_photo(&folder.join(".thumbnails/hidden.png"), 23);
    let vault = lib.vault();
    let source = vault.add_source(&folder).expect("add");
    let probe = lib.probe_fn();
    let report = vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");
    assert_eq!((report.found, report.added), (3, 3));

    let unchanged = vault
        .scan_source(source, &probe, &Control::new())
        .expect("rescan");
    assert_eq!(unchanged.unchanged, 3);

    // Tag b, then rename it: the tag must survive the move.
    let b_asset = {
        let writer = lib.writer.lock().unwrap();
        let relpath = medianest_core::PathBytes::from_vec(b"2023/b.png".to_vec());
        writer
            .file_by_path(source, &relpath)
            .expect("lookup")
            .expect("b")
            .asset_id
    };
    lib.writer
        .lock()
        .unwrap()
        .add_tag(&[b_asset], "keeper")
        .expect("tag");
    fs::rename(folder.join("2023/b.png"), folder.join("2023/b-renamed.png")).expect("rename");
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_photo(&folder.join("2023/a.png"), 24);
    fs::remove_file(folder.join("2023/c.png")).expect("delete");

    let report = vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan after changes");
    assert_eq!(
        (report.moved, report.modified, report.missing, report.added),
        (1, 1, 1, 0),
        "{report:?}"
    );
    let writer = lib.writer.lock().unwrap();
    assert_eq!(writer.tags_of(b_asset).expect("tags")[0].name, "keeper");
    assert_eq!(
        writer
            .reviews(Some(ReviewKind::MissingFile), 10)
            .expect("reviews")
            .len(),
        1
    );
}

#[test]
fn unplugged_or_empty_sources_are_offline_not_missing() {
    let lib = Library::new();
    let folder = lib.outside.join("Drive");
    write_photo(&folder.join("a.png"), 30);
    let vault = lib.vault();
    let source = vault.add_source(&folder).expect("add");
    let probe = lib.probe_fn();
    vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");

    fs::remove_file(folder.join("a.png")).expect("empty the mount point");
    let report = vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");
    assert!(report.offline);
    let renamed = lib.outside.join("Drive-elsewhere");
    fs::rename(&folder, &renamed).expect("unplug");
    let report = vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");
    assert!(report.offline);
    assert_eq!(
        lib.writer
            .lock()
            .unwrap()
            .stats()
            .expect("stats")
            .missing_files,
        0
    );
}

#[test]
fn sources_cannot_contain_or_be_inside_the_library() {
    let lib = Library::new();
    let vault = lib.vault();
    assert!(matches!(
        vault.add_source(&lib.layout.media()),
        Err(VaultError::InsideLibrary(_))
    ));
    let parent = lib.root.parent().expect("parent").to_path_buf();
    assert!(matches!(
        vault.add_source(&parent),
        Err(VaultError::ContainsLibrary(_))
    ));
}

#[test]
fn verification_detects_corruption_and_merges_linked_duplicates() {
    let lib = Library::new();
    let original = lib.photo("verify.png", 40);
    let ImportOutcome::Imported { asset, relpath, .. } =
        lib.import(&original, ImportMode::Copy).expect("import")
    else {
        panic!("expected import");
    };
    // A linked folder holds the same picture: hashing reveals it is one asset.
    let folder = lib.outside.join("Linked");
    fs::create_dir_all(&folder).expect("dir");
    fs::copy(&original, folder.join("same.png")).expect("copy");
    let vault = lib.vault();
    let source = vault.add_source(&folder).expect("add");
    let probe = lib.probe_fn();
    vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");
    assert_eq!(lib.writer.lock().unwrap().stats().expect("stats").assets, 2);
    let unverified = lib
        .writer
        .lock()
        .unwrap()
        .files_with_status(FileStatus::Unverified, medianest_core::FileId(0), 10)
        .expect("files");
    let outcome = vault
        .verify_file(&unverified[0], &Control::new())
        .expect("verify");
    assert!(
        matches!(outcome, medianest_vault::VerifyOutcome::Merged { .. }),
        "{outcome:?}"
    );
    assert_eq!(lib.writer.lock().unwrap().stats().expect("stats").assets, 1);
    assert_eq!(
        lib.writer
            .lock()
            .unwrap()
            .files_of(asset)
            .expect("files")
            .len(),
        2
    );

    // Corrupt the managed copy.
    let managed = lib.layout.media().join(relpath.as_path());
    let mut bytes = fs::read(&managed).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&managed, bytes).expect("corrupt");
    let location = lib
        .writer
        .lock()
        .unwrap()
        .files_of(asset)
        .expect("files")
        .into_iter()
        .find(|location| location.source_kind == SourceKind::Managed)
        .expect("managed");
    let outcome = vault
        .verify_file(&location, &Control::new())
        .expect("verify");
    assert_eq!(outcome, medianest_vault::VerifyOutcome::Changed);
    let items = lib
        .writer
        .lock()
        .unwrap()
        .timeline(&medianest_catalog::View::library())
        .expect("timeline");
    assert!(items[0].has(flags::CHANGED));
}

#[test]
fn backups_restore_the_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("Library");
    let camera = dir.path().join("Camera");
    let toolkit = MediaToolkit::in_process();
    let import = |vault: &Vault<'_>, name: &str, seed: u8| {
        let path = camera.join(name);
        write_photo(&path, seed);
        let metadata = toolkit.probe(&path).expect("probe");
        vault
            .import_file(&path, &metadata, ImportMode::Copy, None, &Control::new())
            .expect("import");
    };
    let backup = {
        let opened = medianest_vault::create(&root).expect("create");
        let writer = Mutex::new(opened.writer);
        let vault = Vault::new(&opened.layout, &writer);
        import(&vault, "x.png", 50);
        let backup = medianest_vault::create_backup(&writer.lock().unwrap(), &opened.layout)
            .expect("backup");
        import(&vault, "y.png", 51);
        assert_eq!(
            medianest_vault::list_backups(&opened.layout)
                .expect("list")
                .len(),
            1
        );
        backup
    };
    let layout = Layout::new(root.canonicalize().expect("root"));
    medianest_vault::restore_backup(&layout, &backup).expect("restore");
    let reopened = medianest_vault::open(&root).expect("open");
    assert_eq!(reopened.writer.stats().expect("stats").assets, 1);
}

#[test]
fn trash_and_restore_round_trip() {
    // The trash location comes from the environment, so run the real test in
    // a child process with a private data folder.
    let home = tempfile::tempdir().expect("home");
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--ignored",
            "--exact",
            "trash_round_trip_in_private_home",
            "--test-threads=1",
        ])
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("HOME", home.path())
        .status()
        .expect("run");
    assert!(status.success());
}

#[test]
#[ignore = "run by trash_and_restore_round_trip with a private data folder"]
fn trash_round_trip_in_private_home() {
    assert!(std::env::var_os("XDG_DATA_HOME").is_some());
    let lib = Library::new();
    let original = lib.photo("trash-me.png", 60);
    let ImportOutcome::Imported { asset, relpath, .. } =
        lib.import(&original, ImportMode::Copy).expect("import")
    else {
        panic!("expected import");
    };
    let managed = lib.layout.media().join(relpath.as_path());
    let report = lib
        .vault()
        .trash_assets(&[asset], LinkedPolicy::MoveToTrash)
        .expect("trash");
    assert_eq!(report.trashed, 1, "{report:?}");
    assert!(!managed.exists());
    assert!(
        lib.writer
            .lock()
            .unwrap()
            .asset(asset)
            .expect("asset")
            .trashed_ms
            .is_some()
    );
    let report = lib.vault().restore_assets(&[asset]).expect("restore");
    assert_eq!(report.restored, 1, "{report:?}");
    assert!(managed.exists());
    assert!(
        lib.writer
            .lock()
            .unwrap()
            .asset(asset)
            .expect("asset")
            .trashed_ms
            .is_none()
    );

    // Removing a linked file from the library leaves it on disk and out of rescans.
    let folder = lib.outside.join("Linked");
    write_photo(&folder.join("keep-on-disk.png"), 61);
    let vault = lib.vault();
    let source = vault.add_source(&folder).expect("add");
    let probe = lib.probe_fn();
    vault
        .scan_source(source, &probe, &Control::new())
        .expect("scan");
    let linked = {
        let writer = lib.writer.lock().unwrap();
        let relpath = medianest_core::PathBytes::from_vec(b"keep-on-disk.png".to_vec());
        writer
            .file_by_path(source, &relpath)
            .expect("lookup")
            .expect("file")
            .asset_id
    };
    let report = vault
        .trash_assets(&[linked], LinkedPolicy::RemoveFromLibrary)
        .expect("remove");
    assert_eq!(report.forgotten.len(), 1);
    assert!(folder.join("keep-on-disk.png").exists());
    let rescan = vault
        .scan_source(source, &probe, &Control::new())
        .expect("rescan");
    assert_eq!(rescan.added, 0, "excluded files stay out");
}
