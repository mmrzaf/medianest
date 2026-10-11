//! A page of photos: the library, a filter, an album, a folder or the trash.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use medianest_service::Event;
use medianest_service::catalog::{TimelineItem, View, flags};
use medianest_service::core::query::Query;
use medianest_service::core::{AlbumId, AssetId};

use super::Section;
use crate::actions;
use crate::context::Ctx;
use crate::grid::{PhotoGrid, Scrubber, Sections};
use crate::i18n::{tr, trn};
use crate::util::{self, Debounce};

/// What kind of collection a page shows, which decides its actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineKind {
    /// Everything: the main Photos section.
    Library,
    /// A fixed filter such as Favorites or a tag.
    Filtered,
    Recent,
    Trash,
    /// A manual album, in its own order.
    Album(AlbumId),
}

pub struct TimelineOptions {
    pub title: String,
    pub view: View,
    pub kind: TimelineKind,
    pub searchable: bool,
    pub empty_icon: &'static str,
    pub empty_title: String,
    pub empty_description: String,
    pub sidebar_toggle: Option<gtk::ToggleButton>,
}

pub struct TimelinePage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    kind: TimelineKind,
    base_view: RefCell<View>,
    search_query: RefCell<Query>,
    root: adw::ToolbarView,
    title: adw::WindowTitle,
    header: adw::HeaderBar,
    grid: PhotoGrid,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    suggestions: gtk::FlowBox,
    selection_bar: gtk::ActionBar,
    selection_revealer: gtk::Revealer,
    favorite_button: gtk::Button,
    normal_end: gtk::Box,
    select_cancel: gtk::Button,
    select_all: gtk::Button,
    start_toggle: Option<gtk::ToggleButton>,
    actions: gio::SimpleActionGroup,
    empty_text: (String, String, &'static str),
    generation: Cell<u64>,
    loaded: Cell<bool>,
    stale: Cell<bool>,
    reload_debounce: Debounce,
    suggest_debounce: Debounce,
    on_selection_title: RefCell<String>,
    subtitle: RefCell<String>,
}

impl std::fmt::Debug for TimelinePage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelinePage")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl TimelinePage {
    pub fn new(ctx: Rc<Ctx>, options: TimelineOptions) -> Rc<Self> {
        let grid = PhotoGrid::new(Rc::clone(&ctx.loader));
        {
            let config = ctx.config.borrow();
            grid.set_zoom(config.zoom);
            grid.set_square(config.square_tiles);
        }
        if matches!(options.kind, TimelineKind::Album(_) | TimelineKind::Recent) {
            grid.set_sections(Sections::None);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .vexpand(true)
            .build();
        let scrubber = Scrubber::new(&grid);
        let overlay = gtk::Overlay::builder().child(&scroller).build();
        overlay.add_overlay(&scrubber);

        let empty = adw::StatusPage::builder()
            .icon_name(options.empty_icon)
            .title(&options.empty_title)
            .description(&options.empty_description)
            .vexpand(true)
            .build();
        if options.kind == TimelineKind::Library {
            let import = gtk::Button::builder()
                .label(tr("_Import…"))
                .use_underline(true)
                .action_name("win.import")
                .halign(gtk::Align::Center)
                .css_classes(["pill", "suggested-action"])
                .build();
            let add = gtk::Button::builder()
                .label(tr("_Add a Folder…"))
                .use_underline(true)
                .action_name("win.add-folder")
                .halign(gtk::Align::Center)
                .css_classes(["pill"])
                .build();
            let buttons = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .halign(gtk::Align::Center)
                .build();
            buttons.append(&import);
            buttons.append(&add);
            empty.set_child(Some(&buttons));
        }
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&overlay, Some("grid"));
        stack.add_named(&empty, Some("empty"));
        stack.set_visible_child_name("loading");

        // Header.
        let title = adw::WindowTitle::new(&options.title, "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        if let Some(toggle) = &options.sidebar_toggle {
            header.pack_start(toggle);
        }
        let select_cancel = gtk::Button::builder()
            .label(tr("_Cancel"))
            .use_underline(true)
            .visible(false)
            .build();
        header.pack_start(&select_cancel);
        let select_all = gtk::Button::builder()
            .label(tr("Select _All"))
            .use_underline(true)
            .visible(false)
            .build();
        header.pack_end(&select_all);

        let normal_end = gtk::Box::builder().spacing(6).build();
        let view_menu = view_menu();
        let view_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&view_menu)
            .tooltip_text(tr("View Options"))
            .build();
        let select_button = gtk::Button::builder()
            .icon_name("selection-mode-symbolic")
            .tooltip_text(tr("Select Items"))
            .build();
        normal_end.append(&view_button);
        normal_end.append(&select_button);
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(tr("Search"))
            .visible(options.searchable)
            .build();
        normal_end.append(&search_button);
        header.pack_end(&normal_end);

        // Search.
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(tr("Search tags, places, cameras, dates…"))
            .hexpand(true)
            .build();
        let help = gtk::MenuButton::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text(tr("Search Tips"))
            .popover(&search_help())
            .css_classes(["flat"])
            .build();
        let search_box = gtk::Box::builder().spacing(6).build();
        search_box.append(&search_entry);
        search_box.append(&help);
        let suggestions = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(12)
            .visible(false)
            .build();
        let search_column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        search_column.append(&search_box);
        search_column.append(&suggestions);
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .child(&search_column)
            .build();
        let search_bar = gtk::SearchBar::builder()
            .child(&clamp)
            .show_close_button(false)
            .build();
        search_bar.connect_entry(&search_entry);
        search_bar
            .bind_property("search-mode-enabled", &search_button, "active")
            .bidirectional()
            .sync_create()
            .build();

        // Selection bar.
        let selection_bar = gtk::ActionBar::new();
        let favorite_button = gtk::Button::builder()
            .icon_name("mn-heart-outline-symbolic")
            .action_name("sel.favorite")
            .tooltip_text(tr("Favorite"))
            .build();
        let selection_revealer = gtk::Revealer::builder()
            .child(&selection_bar)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .build();

        let root = adw::ToolbarView::builder()
            .top_bar_style(adw::ToolbarStyle::Raised)
            .build();
        root.add_top_bar(&header);
        root.add_top_bar(&search_bar);
        root.set_content(Some(&stack));
        root.add_bottom_bar(&selection_revealer);

        let start_toggle = options.sidebar_toggle.clone();
        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            kind: options.kind,
            base_view: RefCell::new(options.view),
            search_query: RefCell::default(),
            root,
            title,
            header,
            grid,
            stack,
            empty,
            search_bar,
            search_entry,
            suggestions,
            selection_bar,
            selection_revealer,
            favorite_button,
            normal_end,
            select_cancel,
            select_all,
            start_toggle,
            actions: gio::SimpleActionGroup::new(),
            empty_text: (
                options.empty_title,
                options.empty_description,
                options.empty_icon,
            ),
            generation: Cell::new(0),
            loaded: Cell::new(false),
            stale: Cell::new(true),
            reload_debounce: Debounce::new(250),
            suggest_debounce: Debounce::new(150),
            on_selection_title: RefCell::new(options.title),
            subtitle: RefCell::default(),
        });
        this.setup(&select_button, &search_button);
        this
    }

    pub fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// Wraps the page for pushing onto a navigation view.
    pub fn navigation_page(self: &Rc<Self>) -> adw::NavigationPage {
        let page = adw::NavigationPage::builder()
            .title(self.on_selection_title.borrow().as_str())
            .child(&self.root)
            .build();
        // Keep the page alive as long as it is shown.
        let keep = Rc::clone(self);
        page.connect_hidden(move |_| {
            let _ = &keep;
        });
        let this = Rc::clone(self);
        page.connect_shown(move |_| this.shown_now());
        page
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_title(title);
        *self.on_selection_title.borrow_mut() = title.to_owned();
    }

    pub fn set_view(self: &Rc<Self>, view: View) {
        *self.base_view.borrow_mut() = view;
        self.loaded.set(false);
        self.reload();
    }

    /// Extra buttons in the header, such as an album's menu.
    pub fn add_header_widget(&self, widget: &impl IsA<gtk::Widget>) {
        self.normal_end.prepend(widget);
    }

    fn setup(self: &Rc<Self>, select_button: &gtk::Button, search_button: &gtk::ToggleButton) {
        let ctx = Rc::clone(&self.ctx);
        self.grid
            .set_drag_files(move |ids| actions::drag_paths(&ctx, ids));

        let weak = Rc::downgrade(self);
        self.grid.connect_item_activated(move |grid, index| {
            if let Some(this) = weak.upgrade() {
                let album = match this.kind {
                    TimelineKind::Album(album) => Some(album),
                    _ => None,
                };
                crate::viewer::open(
                    &this.ctx,
                    grid,
                    index,
                    album,
                    this.kind == TimelineKind::Trash,
                );
            }
        });
        let weak = Rc::downgrade(self);
        self.grid.connect_selection_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.selection_changed();
            }
        });
        let weak = Rc::downgrade(self);
        self.grid.connect_context_menu(move |grid, x, y, index| {
            if let Some(this) = weak.upgrade() {
                if index.is_some() {
                    this.popup_menu(grid, x, y);
                }
            }
        });
        let grid = self.grid.downgrade();
        let ctx_ref = Rc::downgrade(&self.ctx);
        self.ctx.connect_config_changed(move || {
            if let (Some(grid), Some(ctx)) = (grid.upgrade(), ctx_ref.upgrade()) {
                grid.set_square(ctx.config.borrow().square_tiles);
            }
        });
        let ctx = Rc::clone(&self.ctx);
        self.grid.connect_zoom_changed(move |_, zoom| {
            let mut config = ctx.config.borrow_mut();
            config.zoom = zoom;
            config.save();
        });

        let grid = self.grid.clone();
        select_button.connect_clicked(move |_| grid.set_selection_mode(true));
        let grid = self.grid.clone();
        self.select_cancel
            .connect_clicked(move |_| grid.set_selection_mode(false));
        let grid = self.grid.clone();
        self.select_all.connect_clicked(move |_| grid.select_all());

        // Typing anywhere on the page starts a search.
        if search_button.is_visible() {
            self.search_bar.set_key_capture_widget(Some(&self.root));
        }
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |entry| {
            if let Some(this) = weak.upgrade() {
                let text = entry.text().to_string();
                *this.search_query.borrow_mut() = Query::parse(&text);
                this.reload();
                this.suggest(&text);
            }
        });
        let weak = Rc::downgrade(self);
        self.search_bar
            .connect_search_mode_enabled_notify(move |bar| {
                if let Some(this) = weak.upgrade() {
                    if !bar.is_search_mode() && !this.search_entry.text().is_empty() {
                        this.search_entry.set_text("");
                    }
                }
            });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_activate(move |_| {
            if let Some(this) = weak.upgrade() {
                this.grid.grab_focus();
            }
        });

        self.install_actions();
        self.build_selection_bar();

        // Files dropped from a file manager are imported.
        if self.kind == TimelineKind::Library {
            let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
            let ctx = Rc::clone(&self.ctx);
            drop.connect_drop(move |_, value, _, _| {
                let Ok(list) = value.get::<gdk::FileList>() else {
                    return false;
                };
                let paths: Vec<_> = list.files().iter().filter_map(gio::File::path).collect();
                if paths.is_empty() {
                    return false;
                }
                crate::dialogs::import::start(&ctx, paths);
                true
            });
            self.root.add_controller(drop);
        }

        let view_actions = gio::SimpleActionGroup::new();
        let grid = self.grid.clone();
        let zoom_in = gio::SimpleAction::new("zoom-in", None);
        zoom_in.connect_activate(move |_, _| grid.zoom_in());
        let grid = self.grid.clone();
        let zoom_out = gio::SimpleAction::new("zoom-out", None);
        zoom_out.connect_activate(move |_, _| grid.zoom_out());
        let square = gio::SimpleAction::new_stateful(
            "square",
            None,
            &self.ctx.config.borrow().square_tiles.to_variant(),
        );
        let grid = self.grid.clone();
        let ctx = Rc::clone(&self.ctx);
        square.connect_activate(move |action, _| {
            let value = !action
                .state()
                .and_then(|s| s.get::<bool>())
                .unwrap_or(false);
            action.set_state(&value.to_variant());
            grid.set_square(value);
            ctx.config.borrow_mut().square_tiles = value;
            ctx.config_changed();
        });
        view_actions.add_action(&zoom_in);
        view_actions.add_action(&zoom_out);
        view_actions.add_action(&square);
        self.root.insert_action_group("view", Some(&view_actions));
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, run: SelectionHandler<Self>| {
            let weak = Rc::downgrade(self);
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    let ids = this.grid.selection();
                    if !ids.is_empty() {
                        run(&this, ids);
                    }
                }
            });
            self.actions.add_action(&action);
        };
        add(
            "favorite",
            Box::new(|this, ids| {
                let all = this.all_selected_have(flags::FAVORITE);
                actions::set_favorite(&this.ctx, ids, !all);
            }),
        );
        add(
            "trash",
            Box::new(|this, ids| {
                this.grid.set_selection_mode(false);
                actions::trash(&this.ctx, ids);
            }),
        );
        add(
            "restore",
            Box::new(|this, ids| {
                this.grid.set_selection_mode(false);
                actions::restore(&this.ctx, ids);
            }),
        );
        add(
            "delete-forever",
            Box::new(|this, ids| actions::delete_forever(&this.ctx, ids)),
        );
        add(
            "rotate-left",
            Box::new(|this, ids| actions::rotate(&this.ctx, ids, -90)),
        );
        add(
            "rotate-right",
            Box::new(|this, ids| actions::rotate(&this.ctx, ids, 90)),
        );
        add(
            "hide",
            Box::new(|this, ids| {
                let hidden = this.all_selected_have(flags::HIDDEN);
                this.grid.set_selection_mode(false);
                actions::set_hidden(&this.ctx, ids, !hidden);
            }),
        );
        add(
            "tag",
            Box::new(|this, ids| crate::dialogs::tags::show(&this.ctx, ids)),
        );
        add(
            "album",
            Box::new(|this, ids| crate::dialogs::albums::show(&this.ctx, ids)),
        );
        add(
            "date",
            Box::new(|this, ids| crate::dialogs::date::show(&this.ctx, ids)),
        );
        add(
            "export",
            Box::new(|this, ids| crate::dialogs::export::show(&this.ctx, ids)),
        );
        add(
            "copy",
            Box::new(|this, ids| actions::copy_files(&this.ctx, ids)),
        );
        add(
            "show-in-files",
            Box::new(|this, ids| actions::show_in_files(&this.ctx, ids[0])),
        );
        add(
            "open-with",
            Box::new(|this, ids| actions::open_with(&this.ctx, ids[0])),
        );
        add(
            "info",
            Box::new(|this, ids| {
                if let Some(index) = this.grid.index_of(ids[0]) {
                    let album = match this.kind {
                        TimelineKind::Album(album) => Some(album),
                        _ => None,
                    };
                    crate::viewer::open_with_info(&this.ctx, &this.grid, index, album);
                }
            }),
        );
        if let TimelineKind::Album(album) = self.kind {
            add(
                "remove-from-album",
                Box::new(move |this, ids| {
                    actions::remove_from_album(&this.ctx, album, ids, true);
                }),
            );
            add(
                "set-cover",
                Box::new(move |this, ids| actions::set_cover(&this.ctx, album, ids[0])),
            );
        }
        for rating in 0..=5u8 {
            add(
                &format!("rate-{rating}"),
                Box::new(move |this, ids| actions::set_rating(&this.ctx, ids, rating)),
            );
        }
        let grid = self.grid.clone();
        let select_all = gio::SimpleAction::new("select-all", None);
        select_all.connect_activate(move |_, _| grid.select_all());
        self.actions.add_action(&select_all);
        let grid = self.grid.clone();
        let select_none = gio::SimpleAction::new("select-none", None);
        select_none.connect_activate(move |_, _| grid.set_selection_mode(false));
        self.actions.add_action(&select_none);
        self.root.insert_action_group("sel", Some(&self.actions));

        // Single-key shortcuts while the grid has focus.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Local);
        let trash_or_restore = if self.kind == TimelineKind::Trash {
            "sel.restore"
        } else {
            "sel.trash"
        };
        for (trigger, action) in [
            ("KP_Delete", trash_or_restore),
            ("Delete", trash_or_restore),
            ("f", "sel.favorite"),
            ("t", "sel.tag"),
            ("<Alt>Return", "sel.info"),
            ("<Control>c", "sel.copy"),
            ("<Control>r", "sel.rotate-right"),
            ("<Control><Shift>r", "sel.rotate-left"),
            ("<Control>e", "sel.export"),
            ("<Control>h", "sel.hide"),
            ("<Control>d", "sel.date"),
            ("<Control><Shift>a", "sel.select-none"),
            ("0", "sel.rate-0"),
            ("1", "sel.rate-1"),
            ("2", "sel.rate-2"),
            ("3", "sel.rate-3"),
            ("4", "sel.rate-4"),
            ("5", "sel.rate-5"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.grid.add_controller(shortcuts);
    }

    fn build_selection_bar(&self) {
        let button = |icon: &str, action: &str, tooltip: String| {
            gtk::Button::builder()
                .icon_name(icon)
                .action_name(action)
                .tooltip_text(tooltip)
                .build()
        };
        if self.kind == TimelineKind::Trash {
            let restore = gtk::Button::builder()
                .label(tr("_Restore"))
                .use_underline(true)
                .action_name("sel.restore")
                .css_classes(["suggested-action"])
                .build();
            let delete = gtk::Button::builder()
                .label(tr("_Remove From Library"))
                .use_underline(true)
                .action_name("sel.delete-forever")
                .css_classes(["destructive-action"])
                .build();
            self.selection_bar.pack_start(&restore);
            self.selection_bar.pack_end(&delete);
            return;
        }
        self.selection_bar.pack_start(&self.favorite_button);
        self.selection_bar.pack_start(&button(
            "view-grid-symbolic",
            "sel.album",
            tr("Add to Album"),
        ));
        self.selection_bar
            .pack_start(&button("mn-tag-symbolic", "sel.tag", tr("Tags")));
        self.selection_bar
            .pack_start(&button("send-to-symbolic", "sel.export", tr("Export")));
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&self.selection_menu(false))
            .tooltip_text(tr("More"))
            .direction(gtk::ArrowType::Up)
            .build();
        self.selection_bar.pack_end(&more);
        self.selection_bar.pack_end(&button(
            "user-trash-symbolic",
            "sel.trash",
            tr("Move to Trash"),
        ));
        if matches!(self.kind, TimelineKind::Album(_)) {
            self.selection_bar.pack_end(&button(
                "list-remove-symbolic",
                "sel.remove-from-album",
                tr("Remove From Album"),
            ));
        }
    }

    /// The menu of actions on selected items. The context menu has the
    /// common actions too; the selection bar shows those as buttons.
    fn selection_menu(&self, full: bool) -> gio::Menu {
        let menu = gio::Menu::new();
        if self.kind == TimelineKind::Trash {
            menu.append(Some(&tr("_Restore")), Some("sel.restore"));
            menu.append(
                Some(&tr("_Remove From Library…")),
                Some("sel.delete-forever"),
            );
            return menu;
        }
        if full {
            let common = gio::Menu::new();
            common.append(Some(&tr("_Open")), Some("sel.info"));
            common.append(Some(&tr("_Favorite")), Some("sel.favorite"));
            common.append(Some(&tr("Add to _Album…")), Some("sel.album"));
            common.append(Some(&tr("_Tags…")), Some("sel.tag"));
            menu.append_section(None, &common);
        }
        let edit = gio::Menu::new();
        edit.append(Some(&tr("Rotate _Left")), Some("sel.rotate-left"));
        edit.append(Some(&tr("Rotate _Right")), Some("sel.rotate-right"));
        edit.append(Some(&tr("Change _Date…")), Some("sel.date"));
        let rating = gio::Menu::new();
        rating.append(Some(&tr("No Rating")), Some("sel.rate-0"));
        for stars in 1..=5u8 {
            rating.append(
                Some(&"★".repeat(stars as usize)),
                Some(&format!("sel.rate-{stars}")),
            );
        }
        edit.append_submenu(Some(&tr("Rati_ng")), &rating);
        let hidden_label = if self.base_view.borrow().query.wants_hidden() {
            tr("_Unhide")
        } else {
            tr("_Hide")
        };
        edit.append(Some(&hidden_label), Some("sel.hide"));
        menu.append_section(None, &edit);
        if let TimelineKind::Album(_) = self.kind {
            let album = gio::Menu::new();
            album.append(Some(&tr("Use as Album _Cover")), Some("sel.set-cover"));
            album.append(
                Some(&tr("Re_move From Album")),
                Some("sel.remove-from-album"),
            );
            menu.append_section(None, &album);
        }
        let files = gio::Menu::new();
        files.append(Some(&tr("_Copy")), Some("sel.copy"));
        files.append(Some(&tr("_Export…")), Some("sel.export"));
        files.append(Some(&tr("Show in _Files")), Some("sel.show-in-files"));
        files.append(Some(&tr("Open _With…")), Some("sel.open-with"));
        menu.append_section(None, &files);
        if full {
            let danger = gio::Menu::new();
            danger.append(Some(&tr("Move to _Trash")), Some("sel.trash"));
            menu.append_section(None, &danger);
        }
        menu
    }

    fn popup_menu(&self, grid: &PhotoGrid, x: f64, y: f64) {
        let popover = gtk::PopoverMenu::from_model(Some(&self.selection_menu(true)));
        popover.set_parent(grid);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        #[allow(clippy::cast_possible_truncation)]
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    fn all_selected_have(&self, flag: u16) -> bool {
        let selected: std::collections::HashSet<AssetId> =
            self.grid.selection().into_iter().collect();
        let items = self.grid.items();
        let mut any = false;
        for item in items.iter().filter(|item| selected.contains(&item.id)) {
            any = true;
            if !item.has(flag) {
                return false;
            }
        }
        any
    }

    fn selection_changed(&self) {
        let selecting = self.grid.is_selection_mode();
        let count = self.grid.selection_count() as u64;
        self.selection_revealer.set_reveal_child(selecting);
        self.normal_end.set_visible(!selecting);
        self.select_cancel.set_visible(selecting);
        self.select_all.set_visible(selecting);
        if let Some(toggle) = &self.start_toggle {
            toggle.set_visible(!selecting && toggle.is_visible());
        }
        if selecting {
            self.header.add_css_class("selection-mode");
            self.title.set_subtitle("");
            self.title.set_title(&if count == 0 {
                tr("Select Items")
            } else {
                trn("{} Selected", "{} Selected", count)
            });
        } else {
            self.header.remove_css_class("selection-mode");
            self.title.set_subtitle(&self.subtitle.borrow());
            self.title.set_title(&self.on_selection_title.borrow());
        }
        for action in self.actions.list_actions() {
            if let Some(action) = self.actions.lookup_action(&action) {
                if let Ok(action) = action.downcast::<gio::SimpleAction>() {
                    let always = matches!(action.name().as_str(), "select-all" | "select-none");
                    action.set_enabled(always || count > 0);
                }
            }
        }
        let favorite = self.all_selected_have(flags::FAVORITE);
        self.favorite_button.set_icon_name(if favorite {
            "mn-heart-filled-symbolic"
        } else {
            "mn-heart-outline-symbolic"
        });
        self.favorite_button.set_tooltip_text(Some(&if favorite {
            tr("Remove From Favorites")
        } else {
            tr("Add to Favorites")
        }));
    }

    fn effective_view(&self) -> View {
        let mut view = self.base_view.borrow().clone();
        for term in &self.search_query.borrow().terms {
            view.query.push(term.clone());
        }
        view
    }

    pub fn reload(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.stale.set(false);
        let view = self.effective_view();
        let weak = Rc::downgrade(self);
        let ctx = Rc::clone(&self.ctx);
        util::spawn(async move {
            let items = ctx
                .call(move |library| library.read(|db| db.timeline(&view)))
                .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.generation.get() != generation {
                return;
            }
            match items {
                Ok(items) => this.apply(items),
                Err(error) => ctx.error(&tr("Could not load photos"), &error),
            }
        });
    }

    fn apply(&self, items: Vec<TimelineItem>) {
        let first_load = !self.loaded.replace(true);
        let videos = items.iter().filter(|item| item.has(flags::VIDEO)).count() as u64;
        let photos = items.len() as u64 - videos;
        let subtitle = if items.is_empty() {
            String::new()
        } else {
            util::photos_and_videos(photos, videos)
        };
        if !self.grid.is_selection_mode() {
            self.title.set_subtitle(&subtitle);
        }
        *self.subtitle.borrow_mut() = subtitle;
        let searching = !self.search_query.borrow().is_empty();
        if items.is_empty() {
            if searching {
                self.empty.set_icon_name(Some("system-search-symbolic"));
                self.empty.set_title(&tr("No Results"));
                self.empty
                    .set_description(Some(&tr("Try other words, or remove a filter.")));
                if let Some(child) = self.empty.child() {
                    child.set_visible(false);
                }
            } else {
                let (title, description, icon) = &self.empty_text;
                self.empty.set_icon_name(Some(icon));
                self.empty.set_title(title);
                self.empty.set_description(Some(description));
                if let Some(child) = self.empty.child() {
                    child.set_visible(true);
                }
            }
            self.stack.set_visible_child_name("empty");
        } else {
            self.stack.set_visible_child_name("grid");
        }
        self.grid.set_items(Rc::new(items));
        if first_load || searching {
            self.grid.scroll_to_top();
        }
        if first_load && !searching && self.grid.is_mapped() {
            self.grid.grab_focus();
        }
    }

    fn shown_now(self: &Rc<Self>) {
        if self.stale.get() || !self.loaded.get() {
            self.reload();
        }
    }

    fn suggest(self: &Rc<Self>, text: &str) {
        let word = text
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or_default()
            .to_owned();
        if word.chars().count() < 2 || word.contains(':') {
            self.suggestions.remove_all();
            self.suggestions.set_visible(false);
            return;
        }
        let weak = Rc::downgrade(self);
        let ctx = Rc::clone(&self.ctx);
        self.suggest_debounce.call(move || {
            util::spawn(async move {
                let prefix = word.clone();
                let found = ctx
                    .call(move |library| {
                        library.read(|db| {
                            let mut found = Vec::new();
                            for facet in db.tag_suggestions(&prefix, 4)? {
                                found.push(("tag", facet));
                            }
                            for facet in db.places(&prefix, 4)? {
                                found.push(("place", facet));
                            }
                            for facet in db.album_suggestions(&prefix, 3)? {
                                found.push(("album", facet));
                            }
                            for facet in db.cameras(&prefix, 3)? {
                                found.push(("camera", facet));
                            }
                            Ok(found)
                        })
                    })
                    .await
                    .unwrap_or_default();
                let Some(this) = weak.upgrade() else {
                    return;
                };
                this.suggestions.remove_all();
                this.suggestions.set_visible(!found.is_empty());
                for (key, facet) in found {
                    let label = match key {
                        "tag" => format!("{} {}", tr("Tag:"), facet.value),
                        "place" => format!("{} {}", tr("Place:"), facet.value),
                        "album" => format!("{} {}", tr("Album:"), facet.value),
                        _ => format!("{} {}", tr("Camera:"), facet.value),
                    };
                    let chip = gtk::Button::builder()
                        .label(format!("{label}  {}", util::number(facet.count)))
                        .css_classes(["pill", "small", "tag-chip"])
                        .build();
                    let entry = this.search_entry.clone();
                    let word = word.clone();
                    chip.connect_clicked(move |_| {
                        let text = entry.text().to_string();
                        let base = text.strip_suffix(word.as_str()).unwrap_or(&text);
                        let value = if facet.value.contains(char::is_whitespace) {
                            format!("\"{}\"", facet.value)
                        } else {
                            facet.value.clone()
                        };
                        entry.set_text(&format!("{base}{key}:{value} "));
                        entry.set_position(-1);
                        entry.grab_focus();
                    });
                    this.suggestions.append(&chip);
                }
            });
        });
    }
}

impl Section for TimelinePage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() || !self.loaded.get() {
            // Sections are created through Rc; reload needs one.
            self.request_reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        let relevant = match event {
            Event::ContentChanged | Event::AssetsChanged(_) => true,
            Event::AlbumsChanged => matches!(self.kind, TimelineKind::Album(_)),
            Event::TagsChanged => {
                !self.base_view.borrow().query.is_empty() || !self.search_query.borrow().is_empty()
            }
            _ => false,
        };
        if !relevant {
            return;
        }
        if visible && self.root.is_mapped() {
            self.request_reload();
        } else {
            self.stale.set(true);
        }
    }

    fn search(&self, text: &str) {
        self.search_bar.set_search_mode(true);
        if !text.is_empty() {
            self.search_entry.set_text(text);
        }
        self.search_entry.grab_focus();
    }

    fn command(&self, name: &str) {
        match name {
            "select-all" => self.grid.select_all(),
            "zoom-in" => self.grid.zoom_in(),
            "zoom-out" => self.grid.zoom_out(),
            _ => {}
        }
    }
}

impl TimelinePage {
    /// Reloads soon; used where only `&self` is available.
    fn request_reload(&self) {
        self.stale.set(true);
        let this = self.this.clone();
        self.reload_debounce.call(move || {
            if let Some(this) = this.upgrade() {
                this.reload();
            }
        });
    }
}

type SelectionHandler<T> = Box<dyn Fn(&Rc<T>, Vec<AssetId>)>;

fn view_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let zoom = gio::Menu::new();
    zoom.append(Some(&tr("Zoom _In")), Some("view.zoom-in"));
    zoom.append(Some(&tr("Zoom _Out")), Some("view.zoom-out"));
    menu.append_section(None, &zoom);
    let shape = gio::Menu::new();
    shape.append(Some(&tr("_Square Tiles")), Some("view.square"));
    menu.append_section(None, &shape);
    menu
}

fn search_help() -> gtk::Popover {
    let rows = [
        (
            "beach",
            tr("Words in names, tags, places, cameras and captions"),
        ),
        ("tag:family", tr("Items with a tag")),
        ("album:\"Summer 2023\"", tr("Items in an album")),
        ("place:Paris", tr("Taken in a place")),
        ("2023, july, 2023-07", tr("Taken in a year, month or day")),
        ("camera:pixel", tr("Taken with a camera")),
        (
            "is:fav, is:video, is:live",
            tr("Favorites, videos, Live Photos"),
        ),
        ("rating>=4", tr("Rated four stars or more")),
        (
            "has:location, is:hidden",
            tr("With a location; hidden items"),
        ),
        ("-tag:work", tr("A leading minus excludes")),
    ];
    let grid = gtk::Grid::builder()
        .row_spacing(6)
        .column_spacing(18)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    for (row, (example, meaning)) in rows.iter().enumerate() {
        let example = gtk::Label::builder()
            .label(*example)
            .xalign(0.0)
            .selectable(true)
            .css_classes(["monospace"])
            .build();
        let meaning = gtk::Label::builder()
            .label(meaning)
            .xalign(0.0)
            .wrap(true)
            .build();
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let row = row as i32;
        grid.attach(&example, 0, row, 1, 1);
        grid.attach(&meaning, 1, row, 1, 1);
    }
    gtk::Popover::builder().child(&grid).build()
}
