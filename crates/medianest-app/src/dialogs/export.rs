//! Copying originals out of the library.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use medianest_service::ExportOptions;
use medianest_service::core::AssetId;

use crate::context::Ctx;
use crate::i18n::{tr, trn};

pub fn show(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let count = ids.len() as u64;
    let destination = Rc::new(RefCell::new(
        glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(glib::home_dir),
    ));

    let folder_row = adw::ActionRow::builder()
        .title(tr("Destination"))
        .subtitle(glib::markup_escape_text(
            &destination.borrow().display().to_string(),
        ))
        .activatable(true)
        .build();
    folder_row.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    let by_date = adw::SwitchRow::builder()
        .title(tr("Organize by Date"))
        .subtitle(tr("Put files into Year/Month folders"))
        .build();
    let motion = adw::SwitchRow::builder()
        .title(tr("Include Live Photo Videos"))
        .active(true)
        .build();
    let group = adw::PreferencesGroup::builder()
        .description(tr(
            "Originals are copied unchanged, with their metadata. Files with the same name are \
             never overwritten.",
        ))
        .build();
    group.add(&folder_row);
    group.add(&by_date);
    group.add(&motion);

    let export = gtk::Button::builder()
        .label(tr("_Export"))
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::builder()
        .label(tr("_Cancel"))
        .use_underline(true)
        .build();
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&export);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(trn("Export {} Item", "Export {} Items", count))
        .content_width(440)
        .child(&toolbar)
        .build();

    {
        let destination = Rc::clone(&destination);
        let window = ctx.window.clone();
        folder_row.connect_activated(move |row| {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Export To"))
                .initial_folder(&gio::File::for_path(&*destination.borrow()))
                .build();
            let destination = Rc::clone(&destination);
            let row = row.clone();
            chooser.select_folder(Some(&window), gio::Cancellable::NONE, move |chosen| {
                if let Some(path) = chosen.ok().and_then(|file| file.path()) {
                    row.set_subtitle(&glib::markup_escape_text(&path.display().to_string()));
                    *destination.borrow_mut() = path;
                }
            });
        });
    }
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    let ctx_ref = Rc::clone(ctx);
    let dialog_ref = dialog.clone();
    export.connect_clicked(move |_| {
        dialog_ref.close();
        let options = ExportOptions {
            destination: PathBuf::from(&*destination.borrow()),
            folders_by_date: by_date.is_active(),
            include_motion: motion.is_active(),
        };
        // Progress shows in the activity area; a toast reports the result.
        ctx_ref.library.export_files(ids.clone(), options);
    });
    dialog.present(Some(&ctx.window));
}
