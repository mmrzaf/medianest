//! What every page of an open library needs: the library, thumbnails,
//! hover filmstrips, toasts, undo and navigation.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::Library;

use crate::config::Config;
use crate::filmstrips::Filmstrips;
use crate::i18n::tr;
use crate::textures::ThumbnailLoader;

type Undo = Box<dyn FnOnce()>;
type Navigate = Box<dyn Fn(Destination)>;

/// Places the interface can be asked to show.
#[derive(Clone, Debug)]
pub enum Destination {
    Review,
    Folders,
    Search(String),
}

pub struct Ctx {
    pub library: Library,
    pub loader: Rc<ThumbnailLoader>,
    pub filmstrips: Rc<Filmstrips>,
    pub config: Rc<RefCell<Config>>,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    /// The window's top-level navigation, where the viewer is pushed.
    pub nav: adw::NavigationView,
    undo: RefCell<Option<Undo>>,
    config_listeners: RefCell<Vec<Box<dyn Fn()>>>,
    navigate: RefCell<Option<Navigate>>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("library", &self.library)
            .finish_non_exhaustive()
    }
}

impl Ctx {
    pub fn new(
        library: Library,
        config: Rc<RefCell<Config>>,
        window: adw::ApplicationWindow,
        toasts: adw::ToastOverlay,
        nav: adw::NavigationView,
    ) -> Rc<Self> {
        Rc::new(Self {
            loader: ThumbnailLoader::new(library.clone()),
            filmstrips: Filmstrips::new(library.clone()),
            library,
            config,
            window,
            toasts,
            nav,
            undo: RefCell::new(None),
            config_listeners: RefCell::default(),
            navigate: RefCell::new(None),
        })
    }

    /// Runs blocking library work off the main thread.
    pub async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Library) -> medianest_service::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        match self.library.run(work).await {
            Some(Ok(value)) => Ok(value),
            Some(Err(error)) => Err(error.to_string()),
            None => Err(tr("The library was closed")),
        }
    }

    pub fn toast(&self, message: &str) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(message))
            .timeout(4)
            .build();
        self.toasts.add_toast(toast);
    }

    /// Shows a failure with its reason.
    pub fn error(&self, what: &str, reason: &str) {
        tracing::warn!(what, reason, "operation failed");
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(&format!("{what}: {reason}")))
            .timeout(8)
            .priority(adw::ToastPriority::High)
            .build();
        self.toasts.add_toast(toast);
    }

    /// Shows a message with an Undo button that runs `undo`.
    pub fn toast_undo(self: &Rc<Self>, message: &str, undo: impl FnOnce() + 'static) {
        *self.undo.borrow_mut() = Some(Box::new(undo));
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(message))
            .button_label(tr("_Undo"))
            .use_markup(true)
            .timeout(6)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(ctx) = weak.upgrade() {
                ctx.undo();
            }
        });
        self.toasts.add_toast(toast);
    }

    /// Reverts the last undoable change, once.
    pub fn undo(&self) {
        let undo = self.undo.borrow_mut().take();
        if let Some(undo) = undo {
            undo();
        }
    }

    /// Calls `listener` after the preferences change.
    pub fn connect_config_changed(&self, listener: impl Fn() + 'static) {
        self.config_listeners.borrow_mut().push(Box::new(listener));
    }

    /// Saves the configuration and tells everyone who listens.
    pub fn config_changed(&self) {
        self.config.borrow().save();
        for listener in self.config_listeners.borrow().iter() {
            listener();
        }
    }

    pub fn set_navigate(&self, navigate: impl Fn(Destination) + 'static) {
        *self.navigate.borrow_mut() = Some(Box::new(navigate));
    }

    pub fn go(&self, destination: Destination) {
        if let Some(navigate) = self.navigate.borrow().as_ref() {
            navigate(destination);
        }
    }

    /// Asks a yes/no question; `confirm` runs on the destructive answer.
    pub fn confirm(
        &self,
        heading: &str,
        body: &str,
        action: &str,
        destructive: bool,
        confirm: impl FnOnce() + 'static,
    ) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_responses(&[("cancel", &tr("_Cancel")), ("confirm", action)]);
        dialog.set_response_appearance(
            "confirm",
            if destructive {
                adw::ResponseAppearance::Destructive
            } else {
                adw::ResponseAppearance::Suggested
            },
        );
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let confirm = RefCell::new(Some(confirm));
        dialog.connect_response(None, move |_, response| {
            if response == "confirm" {
                if let Some(confirm) = confirm.borrow_mut().take() {
                    confirm();
                }
            }
        });
        dialog.present(Some(&self.window));
    }
}
