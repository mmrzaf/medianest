//! Albums: hand-picked collections and saved searches.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::Event;
use medianest_service::catalog::{Album, Scope, Sort, View};
use medianest_service::core::query::Query;
use medianest_service::core::{AlbumId, AlbumSort};

use super::Section;
use super::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::context::Ctx;
use crate::i18n::{tr, trf};
use crate::textures;
use crate::thumb::Thumb;
use crate::util::{self, Debounce};

const CARD_SIZE: i32 = 176;

pub struct AlbumsPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    flow: gtk::FlowBox,
    stack: gtk::Stack,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl AlbumsPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let new_menu = gtk::gio::Menu::new();
        new_menu.append(Some(&tr("_New Album…")), Some("albums.new"));
        new_menu.append(Some(&tr("New _Smart Album…")), Some("albums.new-smart"));
        let add = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            .menu_model(&new_menu)
            .tooltip_text(tr("New Album"))
            .build();
        let header = adw::HeaderBar::new();
        header.pack_start(&toggle);
        header.pack_end(&add);

        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .column_spacing(18)
            .row_spacing(18)
            .min_children_per_line(1)
            .max_children_per_line(12)
            .valign(gtk::Align::Start)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&flow)
            .build();
        let new_button = gtk::Button::builder()
            .label(tr("_New Album…"))
            .use_underline(true)
            .action_name("albums.new")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("view-grid-symbolic")
            .title(tr("No Albums"))
            .description(tr(
                "Albums collect photos you choose. Smart albums fill themselves from a search, \
                 such as “tag:family 2023”.",
            ))
            .child(&new_button)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("albums"));
        stack.add_named(&empty, Some("empty"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let root = adw::NavigationPage::builder()
            .title(tr("Albums"))
            .tag("albums")
            .child(&toolbar)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&root);

        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            nav,
            flow,
            stack,
            stale: Cell::new(true),
            debounce: Debounce::new(300),
        });
        this.install_actions();
        this
    }

    fn install_actions(&self) {
        let group = gtk::gio::SimpleActionGroup::new();
        let new = gtk::gio::SimpleAction::new("new", None);
        let weak = self.this.clone();
        new.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.create(false);
            }
        });
        let smart = gtk::gio::SimpleAction::new("new-smart", None);
        let weak = self.this.clone();
        smart.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.create(true);
            }
        });
        group.add_action(&new);
        group.add_action(&smart);
        self.nav.insert_action_group("albums", Some(&group));
    }

    fn create(&self, smart: bool) {
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        album_form(&self.ctx, smart, None, None, move |name, query| {
            let ctx = Rc::clone(&ctx);
            let weak = weak.clone();
            util::spawn(async move {
                let album_name = name.clone();
                match ctx
                    .call(move |library| library.create_album(&album_name, query.as_deref()))
                    .await
                {
                    Ok(id) => {
                        if let Some(this) = weak.upgrade() {
                            this.reload();
                            this.open_album_by_id(id);
                        }
                    }
                    Err(error) => ctx.error(&tr("Could not create the album"), &error),
                }
            });
        });
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let albums = ctx
                .call(|library| {
                    let albums = library.read(|db| db.albums())?;
                    let mut result = Vec::new();
                    for album in albums {
                        let count = match &album.query {
                            Some(query) => library.read(|db| {
                                db.count(&View::library().with_query(Query::parse(query)))
                            })?,
                            None => album.count,
                        };
                        let cover = library.album_cover(&album)?;
                        let pixels = cover
                            .and_then(|id| library.read(|db| db.asset(id)).ok())
                            .and_then(|asset| {
                                textures::decode_jpeg(&library.thumbnail_path(asset.uuid)).ok()
                            });
                        result.push((album, count, pixels));
                    }
                    Ok(result)
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match albums {
                Ok(albums) => {
                    this.flow.remove_all();
                    this.stack.set_visible_child_name(if albums.is_empty() {
                        "empty"
                    } else {
                        "albums"
                    });
                    for (album, count, pixels) in albums {
                        this.flow.append(&this.card(
                            album,
                            count,
                            pixels.map(textures::Pixels::texture),
                        ));
                    }
                }
                Err(error) => this.ctx.error(&tr("Could not load albums"), &error),
            }
        });
    }

    fn card(&self, album: Album, count: u64, cover: Option<gtk::gdk::Texture>) -> gtk::Widget {
        let picture = Thumb::new(CARD_SIZE, 12.0);
        picture.add_css_class("album-cover");
        picture.set_texture(cover.as_ref());
        let frame = gtk::Overlay::builder().child(&picture).build();
        if cover.is_none() {
            let icon = gtk::Image::builder()
                .icon_name(if album.is_smart() {
                    "system-search-symbolic"
                } else {
                    "view-grid-symbolic"
                })
                .pixel_size(48)
                .css_classes(["dim-label"])
                .build();
            frame.add_overlay(&icon);
        }
        if album.is_smart() {
            let badge = gtk::Image::builder()
                .icon_name("system-search-symbolic")
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .margin_top(8)
                .margin_end(8)
                .css_classes(["osd", "circular", "album-badge"])
                .tooltip_text(tr("Smart Album"))
                .build();
            frame.add_overlay(&badge);
        }
        let name = gtk::Label::builder()
            .label(&album.name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .css_classes(["heading"])
            .build();
        let detail = gtk::Label::builder()
            .label(util::items(count))
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        content.append(&frame);
        content.append(&name);
        content.append(&detail);
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["flat", "album-card"])
            .tooltip_text(album.query.clone().unwrap_or_default())
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {}",
            album.name,
            util::items(count)
        ))]);
        let weak = self.this.clone();
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open_album(&album);
            }
        });
        button.upcast()
    }

    fn open_album_by_id(&self, id: AlbumId) {
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            if let Ok(album) = ctx
                .call(move |library| library.read(|db| db.album(id)))
                .await
            {
                if let Some(this) = weak.upgrade() {
                    this.open_album(&album);
                }
            }
        });
    }

    fn open_album(&self, album: &Album) {
        let (view, kind) = album_view(album);
        let page = TimelinePage::new(
            Rc::clone(&self.ctx),
            TimelineOptions {
                title: album.name.clone(),
                view,
                kind,
                searchable: false,
                empty_icon: "view-grid-symbolic",
                empty_title: tr("Empty Album"),
                empty_description: if album.is_smart() {
                    tr("Nothing matches this album’s search yet.")
                } else {
                    tr("Select photos anywhere and choose “Add to Album”.")
                },
                sidebar_toggle: None,
            },
        );
        page.add_header_widget(&self.album_menu(album, &page));
        self.nav.push(&page.navigation_page());
    }

    fn album_menu(&self, album: &Album, page: &Rc<TimelinePage>) -> gtk::MenuButton {
        let menu = gtk::gio::Menu::new();
        menu.append(Some(&tr("_Rename…")), Some("album.rename"));
        if album.is_smart() {
            menu.append(Some(&tr("Edit _Search…")), Some("album.edit"));
        } else {
            let sort = gtk::gio::Menu::new();
            sort.append(Some(&tr("_Custom Order")), Some("album.sort::manual"));
            sort.append(Some(&tr("_Newest First")), Some("album.sort::newest"));
            sort.append(Some(&tr("_Oldest First")), Some("album.sort::oldest"));
            menu.append_section(Some(&tr("Order")), &sort);
        }
        let danger = gtk::gio::Menu::new();
        danger.append(Some(&tr("_Delete Album…")), Some("album.delete"));
        menu.append_section(None, &danger);
        let button = gtk::MenuButton::builder()
            .icon_name("document-edit-symbolic")
            .menu_model(&menu)
            .tooltip_text(tr("Album"))
            .build();

        let group = gtk::gio::SimpleActionGroup::new();
        let album = Rc::new(std::cell::RefCell::new(album.clone()));

        let rename = gtk::gio::SimpleAction::new("rename", None);
        let (ctx, current, page_ref) =
            (Rc::clone(&self.ctx), Rc::clone(&album), Rc::downgrade(page));
        rename.connect_activate(move |_, _| {
            let snapshot = current.borrow().clone();
            let (ctx2, current, page_ref) =
                (Rc::clone(&ctx), Rc::clone(&current), page_ref.clone());
            album_form(&ctx, false, Some(&snapshot.name), None, move |name, _| {
                let (ctx, current, page_ref) =
                    (Rc::clone(&ctx2), Rc::clone(&current), page_ref.clone());
                let id = snapshot.id;
                util::spawn(async move {
                    let new_name = name.clone();
                    match ctx
                        .call(move |library| library.rename_album(id, &new_name))
                        .await
                    {
                        Ok(()) => {
                            current.borrow_mut().name = name.clone();
                            if let Some(page) = page_ref.upgrade() {
                                page.set_title(&name);
                            }
                        }
                        Err(error) => ctx.error(&tr("Could not rename the album"), &error),
                    }
                });
            });
        });
        group.add_action(&rename);

        let edit = gtk::gio::SimpleAction::new("edit", None);
        let (ctx, current, page_ref) =
            (Rc::clone(&self.ctx), Rc::clone(&album), Rc::downgrade(page));
        edit.connect_activate(move |_, _| {
            let snapshot = current.borrow().clone();
            let (ctx2, current, page_ref) =
                (Rc::clone(&ctx), Rc::clone(&current), page_ref.clone());
            album_form(
                &ctx,
                true,
                Some(&snapshot.name),
                snapshot.query.as_deref(),
                move |name, query| {
                    let (ctx, current, page_ref) =
                        (Rc::clone(&ctx2), Rc::clone(&current), page_ref.clone());
                    let id = snapshot.id;
                    util::spawn(async move {
                        let (new_name, new_query) =
                            (name.clone(), query.clone().unwrap_or_default());
                        let result = ctx
                            .call(move |library| {
                                library.rename_album(id, &new_name)?;
                                library.set_album_query(id, &new_query)
                            })
                            .await;
                        match result {
                            Ok(()) => {
                                let mut album = current.borrow_mut();
                                album.name = name.clone();
                                album.query = query;
                                if let Some(page) = page_ref.upgrade() {
                                    page.set_title(&name);
                                    page.set_view(album_view(&album).0);
                                }
                            }
                            Err(error) => ctx.error(&tr("Could not change the album"), &error),
                        }
                    });
                },
            );
        });
        group.add_action(&edit);

        let current_sort = match album.borrow().sort {
            AlbumSort::Manual => "manual",
            AlbumSort::NewestFirst => "newest",
            AlbumSort::OldestFirst => "oldest",
        };
        let sort = gtk::gio::SimpleAction::new_stateful(
            "sort",
            Some(glib::VariantTy::STRING),
            &current_sort.to_variant(),
        );
        let (ctx, current, page_ref) =
            (Rc::clone(&self.ctx), Rc::clone(&album), Rc::downgrade(page));
        sort.connect_activate(move |action, value| {
            let Some(value) = value.and_then(|v| v.get::<String>()) else {
                return;
            };
            action.set_state(&value.to_variant());
            let sort = match value.as_str() {
                "newest" => AlbumSort::NewestFirst,
                "oldest" => AlbumSort::OldestFirst,
                _ => AlbumSort::Manual,
            };
            current.borrow_mut().sort = sort;
            if let Some(page) = page_ref.upgrade() {
                page.set_view(album_view(&current.borrow()).0);
            }
            let id = current.borrow().id;
            let ctx = Rc::clone(&ctx);
            util::spawn(async move {
                if let Err(error) = ctx
                    .call(move |library| library.set_album_sort(id, sort))
                    .await
                {
                    ctx.error(&tr("Could not change the order"), &error);
                }
            });
        });
        group.add_action(&sort);

        let delete = gtk::gio::SimpleAction::new("delete", None);
        let (ctx, current, nav) = (Rc::clone(&self.ctx), Rc::clone(&album), self.nav.clone());
        delete.connect_activate(move |_, _| {
            let album = current.borrow().clone();
            let (ctx2, nav) = (Rc::clone(&ctx), nav.clone());
            ctx.confirm(
                &trf("Delete “{}”?", &[&album.name]),
                &tr("Only the album is deleted. The photos stay in your library."),
                &tr("_Delete"),
                true,
                move || {
                    let ctx = ctx2;
                    nav.pop();
                    util::spawn(async move {
                        if let Err(error) = ctx
                            .call(move |library| library.delete_album(album.id))
                            .await
                        {
                            ctx.error(&tr("Could not delete the album"), &error);
                        }
                    });
                },
            );
        });
        group.add_action(&delete);
        page.root().insert_action_group("album", Some(&group));
        button
    }
}

fn album_view(album: &Album) -> (View, TimelineKind) {
    match &album.query {
        Some(query) => (
            View::library().with_query(Query::parse(query)),
            TimelineKind::Filtered,
        ),
        None => (
            View {
                scope: Scope::Album(album.id),
                query: Query::default(),
                sort: match album.sort {
                    AlbumSort::Manual => Sort::AlbumOrder,
                    AlbumSort::NewestFirst => Sort::NewestFirst,
                    AlbumSort::OldestFirst => Sort::OldestFirst,
                },
            },
            TimelineKind::Album(album.id),
        ),
    }
}

/// Asks for an album name, and a search for smart albums.
fn album_form(
    ctx: &Rc<Ctx>,
    smart: bool,
    name: Option<&str>,
    query: Option<&str>,
    done: impl Fn(String, Option<String>) + 'static,
) {
    let editing = name.is_some();
    let heading = match (smart, editing) {
        (false, false) => tr("New Album"),
        (true, false) => tr("New Smart Album"),
        (false, true) => tr("Rename Album"),
        (true, true) => tr("Edit Smart Album"),
    };
    let body = if smart {
        tr("A smart album shows everything that matches its search, now and in the future.")
    } else {
        String::new()
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
    let name_row = adw::EntryRow::builder()
        .title(tr("Name"))
        .text(name.unwrap_or_default())
        .activates_default(true)
        .build();
    let query_row = adw::EntryRow::builder()
        .title(tr("Search, e.g. tag:family place:Paris 2023"))
        .text(query.unwrap_or_default())
        .visible(smart)
        .build();
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.append(&name_row);
    list.append(&query_row);
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[
        ("cancel", &tr("_Cancel")),
        ("ok", &if editing { tr("_Save") } else { tr("_Create") }),
    ]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    let validate = {
        let dialog = dialog.clone();
        let (name_row, query_row) = (name_row.clone(), query_row.clone());
        move || {
            let ok = !name_row.text().trim().is_empty()
                && (!smart || !query_row.text().trim().is_empty());
            dialog.set_response_enabled("ok", ok);
        }
    };
    validate();
    let v = validate.clone();
    name_row.connect_changed(move |_| v());
    query_row.connect_changed(move |_| validate());
    dialog.connect_response(None, move |_, response| {
        if response == "ok" {
            let name = name_row.text().trim().to_owned();
            let query = smart.then(|| query_row.text().trim().to_owned());
            done(name, query);
        }
    });
    dialog.present(Some(&ctx.window));
}

impl Section for AlbumsPage {
    fn widget(&self) -> gtk::Widget {
        self.nav.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() {
            self.reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        if matches!(event, Event::AlbumsChanged | Event::ContentChanged) {
            self.stale.set(true);
            if visible {
                let weak = self.this.clone();
                self.debounce.call(move || {
                    if let Some(this) = weak.upgrade() {
                        this.reload();
                    }
                });
            }
        }
    }
}
