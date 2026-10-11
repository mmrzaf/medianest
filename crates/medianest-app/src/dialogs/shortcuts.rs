//! The list of keyboard shortcuts.

use adw::prelude::*;

use crate::i18n::tr;

/// Key caps for an accelerator such as `<Control><Shift>r`.
fn keys(accelerator: &str) -> gtk::Box {
    let parts: Vec<String> = if accelerator.contains("...") {
        vec![accelerator.replace("...", "–")]
    } else {
        match gtk::accelerator_parse(accelerator) {
            Some((key, modifiers)) => gtk::accelerator_get_label(key, modifiers)
                .split('+')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
            None => vec![accelerator.to_owned()],
        }
    };
    let row = gtk::Box::builder()
        .spacing(4)
        .valign(gtk::Align::Center)
        .build();
    for part in parts {
        row.append(
            &gtk::Label::builder()
                .label(part)
                .css_classes(["keycap"])
                .build(),
        );
    }
    row
}

pub fn show(parent: &adw::ApplicationWindow) {
    let sections: Vec<(String, Vec<(&str, String)>)> = vec![
        (
            tr("General"),
            vec![
                ("<Control>i", tr("Import")),
                ("<Control>f", tr("Search")),
                ("<Control>o", tr("Open library")),
                ("<Control>z", tr("Undo")),
                ("F9", tr("Show or hide the sidebar")),
                ("<Control>comma", tr("Preferences")),
                ("<Control>question", tr("Keyboard shortcuts")),
                ("<Control>w", tr("Close window")),
                ("<Control>q", tr("Quit")),
            ],
        ),
        (
            tr("Navigation"),
            vec![
                ("<Alt>1", tr("Photos")),
                ("<Alt>2", tr("Albums")),
                ("<Alt>3", tr("Places")),
                ("<Alt>4", tr("Review")),
            ],
        ),
        (
            tr("Photos"),
            vec![
                ("Return", tr("Open")),
                ("space", tr("Select or deselect")),
                ("<Control>a", tr("Select all")),
                ("<Shift>Down", tr("Extend the selection")),
                ("Escape", tr("Stop selecting")),
                ("<Control>plus", tr("Larger tiles")),
                ("<Control>minus", tr("Smaller tiles")),
                ("f", tr("Favorite")),
                ("t", tr("Tags")),
                ("0...5", tr("Rating")),
                ("<Control>d", tr("Change date")),
                ("<Control>r", tr("Rotate clockwise")),
                ("<Control><Shift>r", tr("Rotate counterclockwise")),
                ("<Control>h", tr("Hide")),
                ("<Control>c", tr("Copy files")),
                ("<Control>e", tr("Export")),
                ("Delete", tr("Move to trash")),
                ("<Alt>Return", tr("Show information")),
            ],
        ),
        (
            tr("Viewer"),
            vec![
                ("Left", tr("Previous")),
                ("Right", tr("Next")),
                ("plus", tr("Zoom in")),
                ("minus", tr("Zoom out")),
                ("<Control>0", tr("Best fit")),
                ("<Control>1", tr("Original size")),
                ("i", tr("Information")),
                ("k", tr("Play or pause")),
                ("F11", tr("Fullscreen")),
                ("F5", tr("Slideshow")),
                ("Escape", tr("Back to the grid")),
            ],
        ),
    ];
    let page = adw::PreferencesPage::new();
    for (title, rows) in sections {
        let group = adw::PreferencesGroup::builder().title(title).build();
        for (accelerator, description) in rows {
            let row = adw::ActionRow::builder().title(description).build();
            row.add_suffix(&keys(accelerator));
            group.add(&row);
        }
        page.add(&group);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(tr("Keyboard Shortcuts"))
        .content_width(520)
        .content_height(640)
        .child(&toolbar)
        .build();
    dialog.present(Some(parent));
}
