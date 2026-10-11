//! The application: actions, the window and the open library.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::APP_ID;
use crate::config::Config;
use crate::i18n::tr;
use crate::window::Window;

/// Shared state of the running application.
#[derive(Debug)]
pub struct Application {
    app: adw::Application,
    config: Rc<RefCell<Config>>,
    window: RefCell<Option<Rc<Window>>>,
}

impl Application {
    pub fn new() -> Rc<Self> {
        let app = adw::Application::builder()
            .application_id(APP_ID)
            .flags(gio::ApplicationFlags::HANDLES_OPEN)
            .resource_base_path(crate::RESOURCE_BASE)
            .build();
        let this = Rc::new(Self {
            app,
            config: Rc::new(RefCell::new(Config::load())),
            window: RefCell::new(None),
        });
        this.setup();
        this
    }

    pub fn run(&self) -> glib::ExitCode {
        self.app.run()
    }

    fn setup(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.app.connect_startup(move |app| {
            if let Some(this) = weak.upgrade() {
                this.install_actions(app);
            }
        });
        let weak = Rc::downgrade(self);
        self.app.connect_activate(move |_| {
            if let Some(this) = weak.upgrade() {
                let window = this.window();
                if !window.has_library() {
                    let last = {
                        let config = this.config.borrow();
                        config
                            .open_last
                            .then(|| config.recent.first().cloned())
                            .flatten()
                    };
                    match last {
                        Some(path) if path.join(".medianest").exists() => {
                            window.open_library(path, false)
                        }
                        _ => window.show_welcome(),
                    }
                }
                window.present();
            }
        });
        let weak = Rc::downgrade(self);
        self.app.connect_open(move |_, files, _| {
            if let Some(this) = weak.upgrade() {
                let window = this.window();
                if let Some(path) = files.first().and_then(gio::File::path) {
                    window.open_library(path, false);
                }
                window.present();
            }
        });
    }

    fn window(self: &Rc<Self>) -> Rc<Window> {
        if let Some(window) = self.window.borrow().as_ref() {
            return Rc::clone(window);
        }
        let window = Window::new(&self.app, Rc::clone(&self.config));
        *self.window.borrow_mut() = Some(Rc::clone(&window));
        window
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application) {
        let quit = gio::ActionEntry::builder("quit")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::Application, _, _| {
                    match this.window.borrow().as_ref() {
                        Some(window) => window.request_close(),
                        None => this.app.quit(),
                    }
                }
            ))
            .build();
        let shortcuts = gio::ActionEntry::builder("shortcuts")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::Application, _, _| {
                    if let Some(window) = this.window.borrow().as_ref() {
                        crate::dialogs::shortcuts::show(&window.widget());
                    }
                }
            ))
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::Application, _, _| this.show_about()
            ))
            .build();
        app.add_action_entries([quit, shortcuts, about]);

        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);
        app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
        app.set_accels_for_action("win.open-library", &["<Control>o"]);
        app.set_accels_for_action("win.import", &["<Control>i"]);
        app.set_accels_for_action("win.search", &["<Control>f"]);
        app.set_accels_for_action("win.select-all", &["<Control>a"]);
        app.set_accels_for_action(
            "win.zoom-in",
            &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
        );
        app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        app.set_accels_for_action("win.toggle-sidebar", &["F9"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.undo", &["<Control>z"]);
        app.set_accels_for_action("win.go-photos", &["<Alt>1"]);
        app.set_accels_for_action("win.go-albums", &["<Alt>2"]);
        app.set_accels_for_action("win.go-places", &["<Alt>3"]);
        app.set_accels_for_action("win.go-review", &["<Alt>4"]);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("MediaNest")
            .application_icon(APP_ID)
            .developer_name("mmrzaf")
            .version(crate::VERSION)
            .website("https://github.com/mmrzaf/medianest")
            .issue_url("https://github.com/mmrzaf/medianest/issues")
            .license_type(gtk::License::MitX11)
            .copyright("© 2026 mmrzaf")
            .comments(tr(
                "A calm home for your photos and videos. MediaNest keeps your originals safe, \
                 organises them by date, and never changes a file without a verified copy.",
            ))
            .build();
        about.set_developers(&["mmrzaf https://github.com/mmrzaf"]);
        about.add_credit_section(
            Some(&tr("Place names")),
            &["GeoNames (CC BY 4.0) https://www.geonames.org"],
        );
        about.add_credit_section(
            Some(&tr("Maps")),
            &["© OpenStreetMap contributors https://www.openstreetmap.org/copyright"],
        );
        about.add_legal_section(
            "GeoNames",
            Some("© GeoNames"),
            gtk::License::Custom,
            Some(&tr(
                "Offline place names are derived from GeoNames data, licensed under the \
                 Creative Commons Attribution 4.0 License.",
            )),
        );
        let parent = self.window.borrow().as_ref().map(|window| window.widget());
        about.present(parent.as_ref());
    }
}

/// The default folder for new libraries.
#[must_use]
pub fn default_library_parent() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(glib::home_dir)
}
