//! The first screen: create or open a library.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::config::Config;
use crate::i18n::tr;

type PathCallback = Box<dyn Fn(PathBuf)>;

pub struct Welcome {
    root: adw::ToolbarView,
    recent_group: adw::PreferencesGroup,
    recent_rows: RefCell<Vec<adw::ActionRow>>,
    config: Rc<RefCell<Config>>,
    on_open: RefCell<Option<PathCallback>>,
    on_create: RefCell<Option<PathCallback>>,
}

impl std::fmt::Debug for Welcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Welcome").finish_non_exhaustive()
    }
}

impl Welcome {
    pub fn new(config: Rc<RefCell<Config>>) -> Rc<Self> {
        let header = adw::HeaderBar::builder().show_title(false).build();
        let menu = gio::Menu::new();
        menu.append(Some(&tr("_Keyboard Shortcuts")), Some("app.shortcuts"));
        menu.append(Some(&tr("_About MediaNest")), Some("app.about"));
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&menu)
                .primary(true)
                .tooltip_text(tr("Main Menu"))
                .build(),
        );

        let create = gtk::Button::builder()
            .label(tr("_Create New Library"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let open = gtk::Button::builder()
            .label(tr("_Open Existing Library…"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        buttons.append(&create);
        buttons.append(&open);

        let recent_group = adw::PreferencesGroup::builder()
            .title(tr("Recent Libraries"))
            .margin_top(24)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        content.append(&buttons);
        content.append(&recent_group);
        let clamp = adw::Clamp::builder()
            .maximum_size(480)
            .child(&content)
            .build();

        let status = adw::StatusPage::builder()
            .icon_name(crate::APP_ID)
            .title(tr("Welcome to MediaNest"))
            .description(tr(
                "A library keeps your photos and videos in one folder, organised by date. \
                 Your originals are copied and verified before anything else happens.",
            ))
            .child(&clamp)
            .build();
        status.add_css_class("welcome");

        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(&status));

        let this = Rc::new(Self {
            root,
            recent_group,
            recent_rows: RefCell::default(),
            config,
            on_open: RefCell::default(),
            on_create: RefCell::default(),
        });

        let weak = Rc::downgrade(&this);
        create.connect_clicked(move |button| {
            if let (Some(this), Some(window)) =
                (weak.upgrade(), button.root().and_downcast::<gtk::Window>())
            {
                this.create_library(&window);
            }
        });
        open.connect_clicked(|button| {
            let _ = button.activate_action("win.open-library", None);
        });
        this.refresh();
        this
    }

    pub fn widget(&self) -> &adw::ToolbarView {
        &self.root
    }

    pub fn connect_open(&self, callback: impl Fn(PathBuf) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(callback));
    }

    pub fn connect_create(&self, callback: impl Fn(PathBuf) + 'static) {
        *self.on_create.borrow_mut() = Some(Box::new(callback));
    }

    /// Rebuilds the list of recent libraries.
    pub fn refresh(self: &Rc<Self>) {
        for row in self.recent_rows.borrow_mut().drain(..) {
            self.recent_group.remove(&row);
        }
        let recent = self.config.borrow().recent.clone();
        self.recent_group.set_visible(!recent.is_empty());
        for path in recent {
            let available = path.join(".medianest").is_dir();
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let subtitle = if available {
                path.display().to_string()
            } else {
                format!(
                    "{} — {}",
                    path.display(),
                    tr("not available; is the disk connected?")
                )
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .activatable(available)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("mn-library-symbolic"));
            if available {
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            }
            let forget = gtk::Button::builder()
                .icon_name("edit-clear-symbolic")
                .tooltip_text(tr("Remove From List"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&forget);
            let weak = Rc::downgrade(self);
            let target = path.clone();
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    if let Some(open) = this.on_open.borrow().as_ref() {
                        open(target.clone());
                    }
                }
            });
            let weak = Rc::downgrade(self);
            forget.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.config.borrow_mut().forget(&path);
                    this.config.borrow().save();
                    this.refresh();
                }
            });
            self.recent_group.add(&row);
            self.recent_rows.borrow_mut().push(row);
        }
    }

    /// Asks for a name and place, then creates the library.
    pub fn create_library(self: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        let weak = Rc::downgrade(self);
        crate::dialogs::new_library::show(parent, move |path| {
            if let Some(this) = weak.upgrade() {
                if let Some(create) = this.on_create.borrow().as_ref() {
                    create(path);
                }
            }
        });
    }
}
