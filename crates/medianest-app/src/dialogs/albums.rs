//! Adding items to an album, or to a new one.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::core::AssetId;

use crate::actions;
use crate::context::Ctx;
use crate::i18n::{tr, trn};
use crate::util;

pub fn show(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let count = ids.len() as u64;
    let new_album = adw::EntryRow::builder()
        .title(tr("New Album"))
        .show_apply_button(true)
        .build();
    let create_group = adw::PreferencesGroup::new();
    create_group.add(&new_album);

    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr("Find an album"))
        .build();
    let albums_group = adw::PreferencesGroup::builder().title(tr("Albums")).build();
    let albums_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    albums_box.append(&search);
    albums_box.append(&list);
    albums_group.add(&albums_box);

    let page = adw::PreferencesPage::new();
    page.add(&create_group);
    page.add(&albums_group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(trn("Add {} Item to Album", "Add {} Items to Album", count))
        .content_width(420)
        .content_height(560)
        .child(&toolbar)
        .build();

    let ids = Rc::new(ids);
    {
        let ctx = Rc::clone(ctx);
        let ids = Rc::clone(&ids);
        let dialog = dialog.clone();
        new_album.connect_apply(move |row| {
            let name = row.text().trim().to_owned();
            if name.is_empty() {
                return;
            }
            dialog.close();
            let ctx = Rc::clone(&ctx);
            let ids = (*ids).clone();
            util::spawn(async move {
                let album_name = name.clone();
                match ctx
                    .call(move |library| library.create_album(&album_name, None))
                    .await
                {
                    Ok(album) => actions::add_to_album(&ctx, album, name, ids),
                    Err(error) => ctx.error(&tr("Could not create the album"), &error),
                }
            });
        });
    }
    {
        let list = list.clone();
        search.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            let mut row = list.first_child();
            while let Some(widget) = row {
                if let Some(action_row) = widget.downcast_ref::<adw::ActionRow>() {
                    action_row.set_visible(
                        needle.is_empty() || action_row.title().to_lowercase().contains(&needle),
                    );
                }
                row = widget.next_sibling();
            }
        });
    }

    let ctx_outer = Rc::clone(ctx);
    let dialog_ref = dialog.clone();
    util::spawn(async move {
        let ctx = ctx_outer;
        let albums = ctx
            .call(|library| library.read(|db| db.albums()))
            .await
            .unwrap_or_default();
        let manual: Vec<_> = albums
            .into_iter()
            .filter(|album| album.query.is_none())
            .collect();
        albums_group.set_visible(!manual.is_empty());
        search.set_visible(manual.len() > 8);
        for album in manual {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&album.name))
                .subtitle(util::items(album.count))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("view-grid-symbolic"));
            let ctx = Rc::clone(&ctx);
            let ids = Rc::clone(&ids);
            let dialog = dialog_ref.clone();
            row.connect_activated(move |_| {
                dialog.close();
                actions::add_to_album(&ctx, album.id, album.name.clone(), (*ids).clone());
            });
            list.append(&row);
        }
    });
    dialog.present(Some(&ctx.window));
}
