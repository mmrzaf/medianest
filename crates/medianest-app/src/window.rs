//! The main window: welcome screen, opening and closing libraries.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use medianest_service::{Library, Settings};

use crate::config::Config;
use crate::context::Ctx;
use crate::i18n::{tr, trf};
use crate::library_view::LibraryView;
use crate::welcome::Welcome;

#[derive(Debug)]
pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    loading: adw::StatusPage,
    welcome: Rc<Welcome>,
    config: Rc<RefCell<Config>>,
    view: RefCell<Option<Rc<LibraryView>>>,
    closing: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application, config: Rc<RefCell<Config>>) -> Rc<Self> {
        let (width, height, maximized) = {
            let config = config.borrow();
            (
                config.window_width,
                config.window_height,
                config.window_maximized,
            )
        };
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("MediaNest")
            .default_width(width)
            .default_height(height)
            .maximized(maximized)
            .width_request(360)
            .height_request(294)
            .build();
        if crate::VERSION.contains('-') {
            window.add_css_class("devel");
        }

        let loading = adw::StatusPage::builder()
            .paintable(&adw::SpinnerPaintable::new(None::<&gtk::Widget>))
            .title(tr("Opening Library…"))
            .build();
        let loading_page = adw::ToolbarView::new();
        loading_page.add_top_bar(&adw::HeaderBar::new());
        loading_page.set_content(Some(&loading));

        let welcome = Welcome::new(Rc::clone(&config));
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(welcome.widget(), Some("welcome"));
        stack.add_named(&loading_page, Some("loading"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        window.set_content(Some(&toasts));

        // Narrow windows collapse the sidebar.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            720.0,
            adw::LengthUnit::Sp,
        ));
        window.add_breakpoint(breakpoint.clone());

        let this = Rc::new(Self {
            window,
            toasts,
            stack,
            loading,
            welcome,
            config,
            view: RefCell::new(None),
            closing: Cell::new(false),
        });
        this.setup(&breakpoint);
        this
    }

    pub fn widget(&self) -> adw::ApplicationWindow {
        self.window.clone()
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn has_library(&self) -> bool {
        self.view.borrow().is_some()
    }

    fn setup(self: &Rc<Self>, breakpoint: &adw::Breakpoint) {
        let weak = Rc::downgrade(self);
        self.welcome.connect_open(move |path| {
            if let Some(this) = weak.upgrade() {
                this.open_library(path, false);
            }
        });
        let weak = Rc::downgrade(self);
        self.welcome.connect_create(move |path| {
            if let Some(this) = weak.upgrade() {
                this.open_library(path, true);
            }
        });

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if this.closing.get() {
                return glib::Propagation::Proceed;
            }
            this.request_close();
            glib::Propagation::Stop
        });

        let open = gio::ActionEntry::builder("open-library")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::ApplicationWindow, _, _| this.choose_library()
            ))
            .build();
        let new = gio::ActionEntry::builder("new-library")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::ApplicationWindow, _, _| this.welcome.create_library(&this.window)
            ))
            .build();
        let close = gio::ActionEntry::builder("close-library")
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::ApplicationWindow, _, _| {
                    this.close_library(|this| this.show_welcome());
                }
            ))
            .build();
        let restore = gio::ActionEntry::builder("restore-backup")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &adw::ApplicationWindow, _, parameter| {
                    if let Some(backup) = parameter.and_then(|p| p.get::<String>()) {
                        this.restore_backup(PathBuf::from(backup));
                    }
                }
            ))
            .build();
        self.window.add_action_entries([open, new, close, restore]);

        let weak = Rc::downgrade(self);
        breakpoint.connect_apply(move |_| {
            if let Some(view) = weak.upgrade().and_then(|this| this.view.borrow().clone()) {
                view.set_narrow(true);
            }
        });
        let weak = Rc::downgrade(self);
        breakpoint.connect_unapply(move |_| {
            if let Some(view) = weak.upgrade().and_then(|this| this.view.borrow().clone()) {
                view.set_narrow(false);
            }
        });
    }

    pub fn show_welcome(&self) {
        self.welcome.refresh();
        self.stack.set_visible_child_name("welcome");
        self.window.set_title(Some("MediaNest"));
    }

    fn choose_library(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title(tr("Open Library"))
            .modal(true)
            .build();
        let this = Rc::clone(self);
        dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |result| {
            if let Ok(Some(path)) = result.map(|file| file.path()) {
                this.open_library(path, false);
            }
        });
    }

    /// Opens (or creates) a library, replacing the one that is open.
    pub fn open_library(self: &Rc<Self>, path: PathBuf, create: bool) {
        if self
            .view
            .borrow()
            .as_ref()
            .is_some_and(|view| view.ctx().library.root() == path)
        {
            return;
        }
        let this = Rc::clone(self);
        self.close_library(move |this_ref| {
            let _ = this_ref;
            this.start_opening(path, create);
        });
    }

    fn start_opening(self: &Rc<Self>, path: PathBuf, create: bool) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.loading.set_title(&if create {
            tr("Creating Library…")
        } else {
            tr("Opening Library…")
        });
        self.loading
            .set_description(Some(&glib::markup_escape_text(&name)));
        self.stack.set_visible_child_name("loading");
        let this = Rc::clone(self);
        crate::util::spawn(async move {
            let target = path.clone();
            let opened = gio::spawn_blocking(move || {
                if create {
                    Library::create(&target, Settings::default())
                } else {
                    Library::open(&target, Settings::default())
                }
            })
            .await;
            match opened {
                Ok(Ok(library)) => this.show_library(library),
                Ok(Err(error)) => {
                    this.show_welcome();
                    let heading = if create {
                        tr("Could Not Create Library")
                    } else {
                        tr("Could Not Open Library")
                    };
                    let dialog = adw::AlertDialog::new(Some(&heading), Some(&error.to_string()));
                    dialog.add_response("close", &tr("_Close"));
                    dialog.present(Some(&this.window));
                    if matches!(
                        error,
                        medianest_service::Error::Vault(
                            medianest_service::vault::VaultError::NotALibrary(_)
                        )
                    ) {
                        this.config.borrow_mut().forget(&path);
                        this.config.borrow().save();
                        this.welcome.refresh();
                    }
                }
                Err(_) => {
                    this.show_welcome();
                    this.toast(&tr("Opening the library failed unexpectedly"));
                }
            }
        });
    }

    fn show_library(self: &Rc<Self>, library: Library) {
        {
            let mut config = self.config.borrow_mut();
            config.remember(library.root());
            config.save();
        }
        let nav = adw::NavigationView::new();
        let ctx = Ctx::new(
            library,
            Rc::clone(&self.config),
            self.window.clone(),
            self.toasts.clone(),
            nav.clone(),
        );
        let view = LibraryView::new(ctx);
        nav.add(view.page());
        if let Some(old) = self.stack.child_by_name("library") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&nav, Some("library"));
        self.stack.set_visible_child_name("library");
        view.set_narrow(self.window.width() > 0 && self.window.width() < 720);
        let title = view.title();
        self.window
            .set_title(Some(&trf("{} — MediaNest", &[&title])));
        *self.view.borrow_mut() = Some(view);
    }

    fn toast(&self, message: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(message)));
    }

    /// Closes the open library, if any, then calls `then`. Asks first when
    /// work is still running.
    fn close_library(self: &Rc<Self>, then: impl FnOnce(&Rc<Self>) + 'static) {
        let Some(view) = self.view.borrow().clone() else {
            then(self);
            return;
        };
        let library = view.ctx().library.clone();
        let ctx = view.ctx();
        let finish = {
            let this = Rc::clone(self);
            move || {
                view.shutdown();
                this.view.borrow_mut().take();
                if let Some(old) = this.stack.child_by_name("library") {
                    this.stack.remove(&old);
                }
                let this_inner = Rc::clone(&this);
                crate::util::spawn(async move {
                    let _ = gio::spawn_blocking(move || library.close()).await;
                    then(&this_inner);
                });
            }
        };
        let busy: Vec<String> = ctx
            .library
            .jobs()
            .into_iter()
            .filter(|job| {
                !job.state.is_finished() && job.lane == medianest_service::Lane::Foreground
            })
            .map(|job| job.title)
            .collect();
        if busy.is_empty() {
            finish();
            return;
        }
        ctx.confirm(
            &tr("Stop Work in Progress?"),
            &trf(
                "{} is still running. Stopping is safe: finished items are kept and nothing is lost.",
                &[&busy.join(", ")],
            ),
            &tr("_Stop and Close"),
            true,
            finish,
        );
    }

    /// Replaces the open library's catalog with a backup and reopens it.
    fn restore_backup(self: &Rc<Self>, backup: PathBuf) {
        let Some(root) = self
            .view
            .borrow()
            .as_ref()
            .map(|view| view.ctx().library.root().to_path_buf())
        else {
            return;
        };
        self.close_library(move |this| {
            let this = Rc::clone(this);
            this.loading.set_title(&tr("Restoring Backup…"));
            this.stack.set_visible_child_name("loading");
            crate::util::spawn(async move {
                let target = root.clone();
                let restored =
                    gio::spawn_blocking(move || Library::restore_backup(&target, &backup)).await;
                match restored {
                    Ok(Ok(saved)) => {
                        this.start_opening(root, false);
                        this.toast(&trf(
                            "Backup restored. The previous catalog was saved as {}",
                            &[&saved.display().to_string()],
                        ));
                    }
                    Ok(Err(error)) => {
                        this.start_opening(root, false);
                        this.toast(&trf(
                            "Could not restore the backup: {}",
                            &[&error.to_string()],
                        ));
                    }
                    Err(_) => this.start_opening(root, false),
                }
            });
        });
    }

    /// Closes the window after the library has shut down cleanly.
    pub fn request_close(self: &Rc<Self>) {
        self.save_geometry();
        self.close_library(|this| {
            this.closing.set(true);
            this.window.close();
        });
    }

    fn save_geometry(&self) {
        let mut config = self.config.borrow_mut();
        config.window_maximized = self.window.is_maximized();
        if !config.window_maximized {
            let (width, height) = self.window.default_size();
            config.window_width = width;
            config.window_height = height;
        }
        config.save();
    }
}
