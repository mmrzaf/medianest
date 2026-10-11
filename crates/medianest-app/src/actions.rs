//! Changes to photos that several screens offer, each with undo where it
//! makes sense.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use medianest_service::core::{AlbumId, AssetId, SourceKind};
use medianest_service::vault::LinkedPolicy;

use crate::context::Ctx;
use crate::i18n::{tr, trf, trn};
use crate::util;

fn count(ids: &[AssetId]) -> u64 {
    ids.len() as u64
}

pub fn set_favorite(ctx: &Rc<Ctx>, ids: Vec<AssetId>, favorite: bool) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        if let Err(error) = ctx
            .call(move |library| library.set_favorite(&change, favorite))
            .await
        {
            ctx.error(&tr("Could not change favorites"), &error);
            return;
        }
        if ids.len() > 1 {
            let message = if favorite {
                trn(
                    "{} item added to Favorites",
                    "{} items added to Favorites",
                    count(&ids),
                )
            } else {
                trn(
                    "{} item removed from Favorites",
                    "{} items removed from Favorites",
                    count(&ids),
                )
            };
            let undo_ctx = Rc::clone(&ctx);
            ctx.toast_undo(&message, move || set_favorite(&undo_ctx, ids, !favorite));
        }
    });
}

pub fn set_rating(ctx: &Rc<Ctx>, ids: Vec<AssetId>, rating: u8) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        if let Err(error) = ctx
            .call(move |library| library.set_rating(&ids, rating))
            .await
        {
            ctx.error(&tr("Could not change the rating"), &error);
        }
    });
}

pub fn rotate(ctx: &Rc<Ctx>, ids: Vec<AssetId>, degrees: i32) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        if let Err(error) = ctx
            .call(move |library| library.rotate(&change, degrees))
            .await
        {
            ctx.error(&tr("Could not rotate"), &error);
            return;
        }
        if ids.len() > 1 {
            let undo_ctx = Rc::clone(&ctx);
            ctx.toast_undo(
                &trn("{} item rotated", "{} items rotated", count(&ids)),
                move || rotate(&undo_ctx, ids, -degrees),
            );
        }
    });
}

pub fn set_hidden(ctx: &Rc<Ctx>, ids: Vec<AssetId>, hidden: bool) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        if let Err(error) = ctx
            .call(move |library| library.set_hidden(&change, hidden))
            .await
        {
            ctx.error(&tr("Could not change visibility"), &error);
            return;
        }
        let message = if hidden {
            trn(
                "{} item hidden. Search for “is:hidden” to see hidden items.",
                "{} items hidden. Search for “is:hidden” to see hidden items.",
                count(&ids),
            )
        } else {
            trn("{} item shown again", "{} items shown again", count(&ids))
        };
        let undo_ctx = Rc::clone(&ctx);
        ctx.toast_undo(&message, move || set_hidden(&undo_ctx, ids, !hidden));
    });
}

/// Moves items to the trash. Items whose only file is in a linked folder
/// ask whether that file should go to the trash too.
pub fn trash(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let check = ids.clone();
        let linked = ctx
            .call(move |library| {
                let mut linked = 0u64;
                for id in check {
                    let files = library.read(|db| db.files_of(id))?;
                    if !files.is_empty()
                        && files.iter().all(|f| f.source_kind == SourceKind::Linked)
                    {
                        linked += 1;
                    }
                }
                Ok(linked)
            })
            .await
            .unwrap_or(0);
        if linked == 0 {
            trash_with(&ctx, ids, LinkedPolicy::MoveToTrash);
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&trn(
                "Delete {} Item From a Linked Folder?",
                "Delete {} Items From Linked Folders?",
                linked,
            )),
            Some(&tr(
                "These files live in folders MediaNest watches but does not manage. You can move \
                 the files to the system trash, or only remove them from the library and leave \
                 the files where they are.",
            )),
        );
        dialog.add_responses(&[
            ("cancel", &tr("_Cancel")),
            ("remove", &tr("_Remove From Library")),
            ("trash", &tr("_Move Files to Trash")),
        ]);
        dialog.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("remove"));
        dialog.set_close_response("cancel");
        let response = dialog.choose_future(Some(&ctx.window)).await;
        match response.as_str() {
            "remove" => trash_with(&ctx, ids, LinkedPolicy::RemoveFromLibrary),
            "trash" => trash_with(&ctx, ids, LinkedPolicy::MoveToTrash),
            _ => {}
        }
    });
}

fn trash_with(ctx: &Rc<Ctx>, ids: Vec<AssetId>, policy: LinkedPolicy) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        match ctx
            .call(move |library| library.trash(&change, policy))
            .await
        {
            Ok(report) => {
                if let Some((name, reason)) = report.failed.first() {
                    ctx.error(&trf("Could not delete {}", &[name]), reason);
                }
                let trashed = u64::from(report.trashed);
                if trashed > 0 {
                    let undo_ctx = Rc::clone(&ctx);
                    ctx.toast_undo(
                        &trn(
                            "{} item moved to the trash",
                            "{} items moved to the trash",
                            trashed,
                        ),
                        move || restore(&undo_ctx, ids),
                    );
                } else if !report.forgotten.is_empty() {
                    ctx.toast(&trn(
                        "{} item removed from the library",
                        "{} items removed from the library",
                        report.forgotten.len() as u64,
                    ));
                }
            }
            Err(error) => ctx.error(&tr("Could not move to the trash"), &error),
        }
    });
}

pub fn restore(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        match ctx.call(move |library| library.restore(&ids)).await {
            Ok(report) => {
                if let Some((name, reason)) = report.failed.first() {
                    ctx.error(&trf("Could not restore {}", &[name]), reason);
                }
                if report.restored > 0 {
                    ctx.toast(&trn(
                        "{} item restored",
                        "{} items restored",
                        u64::from(report.restored),
                    ));
                }
            }
            Err(error) => ctx.error(&tr("Could not restore"), &error),
        }
    });
}

/// Removes trashed items from the library for good.
pub fn delete_forever(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let count = count(&ids);
    let inner = Rc::clone(ctx);
    ctx.confirm(
        &trn("Remove {} Item From the Library?", "Remove {} Items From the Library?", count),
        &tr("MediaNest will forget these items. Their files stay in the system trash until it is emptied."),
        &tr("_Remove"),
        true,
        move || {
            let ctx = inner;
            util::spawn(async move {
                match ctx.call(move |library| library.forget_trashed(&ids)).await {
                    Ok(n) => ctx.toast(&trn("{} item removed", "{} items removed", n as u64)),
                    Err(error) => ctx.error(&tr("Could not remove"), &error),
                }
            });
        },
    );
}

pub fn add_to_album(ctx: &Rc<Ctx>, album: AlbumId, name: String, ids: Vec<AssetId>) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        match ctx
            .call(move |library| library.add_to_album(album, &change))
            .await
        {
            Ok(added) => {
                let undo_ctx = Rc::clone(&ctx);
                let message = trf("{} added to “{}”", &[&util::items(added as u64), &name]);
                ctx.toast_undo(&message, move || {
                    remove_from_album(&undo_ctx, album, ids, false)
                });
            }
            Err(error) => ctx.error(&tr("Could not add to the album"), &error),
        }
    });
}

pub fn remove_from_album(ctx: &Rc<Ctx>, album: AlbumId, ids: Vec<AssetId>, offer_undo: bool) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let change = ids.clone();
        if let Err(error) = ctx
            .call(move |library| library.remove_from_album(album, &change))
            .await
        {
            ctx.error(&tr("Could not remove from the album"), &error);
            return;
        }
        if offer_undo {
            let undo_ctx = Rc::clone(&ctx);
            ctx.toast_undo(
                &trn(
                    "{} item removed from the album",
                    "{} items removed from the album",
                    count(&ids),
                ),
                move || {
                    let ctx = undo_ctx;
                    util::spawn(async move {
                        let _ = ctx
                            .call(move |library| library.add_to_album(album, &ids))
                            .await;
                    });
                },
            );
        }
    });
}

pub fn set_cover(ctx: &Rc<Ctx>, album: AlbumId, id: AssetId) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        match ctx
            .call(move |library| library.set_album_cover(album, Some(id)))
            .await
        {
            Ok(()) => ctx.toast(&tr("Album cover changed")),
            Err(error) => ctx.error(&tr("Could not change the cover"), &error),
        }
    });
}

/// Shows the file in the file manager, selected.
pub fn show_in_files(ctx: &Rc<Ctx>, id: AssetId) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let path = match ctx.call(move |library| library.original_path(id)).await {
            Ok(path) => path,
            Err(error) => {
                ctx.error(&tr("Could not show the file"), &error);
                return;
            }
        };
        let file = gio::File::for_path(&path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        if let Err(error) = launcher
            .open_containing_folder_future(Some(&ctx.window))
            .await
        {
            if !error.matches(gtk::DialogError::Dismissed) {
                ctx.error(&tr("Could not show the file"), &error.to_string());
            }
        }
    });
}

/// Opens the original in another application chosen by the user.
pub fn open_with(ctx: &Rc<Ctx>, id: AssetId) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let path = match ctx.call(move |library| library.original_path(id)).await {
            Ok(path) => path,
            Err(error) => {
                ctx.error(&tr("Could not open the file"), &error);
                return;
            }
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        launcher.set_always_ask(true);
        if let Err(error) = launcher.launch_future(Some(&ctx.window)).await {
            if !error.matches(gtk::DialogError::Dismissed) {
                ctx.error(&tr("Could not open the file"), &error.to_string());
            }
        }
    });
}

/// Puts the original files on the clipboard, to paste into a file manager
/// or a chat.
pub fn copy_files(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        let paths = ctx
            .call(move |library| {
                Ok(ids
                    .iter()
                    .filter_map(|id| library.original_path(*id).ok())
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap_or_default();
        if paths.is_empty() {
            ctx.toast(&tr("The files are not available"));
            return;
        }
        let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
        let list = gdk::FileList::from_array(&files);
        let uris = files.iter().fold(String::new(), |mut uris, file| {
            uris.push_str(&file.uri());
            uris.push_str("\r\n");
            uris
        });
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_value(&list.to_value()),
            gdk::ContentProvider::for_bytes(
                "text/uri-list",
                &glib::Bytes::from_owned(uris.into_bytes()),
            ),
        ]);
        if ctx.window.clipboard().set_content(Some(&provider)).is_ok() {
            ctx.toast(&trn(
                "{} file copied",
                "{} files copied",
                files.len() as u64,
            ));
        }
    });
}

/// Originals for dragging out of the grid. Blocking, but each lookup is
/// a single indexed query; capped to keep drags responsive.
pub fn drag_paths(ctx: &Ctx, ids: &[AssetId]) -> Vec<std::path::PathBuf> {
    ids.iter()
        .take(500)
        .filter_map(|id| ctx.library.original_path(*id).ok())
        .collect()
}
