//! Choosing a name and place for a new library.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::{tr, trf};

/// Shows the dialog; `create` receives the new library's folder.
pub fn show(parent: &impl IsA<gtk::Widget>, create: impl Fn(PathBuf) + 'static) {
    let location = Rc::new(RefCell::new(crate::application::default_library_parent()));

    let name = adw::EntryRow::builder()
        .title(tr("Name"))
        .text(tr("Photo Library"))
        .build();
    let place = adw::ActionRow::builder()
        .title(tr("Location"))
        .subtitle(glib::markup_escape_text(
            &location.borrow().display().to_string(),
        ))
        .activatable(true)
        .build();
    place.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    let group = adw::PreferencesGroup::builder()
        .description(tr(
            "Photos and videos you import are copied into a “Media” folder inside the library, \
             sorted by year, month and day. Choose a disk with plenty of space.",
        ))
        .build();
    group.add(&name);
    group.add(&place);

    let result = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .margin_top(12)
        .build();

    let create_button = gtk::Button::builder()
        .label(tr("_Create"))
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::builder()
        .label(tr("_Cancel"))
        .use_underline(true)
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&create_button);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&group);
    body.append(&result);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title(tr("New Library"))
        .content_width(460)
        .child(&toolbar)
        .default_widget(&create_button)
        .build();

    let target = {
        let location = Rc::clone(&location);
        let name = name.clone();
        move || -> Option<PathBuf> {
            let text = name.text();
            let text = text.trim();
            (!text.is_empty() && !text.contains('/')).then(|| location.borrow().join(text))
        }
    };
    let update = {
        let target = target.clone();
        let result = result.clone();
        let create_button = create_button.clone();
        move || match target() {
            Some(path) if path.exists() && path.join(".medianest").exists() => {
                result.set_label(&tr("A library with this name already exists there."));
                create_button.set_sensitive(false);
            }
            Some(path) => {
                result.set_label(&trf(
                    "The library will be created at {}",
                    &[&path.display().to_string()],
                ));
                create_button.set_sensitive(true);
            }
            None => {
                result.set_label(&tr("Enter a name without slashes."));
                create_button.set_sensitive(false);
            }
        }
    };
    update();
    name.connect_changed({
        let update = update.clone();
        move |_| update()
    });
    name.set_activates_default(true);

    place.connect_activated({
        let location = Rc::clone(&location);
        let dialog = dialog.clone();
        let update = update.clone();
        move |row| {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Choose a Location"))
                .initial_folder(&gio::File::for_path(&*location.borrow()))
                .build();
            let location = Rc::clone(&location);
            let row = row.clone();
            let update = update.clone();
            let parent = dialog.root().and_downcast::<gtk::Window>();
            chooser.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |chosen| {
                if let Some(path) = chosen.ok().and_then(|file| file.path()) {
                    row.set_subtitle(&glib::markup_escape_text(&path.display().to_string()));
                    *location.borrow_mut() = path;
                    update();
                }
            });
        }
    });

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    create_button.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            if let Some(path) = target() {
                dialog.close();
                create(path);
            }
        }
    });
    dialog.present(Some(parent));
    name.grab_focus();
}
