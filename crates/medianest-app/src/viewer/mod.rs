//! Looking at one photo or video at a time.

mod info;
mod zoom_image;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use medianest_service::AssetDetail;
use medianest_service::catalog::{TimelineItem, flags};
use medianest_service::core::{AlbumId, AssetId};

use crate::actions;
use crate::context::Ctx;
use crate::grid::PhotoGrid;
use crate::i18n::tr;
use crate::player::PlayerStream;
use crate::textures::{self, Pixels};
use crate::util;
use info::InfoPanel;
use zoom_image::ZoomImage;

/// Opens the viewer on the grid's item at `index`.
pub fn open(ctx: &Rc<Ctx>, grid: &PhotoGrid, index: usize, album: Option<AlbumId>, trash: bool) {
    if ctx.nav.find_page("viewer").is_some() {
        return;
    }
    let viewer = Viewer::new(Rc::clone(ctx), grid, index, album, trash);
    ctx.nav.push(&viewer.page);
}

/// Opens the viewer with the information panel shown.
pub fn open_with_info(ctx: &Rc<Ctx>, grid: &PhotoGrid, index: usize, album: Option<AlbumId>) {
    if ctx.nav.find_page("viewer").is_some() {
        return;
    }
    let viewer = Viewer::new(Rc::clone(ctx), grid, index, album, false);
    viewer.split.set_show_sidebar(true);
    ctx.nav.push(&viewer.page);
}

type Handler<T> = Box<dyn Fn(&Rc<T>, AssetId)>;

const PREFETCH: usize = 1;
const CONTROLS_TIMEOUT: Duration = Duration::from_millis(2500);

struct Viewer {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    items: RefCell<Rc<Vec<TimelineItem>>>,
    index: Cell<usize>,
    grid: glib::WeakRef<PhotoGrid>,
    album: Option<AlbumId>,
    trash: bool,
    page: adw::NavigationPage,
    toolbar: adw::ToolbarView,
    title: adw::WindowTitle,
    split: adw::OverlaySplitView,
    stack: gtk::Stack,
    image: ZoomImage,
    video: gtk::Video,
    live_picture: gtk::Picture,
    live_button: gtk::Button,
    missing: adw::StatusPage,
    previous: gtk::Button,
    next: gtk::Button,
    controls: Vec<gtk::Revealer>,
    zoom_label: gtk::Label,
    favorite: gtk::Button,
    info: Rc<InfoPanel>,
    detail: RefCell<Option<AssetDetail>>,
    previews: RefCell<HashMap<AssetId, gdk::Texture>>,
    generation: Cell<u64>,
    full_loading: Cell<bool>,
    hide_controls: RefCell<Option<glib::SourceId>>,
    slideshow: RefCell<Option<glib::SourceId>>,
    actions: gio::SimpleActionGroup,
    keep_alive: RefCell<Option<Rc<Self>>>,
}

impl Viewer {
    fn new(
        ctx: Rc<Ctx>,
        grid: &PhotoGrid,
        index: usize,
        album: Option<AlbumId>,
        trash: bool,
    ) -> Rc<Self> {
        let image = ZoomImage::default();
        let video = gtk::Video::builder()
            .autoplay(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        video.set_graphics_offload(gtk::GraphicsOffloadEnabled::Enabled);
        let missing = adw::StatusPage::builder()
            .icon_name("image-missing-symbolic")
            .title(tr("Cannot Show This Item"))
            .build();
        let live_picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .visible(false)
            .can_target(false)
            .build();
        let image_overlay = gtk::Overlay::builder().child(&image).build();
        image_overlay.add_overlay(&live_picture);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(100)
            .build();
        stack.add_named(&image_overlay, Some("image"));
        stack.add_named(&video, Some("video"));
        stack.add_named(&missing, Some("missing"));

        let osd_button = |icon: &str, tooltip: String, halign: gtk::Align| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .css_classes(["osd", "circular", "viewer-overlay-button"])
                .valign(gtk::Align::Center)
                .width_request(48)
                .height_request(48)
                .build();
            let revealer = gtk::Revealer::builder()
                .child(&button)
                .transition_type(gtk::RevealerTransitionType::Crossfade)
                .halign(halign)
                .valign(gtk::Align::Center)
                .margin_start(18)
                .margin_end(18)
                .build();
            (button, revealer)
        };
        let (previous, previous_revealer) =
            osd_button("go-previous-symbolic", tr("Previous"), gtk::Align::Start);
        let (next, next_revealer) = osd_button("go-next-symbolic", tr("Next"), gtk::Align::End);

        let live_content = gtk::Box::builder().spacing(6).build();
        live_content.append(&gtk::Image::from_icon_name("mn-live-symbolic"));
        live_content.append(&gtk::Label::new(Some(&tr("LIVE"))));
        let live_button = gtk::Button::builder()
            .child(&live_content)
            .tooltip_text(tr("Play Live Photo"))
            .css_classes(["osd", "pill", "live-badge"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(18)
            .margin_top(18)
            .visible(false)
            .build();

        let zoom_label = gtk::Label::builder()
            .width_chars(5)
            .css_classes(["numeric"])
            .build();
        let zoom_out = gtk::Button::builder()
            .icon_name("zoom-out-symbolic")
            .action_name("viewer.zoom-out")
            .tooltip_text(tr("Zoom Out"))
            .css_classes(["flat"])
            .build();
        let zoom_in = gtk::Button::builder()
            .icon_name("zoom-in-symbolic")
            .action_name("viewer.zoom-in")
            .tooltip_text(tr("Zoom In"))
            .css_classes(["flat"])
            .build();
        let zoom_fit = gtk::Button::builder()
            .icon_name("zoom-fit-best-symbolic")
            .action_name("viewer.zoom-fit")
            .tooltip_text(tr("Best Fit"))
            .css_classes(["flat"])
            .build();
        let zoom_box = gtk::Box::builder()
            .css_classes(["osd", "toolbar", "viewer-zoom"])
            .spacing(2)
            .build();
        zoom_box.append(&zoom_out);
        zoom_box.append(&zoom_label);
        zoom_box.append(&zoom_in);
        zoom_box.append(&zoom_fit);
        let zoom_revealer = gtk::Revealer::builder()
            .child(&zoom_box)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .margin_end(18)
            .margin_bottom(18)
            .build();

        let overlay = gtk::Overlay::builder().child(&stack).build();
        overlay.add_overlay(&previous_revealer);
        overlay.add_overlay(&next_revealer);
        overlay.add_overlay(&live_button);
        overlay.add_overlay(&zoom_revealer);
        overlay.add_css_class("viewer");

        let info = InfoPanel::new(Rc::clone(&ctx));
        let split = adw::OverlaySplitView::builder()
            .content(&overlay)
            .sidebar(info.widget())
            .sidebar_position(gtk::PackType::End)
            .show_sidebar(false)
            .min_sidebar_width(300.0)
            .max_sidebar_width(380.0)
            .build();

        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(tr("More"))
            .build();
        let info_button = gtk::ToggleButton::builder()
            .icon_name("mn-info-symbolic")
            .tooltip_text(tr("Information"))
            .build();
        split
            .bind_property("show-sidebar", &info_button, "active")
            .bidirectional()
            .sync_create()
            .build();
        let favorite = gtk::Button::builder()
            .icon_name("mn-heart-outline-symbolic")
            .action_name("viewer.favorite")
            .tooltip_text(tr("Add to Favorites"))
            .build();
        header.pack_end(&menu_button);
        header.pack_end(&info_button);
        if trash {
            header.pack_end(
                &gtk::Button::builder()
                    .label(tr("_Restore"))
                    .use_underline(true)
                    .action_name("viewer.restore")
                    .css_classes(["suggested-action"])
                    .build(),
            );
        } else {
            header.pack_end(
                &gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .action_name("viewer.trash")
                    .tooltip_text(tr("Move to Trash"))
                    .build(),
            );
            header.pack_end(&favorite);
        }

        let toolbar = adw::ToolbarView::builder()
            .content(&split)
            .extend_content_to_top_edge(false)
            .build();
        toolbar.add_top_bar(&header);
        let page = adw::NavigationPage::builder()
            .title(tr("Photo"))
            .tag("viewer")
            .child(&toolbar)
            .build();

        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            items: RefCell::new(grid.items()),
            index: Cell::new(index),
            grid: grid.downgrade(),
            album,
            trash,
            page,
            toolbar,
            title,
            split,
            stack,
            image,
            video,
            live_picture,
            live_button,
            missing,
            previous,
            next,
            controls: vec![previous_revealer, next_revealer, zoom_revealer],
            zoom_label,
            favorite,
            info,
            detail: RefCell::default(),
            previews: RefCell::default(),
            generation: Cell::new(0),
            full_loading: Cell::new(false),
            hide_controls: RefCell::default(),
            slideshow: RefCell::default(),
            actions: gio::SimpleActionGroup::new(),
            keep_alive: RefCell::default(),
        });
        menu_button.set_menu_model(Some(&this.menu()));
        this.setup(&overlay);
        this.show_current();
        this
    }

    fn menu(&self) -> gio::Menu {
        let menu = gio::Menu::new();
        let view = gio::Menu::new();
        view.append(Some(&tr("_Slideshow")), Some("viewer.slideshow"));
        view.append(Some(&tr("_Fullscreen")), Some("viewer.fullscreen"));
        menu.append_section(None, &view);
        if !self.trash {
            let edit = gio::Menu::new();
            edit.append(Some(&tr("Rotate _Left")), Some("viewer.rotate-left"));
            edit.append(Some(&tr("Rotate _Right")), Some("viewer.rotate-right"));
            edit.append(Some(&tr("Change _Date…")), Some("viewer.date"));
            edit.append(Some(&tr("Add to _Album…")), Some("viewer.album"));
            edit.append(Some(&tr("_Tags…")), Some("viewer.tag"));
            edit.append(Some(&tr("_Hide")), Some("viewer.hide"));
            menu.append_section(None, &edit);
            if self.album.is_some() {
                let album = gio::Menu::new();
                album.append(Some(&tr("Use as Album _Cover")), Some("viewer.set-cover"));
                album.append(
                    Some(&tr("Re_move From Album")),
                    Some("viewer.remove-from-album"),
                );
                menu.append_section(None, &album);
            }
        }
        let files = gio::Menu::new();
        files.append(Some(&tr("_Copy")), Some("viewer.copy"));
        files.append(Some(&tr("_Export…")), Some("viewer.export"));
        files.append(Some(&tr("Show in _Files")), Some("viewer.show-in-files"));
        files.append(Some(&tr("Open _With…")), Some("viewer.open-with"));
        menu.append_section(None, &files);
        menu
    }

    fn current(&self) -> Option<TimelineItem> {
        self.items.borrow().get(self.index.get()).copied()
    }

    fn setup(self: &Rc<Self>, overlay: &gtk::Overlay) {
        let weak = self.this.clone();
        self.previous.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.step(-1);
            }
        });
        let weak = self.this.clone();
        self.next.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.step(1);
            }
        });
        let weak = self.this.clone();
        self.image.connect_swipe(move |_, direction| {
            if let Some(this) = weak.upgrade() {
                this.step(direction as isize);
            }
        });
        let weak = self.this.clone();
        self.image.connect_zoom_changed(move |image| {
            if let Some(this) = weak.upgrade() {
                #[allow(clippy::cast_possible_truncation)]
                let percent = (image.zoom() * 100.0).round() as i64;
                this.zoom_label.set_label(&format!("{percent} %"));
                if image.needs_more_pixels() {
                    this.load_full_resolution();
                }
            }
        });
        let weak = self.this.clone();
        self.live_button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.play_live();
            }
        });

        // Show the overlay controls while the pointer moves.
        let motion = gtk::EventControllerMotion::new();
        let weak = self.this.clone();
        motion.connect_motion(move |_, _, y| {
            if let Some(this) = weak.upgrade() {
                this.show_controls();
                if this.ctx.window.is_fullscreen() {
                    this.toolbar.set_reveal_top_bars(y < 80.0);
                }
            }
        });
        overlay.add_controller(motion);

        self.install_actions();

        let weak = self.this.clone();
        self.page.connect_hidden(move |_| {
            if let Some(this) = weak.upgrade() {
                this.closed();
            }
        });
        // The viewer lives until its page is popped.
        *self.keep_alive.borrow_mut() = Some(Rc::clone(self));
        let weak = self.this.clone();
        self.page.connect_shown(move |_| {
            if let Some(this) = weak.upgrade() {
                this.image.grab_focus();
            }
        });

        let events = self.ctx.library.subscribe();
        let weak = self.this.clone();
        util::spawn(async move {
            while let Ok(event) = events.recv().await {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                if let medianest_service::Event::AssetsChanged(ids) = &event {
                    if this.current().is_some_and(|item| ids.contains(&item.id)) {
                        this.refresh_detail();
                    }
                }
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, run: Handler<Self>| {
            let weak = self.this.clone();
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    if let Some(item) = this.current() {
                        run(&this, item.id);
                    }
                }
            });
            self.actions.add_action(&action);
        };
        add("previous", Box::new(|this, _| this.step(-1)));
        add("next", Box::new(|this, _| this.step(1)));
        add("first", Box::new(|this, _| this.go_to(0)));
        add(
            "last",
            Box::new(|this, _| {
                let last = this.items.borrow().len().saturating_sub(1);
                this.go_to(last);
            }),
        );
        add(
            "favorite",
            Box::new(|this, id| {
                let favorite = !this.current().is_some_and(|item| item.has(flags::FAVORITE));
                this.update_flag(id, flags::FAVORITE, favorite);
                actions::set_favorite(&this.ctx, vec![id], favorite);
            }),
        );
        add(
            "trash",
            Box::new(|this, id| {
                if this.trash {
                    return;
                }
                actions::trash(&this.ctx, vec![id]);
                this.remove_current();
            }),
        );
        add(
            "restore",
            Box::new(|this, id| {
                actions::restore(&this.ctx, vec![id]);
                this.remove_current();
            }),
        );
        add("rotate-left", Box::new(|this, id| this.rotate(id, -90)));
        add("rotate-right", Box::new(|this, id| this.rotate(id, 90)));
        add(
            "hide",
            Box::new(|this, id| {
                actions::set_hidden(&this.ctx, vec![id], true);
                this.remove_current();
            }),
        );
        add(
            "date",
            Box::new(|this, id| crate::dialogs::date::show(&this.ctx, vec![id])),
        );
        add(
            "album",
            Box::new(|this, id| crate::dialogs::albums::show(&this.ctx, vec![id])),
        );
        add(
            "tag",
            Box::new(|this, id| crate::dialogs::tags::show(&this.ctx, vec![id])),
        );
        add(
            "copy",
            Box::new(|this, id| actions::copy_files(&this.ctx, vec![id])),
        );
        add(
            "export",
            Box::new(|this, id| crate::dialogs::export::show(&this.ctx, vec![id])),
        );
        add(
            "show-in-files",
            Box::new(|this, id| actions::show_in_files(&this.ctx, id)),
        );
        add(
            "open-with",
            Box::new(|this, id| actions::open_with(&this.ctx, id)),
        );
        add("zoom-in", Box::new(|this, _| this.image.zoom_in()));
        add("zoom-out", Box::new(|this, _| this.image.zoom_out()));
        add("zoom-fit", Box::new(|this, _| this.image.zoom_fit()));
        add(
            "zoom-original",
            Box::new(|this, _| this.image.zoom_original()),
        );
        add(
            "info",
            Box::new(|this, _| this.split.set_show_sidebar(!this.split.shows_sidebar())),
        );
        add("fullscreen", Box::new(|this, _| this.toggle_fullscreen()));
        add("slideshow", Box::new(|this, _| this.toggle_slideshow()));
        add(
            "play",
            Box::new(|this, _| {
                if let Some(stream) = this.video.media_stream() {
                    stream.set_playing(!stream.is_playing());
                } else if this.live_button.is_visible() {
                    this.play_live();
                }
            }),
        );
        add(
            "leave",
            Box::new(|this, _| {
                if this.slideshow.borrow().is_some() {
                    this.toggle_slideshow();
                } else if this.ctx.window.is_fullscreen() {
                    this.toggle_fullscreen();
                } else {
                    this.ctx.nav.pop();
                }
            }),
        );
        if let Some(album) = self.album {
            add(
                "set-cover",
                Box::new(move |this, id| actions::set_cover(&this.ctx, album, id)),
            );
            add(
                "remove-from-album",
                Box::new(move |this, id| {
                    actions::remove_from_album(&this.ctx, album, vec![id], true);
                    this.remove_current();
                }),
            );
        }
        for rating in 0..=5u8 {
            add(
                &format!("rate-{rating}"),
                Box::new(move |this, id| {
                    actions::set_rating(&this.ctx, vec![id], rating);
                }),
            );
        }
        self.toolbar
            .insert_action_group("viewer", Some(&self.actions));

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Managed);
        for (trigger, action) in [
            ("Left|KP_Left|Page_Up|BackSpace", "viewer.previous"),
            ("Right|KP_Right|Page_Down|space", "viewer.next"),
            ("Home", "viewer.first"),
            ("End", "viewer.last"),
            ("Escape", "viewer.leave"),
            ("f", "viewer.favorite"),
            ("Delete|KP_Delete", "viewer.trash"),
            ("<Alt>Return|i", "viewer.info"),
            ("F11", "viewer.fullscreen"),
            ("F5", "viewer.slideshow"),
            (
                "plus|equal|KP_Add|<Control>plus|<Control>equal",
                "viewer.zoom-in",
            ),
            ("minus|KP_Subtract|<Control>minus", "viewer.zoom-out"),
            ("<Control>0", "viewer.zoom-fit"),
            ("<Control>1", "viewer.zoom-original"),
            ("<Control>r", "viewer.rotate-right"),
            ("<Control><Shift>r", "viewer.rotate-left"),
            ("<Control>c", "viewer.copy"),
            ("<Control>e", "viewer.export"),
            ("<Control>d", "viewer.date"),
            ("t", "viewer.tag"),
            ("k|p", "viewer.play"),
            ("0", "viewer.rate-0"),
            ("1", "viewer.rate-1"),
            ("2", "viewer.rate-2"),
            ("3", "viewer.rate-3"),
            ("4", "viewer.rate-4"),
            ("5", "viewer.rate-5"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.toolbar.add_controller(shortcuts);
    }

    fn step(self: &Rc<Self>, delta: isize) {
        let len = self.items.borrow().len();
        if len == 0 {
            return;
        }
        let index = self.index.get().saturating_add_signed(delta).min(len - 1);
        if index != self.index.get() {
            self.go_to(index);
        }
    }

    fn go_to(self: &Rc<Self>, index: usize) {
        self.index.set(index);
        self.show_current();
    }

    fn show_controls(&self) {
        let len = self.items.borrow().len();
        let index = self.index.get();
        let is_image = self.stack.visible_child_name().as_deref() == Some("image");
        self.controls[0].set_reveal_child(index > 0);
        self.controls[1].set_reveal_child(index + 1 < len);
        self.controls[2].set_reveal_child(is_image);
        if let Some(source) = self.hide_controls.take() {
            source.remove();
        }
        let weak = self.this.clone();
        let source = glib::timeout_add_local_once(CONTROLS_TIMEOUT, move || {
            if let Some(this) = weak.upgrade() {
                this.hide_controls.take();
                for revealer in &this.controls {
                    revealer.set_reveal_child(false);
                }
                if this.ctx.window.is_fullscreen() {
                    this.toolbar.set_reveal_top_bars(false);
                    this.page.set_cursor_from_name(Some("none"));
                }
            }
        });
        *self.hide_controls.borrow_mut() = Some(source);
        self.page.set_cursor_from_name(None);
    }

    fn show_current(self: &Rc<Self>) {
        let Some(item) = self.current() else {
            self.ctx.nav.pop();
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.full_loading.set(false);
        self.stop_media();
        self.detail.replace(None);
        let title = item
            .local_ms
            .map_or_else(|| tr("Unknown Date"), util::day_title);
        self.title.set_title(&title);
        self.title
            .set_subtitle(&item.local_ms.map(util::format_time).unwrap_or_default());
        self.page.set_title(&title);
        self.update_favorite(item.has(flags::FAVORITE));
        self.live_button.set_visible(item.has(flags::LIVE));

        if item.has(flags::VIDEO) {
            self.stack.set_visible_child_name("video");
            let ctx = Rc::clone(&self.ctx);
            let weak = self.this.clone();
            let id = item.id;
            util::spawn(async move {
                let path = ctx.call(move |library| library.original_path(id)).await;
                let Some(this) = weak.upgrade() else { return };
                if this.generation.get() != generation {
                    return;
                }
                match path {
                    Ok(path) => {
                        let media = PlayerStream::for_file(&path);
                        media.set_muted(this.ctx.config.borrow().start_muted);
                        this.video.set_media_stream(Some(&media));
                    }
                    Err(error) => this.show_missing(&error),
                }
            });
        } else {
            self.stack.set_visible_child_name("image");
            // Start with whatever is at hand, then sharpen.
            let quick = self
                .previews
                .borrow()
                .get(&item.id)
                .cloned()
                .or_else(|| self.ctx.loader.get(item.uuid));
            let size = quick.as_ref().map_or((2048, 2048), |texture| {
                provisional_size(texture, item.aspect, item.quarter_turns)
            });
            self.image
                .set_image(quick.as_ref(), size, item.quarter_turns);
            if !self.previews.borrow().contains_key(&item.id) {
                self.load_preview(item, generation);
            }
        }
        self.refresh_detail();
        self.prefetch();
        self.show_controls();
        if let Some(grid) = self.grid.upgrade() {
            if let Some(index) = grid.index_of(item.id) {
                grid.reveal(index);
            }
        }
    }

    fn show_missing(&self, reason: &str) {
        self.missing
            .set_description(Some(&glib::markup_escape_text(reason)));
        self.stack.set_visible_child_name("missing");
    }

    fn load_preview(self: &Rc<Self>, item: TimelineItem, generation: u64) {
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let id = item.id;
            let pixels = ctx
                .call(move |library| {
                    let path = library.preview(id)?;
                    textures::decode_jpeg(&path)
                        .map_err(|error| medianest_service::Error::Invalid(error.to_string()))
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match pixels {
                Ok(pixels) => {
                    let texture = pixels.texture();
                    this.remember_preview(id, &texture);
                    if this.generation.get() == generation {
                        let size = this
                            .detail
                            .borrow()
                            .as_ref()
                            .and_then(|detail| oriented_size(&detail.asset))
                            .unwrap_or_else(|| {
                                (
                                    texture.width().unsigned_abs(),
                                    texture.height().unsigned_abs(),
                                )
                            });
                        this.image
                            .set_image(Some(&texture), size, item.quarter_turns);
                    }
                }
                Err(error) => {
                    if this.generation.get() == generation
                        && this.ctx.loader.get(item.uuid).is_none()
                    {
                        this.show_missing(&error);
                    }
                }
            }
        });
    }

    fn remember_preview(&self, id: AssetId, texture: &gdk::Texture) {
        let mut previews = self.previews.borrow_mut();
        previews.insert(id, texture.clone());
        if previews.len() > 2 * PREFETCH + 2 {
            let keep: Vec<AssetId> = {
                let items = self.items.borrow();
                let index = self.index.get();
                let start = index.saturating_sub(PREFETCH);
                let end = (index + PREFETCH + 1).min(items.len());
                items[start..end].iter().map(|item| item.id).collect()
            };
            previews.retain(|id, _| keep.contains(id));
        }
    }

    /// Prepares the neighbours so stepping through feels instant.
    fn prefetch(self: &Rc<Self>) {
        let items = self.items.borrow().clone();
        let index = self.index.get();
        for neighbour in [index + 1, index.wrapping_sub(1)] {
            let Some(item) = items.get(neighbour).copied() else {
                continue;
            };
            if item.has(flags::VIDEO) || self.previews.borrow().contains_key(&item.id) {
                continue;
            }
            let ctx = Rc::clone(&self.ctx);
            let weak = self.this.clone();
            util::spawn(async move {
                let id = item.id;
                let pixels = ctx
                    .call(move |library| {
                        let path = library.preview(id)?;
                        textures::decode_jpeg(&path)
                            .map_err(|e| medianest_service::Error::Invalid(e.to_string()))
                    })
                    .await;
                if let (Some(this), Ok(pixels)) = (weak.upgrade(), pixels) {
                    this.remember_preview(id, &pixels.texture());
                }
            });
        }
    }

    fn load_full_resolution(self: &Rc<Self>) {
        let Some(item) = self.current() else { return };
        if self.full_loading.replace(true) || item.has(flags::VIDEO) {
            return;
        }
        let generation = self.generation.get();
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let id = item.id;
            let full = ctx.call(move |library| library.full_image(id)).await;
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() != generation {
                return;
            }
            if let Ok((width, height, data)) = full {
                let texture = Pixels {
                    width,
                    height,
                    data,
                    alpha: true,
                }
                .texture();
                this.image.upgrade_texture(&texture);
            }
        });
    }

    fn refresh_detail(self: &Rc<Self>) {
        let Some(item) = self.current() else { return };
        let generation = self.generation.get();
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let id = item.id;
            let detail = ctx.call(move |library| library.asset_detail(id)).await;
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() != generation {
                return;
            }
            match detail {
                Ok(detail) => {
                    if let Some(place) = &detail.asset.place {
                        let time = detail
                            .asset
                            .taken
                            .local_ms
                            .map(util::format_time)
                            .unwrap_or_default();
                        let subtitle = if time.is_empty() {
                            place.short()
                        } else {
                            format!("{time} · {}", place.short())
                        };
                        this.title.set_subtitle(&subtitle);
                    }
                    this.update_favorite(detail.asset.favorite);
                    this.live_button.set_visible(detail.asset.is_live());
                    let turns = u8::try_from((detail.asset.rotation % 360) / 90).unwrap_or(0);
                    if let (Some(size), true) = (
                        oriented_size(&detail.asset),
                        this.stack.visible_child_name().as_deref() == Some("image"),
                    ) {
                        // Correct the provisional size now that the real one is known.
                        if this.detail.borrow().is_none() {
                            let texture = this
                                .previews
                                .borrow()
                                .get(&id)
                                .cloned()
                                .or_else(|| this.ctx.loader.get(item.uuid));
                            if let Some(texture) = texture {
                                this.image.set_image(Some(&texture), size, turns);
                            }
                        } else {
                            this.image.set_quarter_turns(turns);
                        }
                    }
                    this.info.set_detail(&detail);
                    this.detail.replace(Some(detail));
                }
                Err(error) => this.info.set_error(&error),
            }
        });
    }

    fn update_favorite(&self, favorite: bool) {
        self.favorite.set_icon_name(if favorite {
            "mn-heart-filled-symbolic"
        } else {
            "mn-heart-outline-symbolic"
        });
        self.favorite.set_tooltip_text(Some(&if favorite {
            tr("Remove From Favorites")
        } else {
            tr("Add to Favorites")
        }));
    }

    fn update_flag(&self, id: AssetId, flag: u16, on: bool) {
        let mut items = self.items.borrow_mut();
        let list = Rc::make_mut(&mut items);
        if let Some(item) = list.iter_mut().find(|item| item.id == id) {
            if on {
                item.flags |= flag;
            } else {
                item.flags &= !flag;
            }
        }
        drop(items);
        if flag == flags::FAVORITE {
            self.update_favorite(on);
        }
    }

    fn rotate(self: &Rc<Self>, id: AssetId, degrees: i32) {
        {
            let mut items = self.items.borrow_mut();
            let list = Rc::make_mut(&mut items);
            if let Some(item) = list.iter_mut().find(|item| item.id == id) {
                let turns = i32::from(item.quarter_turns) + degrees / 90;
                item.quarter_turns = u8::try_from(turns.rem_euclid(4)).unwrap_or(0);
                self.image.set_quarter_turns(item.quarter_turns);
            }
        }
        actions::rotate(&self.ctx, vec![id], degrees);
    }

    /// The current item left this view (trashed, hidden, removed).
    fn remove_current(self: &Rc<Self>) {
        let index = self.index.get();
        {
            let mut items = self.items.borrow_mut();
            let list = Rc::make_mut(&mut items);
            if index < list.len() {
                list.remove(index);
            }
            if list.is_empty() {
                drop(items);
                self.ctx.nav.pop();
                return;
            }
            if index >= list.len() {
                self.index.set(list.len() - 1);
            }
        }
        self.show_current();
    }

    fn play_live(self: &Rc<Self>) {
        let Some(item) = self.current() else { return };
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        let generation = self.generation.get();
        util::spawn(async move {
            let id = item.id;
            let video = ctx.call(move |library| library.motion_video(id)).await;
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() != generation {
                return;
            }
            match video {
                Ok(Some(path)) => {
                    let media = PlayerStream::for_file(&path);
                    media.set_muted(this.ctx.config.borrow().start_muted);
                    let picture = this.live_picture.clone();
                    media.connect_ended_notify(move |media| {
                        if media.is_ended() {
                            picture.set_visible(false);
                        }
                    });
                    this.live_picture.set_paintable(Some(&media));
                    this.live_picture.set_visible(true);
                    media.play();
                }
                Ok(None) => {}
                Err(error) => this.ctx.error(&tr("Could not play the Live Photo"), &error),
            }
        });
    }

    fn stop_media(&self) {
        if let Some(stream) = self.video.media_stream() {
            stream.pause();
        }
        self.video.set_media_stream(None::<&gtk::MediaStream>);
        if let Some(media) = self
            .live_picture
            .paintable()
            .and_downcast::<gtk::MediaStream>()
        {
            media.pause();
        }
        self.live_picture.set_paintable(None::<&gdk::Paintable>);
        self.live_picture.set_visible(false);
    }

    fn toggle_fullscreen(&self) {
        let content = self.split.content();
        if self.ctx.window.is_fullscreen() {
            if let Some(content) = &content {
                content.remove_css_class("fullscreen");
            }
            self.ctx.window.unfullscreen();
            self.toolbar.set_reveal_top_bars(true);
            self.page.set_cursor_from_name(None);
        } else {
            if let Some(content) = &content {
                content.add_css_class("fullscreen");
            }
            self.ctx.window.fullscreen();
            self.toolbar.set_reveal_top_bars(false);
            self.split.set_show_sidebar(false);
        }
    }

    fn toggle_slideshow(self: &Rc<Self>) {
        if let Some(source) = self.slideshow.take() {
            source.remove();
            if self.ctx.window.is_fullscreen() {
                self.toggle_fullscreen();
            }
            self.ctx.toast(&tr("Slideshow stopped"));
            return;
        }
        if !self.ctx.window.is_fullscreen() {
            self.toggle_fullscreen();
        }
        let seconds = u64::from(self.ctx.config.borrow().slideshow_seconds.max(1));
        let weak = self.this.clone();
        let source = glib::timeout_add_local(Duration::from_secs(seconds), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let len = this.items.borrow().len();
            let next = if this.index.get() + 1 < len {
                this.index.get() + 1
            } else {
                0
            };
            this.go_to(next);
            glib::ControlFlow::Continue
        });
        *self.slideshow.borrow_mut() = Some(source);
    }

    fn closed(&self) {
        let popped = !self
            .ctx
            .nav
            .navigation_stack()
            .iter::<adw::NavigationPage>()
            .flatten()
            .any(|page| page == self.page);
        if !popped {
            return;
        }
        // Dropped at the end of this call, after the last use of `self`.
        let _keep = self.keep_alive.take();
        self.stop_media();
        if let Some(source) = self.slideshow.take() {
            source.remove();
        }
        if self.ctx.window.is_fullscreen() {
            self.ctx.window.unfullscreen();
        }
        if let (Some(grid), Some(item)) = (self.grid.upgrade(), self.current()) {
            if let Some(index) = grid.index_of(item.id) {
                grid.reveal(index);
            }
            grid.grab_focus();
        }
    }
}

/// The original's size after EXIF orientation (before user rotation).
fn oriented_size(asset: &medianest_service::catalog::Asset) -> Option<(u32, u32)> {
    let (width, height) = (asset.width?, asset.height?);
    Some(if (5..=8).contains(&asset.orientation) {
        (height, width)
    } else {
        (width, height)
    })
}

/// A stand-in size for a texture until the original's size is known: big
/// enough that the fitted view fills the window.
fn provisional_size(texture: &gdk::Texture, aspect: f32, quarter_turns: u8) -> (u32, u32) {
    let (width, height) = (texture.width().max(1), texture.height().max(1));
    let _ = (aspect, quarter_turns);
    let factor = 4096.0 / f64::from(width.max(height));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (
        (f64::from(width) * factor) as u32,
        (f64::from(height) * factor) as u32,
    )
}
