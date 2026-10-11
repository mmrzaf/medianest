//! Preferences of the application and of the open library.

use std::rc::Rc;

use adw::prelude::*;

use crate::context::Ctx;
use crate::i18n::{tr, trf};
use crate::util;

pub fn show(ctx: &Rc<Ctx>) {
    let dialog = adw::PreferencesDialog::builder()
        .search_enabled(false)
        .build();
    dialog.add(&general_page(ctx));
    dialog.add(&library_page(ctx));
    dialog.present(Some(&ctx.window));
}

fn switch(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build()
}

fn general_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let config = ctx.config.borrow().clone();
    let page = adw::PreferencesPage::builder()
        .title(tr("General"))
        .icon_name("preferences-system-symbolic")
        .build();

    let start = adw::PreferencesGroup::builder()
        .title(tr("Starting"))
        .build();
    let open_last = switch(
        &tr("Open the Last Library"),
        &tr("Skip the welcome screen on start"),
        config.open_last,
    );
    start.add(&open_last);
    page.add(&start);

    let photos = adw::PreferencesGroup::builder().title(tr("Photos")).build();
    let square = switch(
        &tr("Square Tiles"),
        &tr("Crop photos to squares in the grid"),
        config.square_tiles,
    );
    photos.add(&square);
    let muted = switch(&tr("Start Videos Muted"), "", config.start_muted);
    photos.add(&muted);
    let slideshow = adw::SpinRow::with_range(1.0, 60.0, 1.0);
    slideshow.set_title(&tr("Slideshow Interval"));
    slideshow.set_subtitle(&tr("Seconds per photo"));
    slideshow.set_value(f64::from(config.slideshow_seconds));
    photos.add(&slideshow);
    page.add(&photos);

    let privacy = adw::PreferencesGroup::builder()
        .title(tr("Maps"))
        .description(tr(
            "Place names are always found offline. Map images are downloaded from \
             OpenStreetMap; turn this off to keep MediaNest completely offline.",
        ))
        .build();
    let maps = switch(
        &tr("Show Online Maps"),
        &tr("Applies the next time the library is opened"),
        config.online_maps,
    );
    privacy.add(&maps);
    page.add(&privacy);

    let save = {
        let ctx = Rc::clone(ctx);
        move |change: &dyn Fn(&mut crate::config::Config)| {
            change(&mut ctx.config.borrow_mut());
            ctx.config_changed();
        }
    };
    let s = save.clone();
    open_last.connect_active_notify(move |row| s(&|c| c.open_last = row.is_active()));
    let s = save.clone();
    square.connect_active_notify(move |row| s(&|c| c.square_tiles = row.is_active()));
    let s = save.clone();
    muted.connect_active_notify(move |row| s(&|c| c.start_muted = row.is_active()));
    let s = save.clone();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    slideshow.connect_value_notify(move |row| s(&|c| c.slideshow_seconds = row.value() as u32));
    maps.connect_active_notify(move |row| save(&|c| c.online_maps = row.is_active()));
    page
}

fn library_page(ctx: &Rc<Ctx>) -> adw::PreferencesPage {
    let settings = ctx.library.settings();
    let page = adw::PreferencesPage::builder()
        .title(tr("Library"))
        .icon_name("mn-library-symbolic")
        .build();

    let folders = adw::PreferencesGroup::builder()
        .title(tr("Linked Folders"))
        .build();
    let watch = switch(
        &tr("Watch for Changes"),
        &tr("Notice new, changed and removed files right away"),
        settings.watch_folders,
    );
    let hashing = switch(
        &tr("Check New Files in the Background"),
        &tr("Find duplicates and prepare files for verification"),
        settings.background_hashing,
    );
    folders.add(&watch);
    folders.add(&hashing);
    page.add(&folders);

    let safety = adw::PreferencesGroup::builder()
        .title(tr("Safety"))
        .description(tr(
            "MediaNest re-reads every file now and then to catch silent damage, and keeps \
             backups of its catalog inside the library folder.",
        ))
        .build();
    let verify = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    verify.set_title(&tr("Verify Files Every"));
    verify.set_subtitle(&tr("Days; 0 turns it off"));
    verify.set_value(f64::from(settings.verify_every_days));
    let backup = adw::SpinRow::with_range(0.0, 720.0, 1.0);
    backup.set_title(&tr("Back Up the Catalog Every"));
    backup.set_subtitle(&tr("Hours; 0 turns it off"));
    backup.set_value(f64::from(settings.backup_every_hours));
    let keep = adw::SpinRow::with_range(1.0, 100.0, 1.0);
    keep.set_title(&tr("Backups to Keep"));
    #[allow(clippy::cast_precision_loss)]
    keep.set_value(settings.keep_backups as f64);
    safety.add(&verify);
    safety.add(&backup);
    safety.add(&keep);
    page.add(&safety);

    let cache = adw::PreferencesGroup::builder()
        .title(tr("Storage"))
        .build();
    let previews = adw::SpinRow::with_range(128.0, 65_536.0, 128.0);
    previews.set_title(&tr("Preview Cache Limit"));
    previews.set_subtitle(&tr("Megabytes for large previews used by the viewer"));
    #[allow(clippy::cast_precision_loss)]
    previews.set_value(settings.preview_cache_mb as f64);
    cache.add(&previews);
    let rebuild = adw::ButtonRow::builder()
        .title(tr("Rebuild Thumbnails"))
        .build();
    cache.add(&rebuild);
    page.add(&cache);

    let apply = {
        let ctx = Rc::clone(ctx);
        move |change: &dyn Fn(&mut medianest_service::Settings)| {
            let mut settings = ctx.library.settings();
            change(&mut settings);
            let library = ctx.library.clone();
            // Saving writes to the catalog; keep it off the main thread.
            drop(library.run(move |library| library.set_settings(settings)));
        }
    };
    let a = apply.clone();
    watch.connect_active_notify(move |row| a(&|s| s.watch_folders = row.is_active()));
    let a = apply.clone();
    hashing.connect_active_notify(move |row| a(&|s| s.background_hashing = row.is_active()));
    let a = apply.clone();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    verify.connect_value_notify(move |row| a(&|s| s.verify_every_days = row.value() as u32));
    let a = apply.clone();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    backup.connect_value_notify(move |row| a(&|s| s.backup_every_hours = row.value() as u32));
    let a = apply.clone();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    keep.connect_value_notify(move |row| a(&|s| s.keep_backups = row.value() as usize));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    previews.connect_value_notify(move |row| apply(&|s| s.preview_cache_mb = row.value() as u64));

    let ctx = Rc::clone(ctx);
    rebuild.connect_activated(move |_| {
        let inner = Rc::clone(&ctx);
        ctx.confirm(
            &tr("Rebuild All Thumbnails?"),
            &tr(
                "Thumbnails are made again in the background. Photos show placeholders until then.",
            ),
            &tr("_Rebuild"),
            false,
            move || {
                let ctx = inner;
                util::spawn(async move {
                    match ctx.call(|library| library.rebuild_thumbnails(false)).await {
                        Ok(count) => {
                            ctx.loader.clear();
                            ctx.toast(&trf(
                                "Rebuilding {} thumbnails",
                                &[&util::number(count as u64)],
                            ));
                        }
                        Err(error) => ctx.error(&tr("Could not rebuild thumbnails"), &error),
                    }
                });
            },
        );
    });
    page
}
