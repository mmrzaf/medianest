//! An open library: sidebar, sections, activity and change events.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use medianest_service::catalog::{Scope, Sort, View};
use medianest_service::core::query::Query;
use medianest_service::{Event, JobState, Lane};

use crate::context::{Ctx, Destination};
use crate::i18n::{tr, trf};
use crate::pages::activity::Activity;
use crate::pages::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::pages::{Section, albums, folders, health, places, review, tags};
use crate::util::{self, Debounce};

const RECENT_DAYS: i64 = 30;

type Handler<T> = Box<dyn Fn(&Rc<T>)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SectionId {
    Photos,
    Favorites,
    Videos,
    Recent,
    Albums,
    Tags,
    Places,
    Folders,
    Review,
    Health,
    Trash,
}

impl SectionId {
    const ALL: [Self; 11] = [
        Self::Photos,
        Self::Favorites,
        Self::Videos,
        Self::Recent,
        Self::Albums,
        Self::Tags,
        Self::Places,
        Self::Folders,
        Self::Review,
        Self::Health,
        Self::Trash,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Photos => "photos",
            Self::Favorites => "favorites",
            Self::Videos => "videos",
            Self::Recent => "recent",
            Self::Albums => "albums",
            Self::Tags => "tags",
            Self::Places => "places",
            Self::Folders => "folders",
            Self::Review => "review",
            Self::Health => "health",
            Self::Trash => "trash",
        }
    }

    fn title(self) -> String {
        match self {
            Self::Photos => tr("Photos"),
            Self::Favorites => tr("Favorites"),
            Self::Videos => tr("Videos"),
            Self::Recent => tr("Recently Added"),
            Self::Albums => tr("Albums"),
            Self::Tags => tr("Tags"),
            Self::Places => tr("Places"),
            Self::Folders => tr("Folders"),
            Self::Review => tr("Review"),
            Self::Health => tr("Library Health"),
            Self::Trash => tr("Trash"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Photos => "image-x-generic-symbolic",
            Self::Favorites => "mn-heart-outline-symbolic",
            Self::Videos => "camera-video-symbolic",
            Self::Recent => "document-open-recent-symbolic",
            Self::Albums => "view-grid-symbolic",
            Self::Tags => "mn-tag-symbolic",
            Self::Places => "mn-map-symbolic",
            Self::Folders => "folder-symbolic",
            Self::Review => "mn-check-badge-symbolic",
            Self::Health => "security-high-symbolic",
            Self::Trash => "user-trash-symbolic",
        }
    }

    /// The sidebar group a section starts, if any.
    fn group(self) -> Option<String> {
        match self {
            Self::Albums => Some(tr("Collections")),
            Self::Review => Some(tr("Care")),
            _ => None,
        }
    }
}

struct SidebarRow {
    row: gtk::ListBoxRow,
    count: gtk::Label,
}

pub struct LibraryView {
    ctx: Rc<Ctx>,
    page: adw::NavigationPage,
    split: adw::OverlaySplitView,
    list: gtk::ListBox,
    rows: HashMap<SectionId, SidebarRow>,
    stack: gtk::Stack,
    sections: RefCell<HashMap<SectionId, Rc<dyn Section>>>,
    current: RefCell<SectionId>,
    activity: Rc<Activity>,
    health_line: gtk::Label,
    counts_debounce: Debounce,
    narrow: std::cell::Cell<bool>,
}

impl std::fmt::Debug for LibraryView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryView").finish_non_exhaustive()
    }
}

impl LibraryView {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let title = library_title(&ctx);

        // Sidebar.
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let mut rows = HashMap::new();
        for section in SectionId::ALL {
            let icon = gtk::Image::from_icon_name(section.icon());
            let label = gtk::Label::builder()
                .label(section.title())
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let count = gtk::Label::builder()
                .css_classes(["sidebar-count", "numeric"])
                .visible(false)
                .build();
            if section == SectionId::Review {
                count.add_css_class("review-count");
            }
            let content = gtk::Box::builder().spacing(12).build();
            content.append(&icon);
            content.append(&label);
            content.append(&count);
            let row = gtk::ListBoxRow::builder()
                .child(&content)
                .name(section.key())
                .build();
            row.update_property(&[gtk::accessible::Property::Label(&section.title())]);
            list.append(&row);
            rows.insert(section, SidebarRow { row, count });
        }
        list.set_header_func(|row, _| {
            let group = SectionId::ALL
                .into_iter()
                .find(|section| section.key() == row.widget_name())
                .and_then(SectionId::group);
            row.set_header(
                group
                    .map(|title| {
                        gtk::Label::builder()
                            .label(title)
                            .xalign(0.0)
                            .css_classes(["heading", "dim-label"])
                            .margin_top(18)
                            .margin_start(12)
                            .margin_bottom(6)
                            .build()
                    })
                    .as_ref(),
            );
        });

        let menu = gio::Menu::new();
        let library_section = gio::Menu::new();
        library_section.append(Some(&tr("_Import…")), Some("win.import"));
        library_section.append(Some(&tr("_Add Folder…")), Some("win.add-folder"));
        menu.append_section(None, &library_section);
        let file_section = gio::Menu::new();
        file_section.append(Some(&tr("_New Library…")), Some("win.new-library"));
        file_section.append(Some(&tr("_Open Library…")), Some("win.open-library"));
        file_section.append(Some(&tr("_Close Library")), Some("win.close-library"));
        menu.append_section(None, &file_section);
        let app_section = gio::Menu::new();
        app_section.append(Some(&tr("_Preferences")), Some("win.preferences"));
        app_section.append(Some(&tr("_Keyboard Shortcuts")), Some("app.shortcuts"));
        app_section.append(Some(&tr("_About MediaNest")), Some("app.about"));
        menu.append_section(None, &app_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text(tr("Main Menu"))
            .build();
        let import_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .action_name("win.import")
            .tooltip_text(tr("Import Photos and Videos"))
            .build();
        let sidebar_header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(&title, ""))
            .build();
        sidebar_header.pack_start(&import_button);
        sidebar_header.pack_end(&menu_button);

        let activity = Activity::new(Rc::clone(&ctx));
        let health_line = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "dim-label", "status-line"])
            .build();
        let health_button = gtk::Button::builder()
            .child(&health_line)
            .css_classes(["flat"])
            .tooltip_text(tr("Show Library Health"))
            .build();
        let footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(6)
            .margin_end(6)
            .margin_bottom(6)
            .build();
        footer.append(activity.widget());
        footer.append(&health_button);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let sidebar = adw::ToolbarView::new();
        sidebar.add_top_bar(&sidebar_header);
        sidebar.set_content(Some(&sidebar_scroll));
        sidebar.add_bottom_bar(&footer);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&stack)
            .min_sidebar_width(220.0)
            .max_sidebar_width(280.0)
            .build();

        let page = adw::NavigationPage::builder()
            .title(&title)
            .tag("library")
            .child(&split)
            .build();

        let this = Rc::new(Self {
            ctx,
            page,
            split,
            list,
            rows,
            stack,
            sections: RefCell::default(),
            current: RefCell::new(SectionId::Photos),
            activity,
            health_line,
            counts_debounce: Debounce::new(400),
            narrow: std::cell::Cell::new(false),
        });
        this.setup(&health_button);
        this.show(SectionId::Photos);
        this.refresh_counts();
        this
    }

    pub fn ctx(&self) -> Rc<Ctx> {
        Rc::clone(&self.ctx)
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    pub fn title(&self) -> String {
        library_title(&self.ctx)
    }

    pub fn set_narrow(&self, narrow: bool) {
        self.narrow.set(narrow);
        self.split.set_collapsed(narrow);
        if !narrow {
            self.split.set_show_sidebar(true);
        }
    }

    /// Stops listening; the window is about to close the library.
    pub fn shutdown(&self) {
        self.sections.borrow_mut().clear();
    }

    fn setup(self: &Rc<Self>, health_button: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let name = row.widget_name();
            if let Some(section) = SectionId::ALL.into_iter().find(|s| s.key() == name) {
                if *this.current.borrow() != section {
                    this.show(section);
                }
                if this.narrow.get() {
                    this.split.set_show_sidebar(false);
                }
            }
        });
        let weak = Rc::downgrade(self);
        health_button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.show(SectionId::Health);
            }
        });

        let weak = Rc::downgrade(self);
        self.ctx.set_navigate(move |destination| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            match destination {
                Destination::Review => this.show(SectionId::Review),
                Destination::Folders => this.show(SectionId::Folders),
                Destination::Search(text) => {
                    this.show(SectionId::Photos);
                    if let Some(section) = this.sections.borrow().get(&SectionId::Photos) {
                        section.search(&text);
                    }
                }
            }
        });

        self.install_actions();

        // Library events.
        let events = self.ctx.library.subscribe();
        let weak = Rc::downgrade(self);
        util::spawn(async move {
            while let Ok(event) = events.recv().await {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                this.handle(&event);
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let window = &self.ctx.window;
        let action = |name: &str, run: Handler<Self>| {
            let weak = Rc::downgrade(self);
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    run(&this);
                }
            });
            window.add_action(&action);
        };
        action(
            "import",
            Box::new(|this| crate::dialogs::import::choose(&this.ctx)),
        );
        action(
            "add-folder",
            Box::new(|this| folders::choose_folder(&this.ctx)),
        );
        action(
            "preferences",
            Box::new(|this| crate::dialogs::preferences::show(&this.ctx)),
        );
        action("undo", Box::new(|this| this.ctx.undo()));
        action(
            "toggle-sidebar",
            Box::new(|this| this.split.set_show_sidebar(!this.split.shows_sidebar())),
        );
        action(
            "search",
            Box::new(|this| {
                let current = *this.current.borrow();
                let section = if matches!(
                    current,
                    SectionId::Photos | SectionId::Favorites | SectionId::Videos
                ) {
                    current
                } else {
                    this.show(SectionId::Photos);
                    SectionId::Photos
                };
                if let Some(section) = this.sections.borrow().get(&section) {
                    section.search("");
                }
            }),
        );
        for (name, target) in [
            ("go-photos", SectionId::Photos),
            ("go-albums", SectionId::Albums),
            ("go-places", SectionId::Places),
            ("go-review", SectionId::Review),
        ] {
            action(name, Box::new(move |this| this.show(target)));
        }
        for name in ["select-all", "zoom-in", "zoom-out"] {
            action(
                name,
                Box::new(move |this| {
                    let current = *this.current.borrow();
                    if let Some(section) = this.sections.borrow().get(&current) {
                        section.command(name);
                    }
                }),
            );
        }
    }

    fn section(self: &Rc<Self>, id: SectionId) -> Rc<dyn Section> {
        if let Some(section) = self.sections.borrow().get(&id) {
            return Rc::clone(section);
        }
        let ctx = Rc::clone(&self.ctx);
        let toggle = self.sidebar_toggle();
        let section: Rc<dyn Section> = match id {
            SectionId::Photos => TimelinePage::new(
                ctx,
                TimelineOptions {
                    title: id.title(),
                    view: View::library(),
                    kind: TimelineKind::Library,
                    searchable: true,
                    empty_icon: "image-x-generic-symbolic",
                    empty_title: tr("No Photos Yet"),
                    empty_description: tr(
                        "Import photos and videos from a camera, phone or folder to get started.",
                    ),
                    sidebar_toggle: Some(toggle),
                },
            ),
            SectionId::Favorites => TimelinePage::new(
                ctx,
                TimelineOptions {
                    title: id.title(),
                    view: View::library().with_query(Query::parse("is:fav")),
                    kind: TimelineKind::Filtered,
                    searchable: true,
                    empty_icon: "mn-heart-outline-symbolic",
                    empty_title: tr("No Favorites"),
                    empty_description: tr(
                        "Mark photos with the heart button or press F to find them here.",
                    ),
                    sidebar_toggle: Some(toggle),
                },
            ),
            SectionId::Videos => TimelinePage::new(
                ctx,
                TimelineOptions {
                    title: id.title(),
                    view: View::library().with_query(Query::parse("is:video")),
                    kind: TimelineKind::Filtered,
                    searchable: true,
                    empty_icon: "camera-video-symbolic",
                    empty_title: tr("No Videos"),
                    empty_description: tr("Videos you import appear here."),
                    sidebar_toggle: Some(toggle),
                },
            ),
            SectionId::Recent => TimelinePage::new(
                ctx,
                TimelineOptions {
                    title: id.title(),
                    view: View {
                        scope: Scope::AddedSince(
                            medianest_service::core::now_ms() - RECENT_DAYS * 86_400_000,
                        ),
                        query: Query::default(),
                        sort: Sort::RecentlyAdded,
                    },
                    kind: TimelineKind::Recent,
                    searchable: false,
                    empty_icon: "document-open-recent-symbolic",
                    empty_title: tr("Nothing Added Recently"),
                    empty_description: tr("Items added in the last 30 days appear here."),
                    sidebar_toggle: Some(toggle),
                },
            ),
            SectionId::Trash => TimelinePage::new(
                ctx,
                TimelineOptions {
                    title: id.title(),
                    view: View {
                        scope: Scope::Trash,
                        query: Query::default(),
                        sort: Sort::NewestFirst,
                    },
                    kind: TimelineKind::Trash,
                    searchable: false,
                    empty_icon: "user-trash-symbolic",
                    empty_title: tr("Trash Is Empty"),
                    empty_description: tr(
                        "Deleted items move to the system trash. You can restore them from here until the trash is emptied.",
                    ),
                    sidebar_toggle: Some(toggle),
                },
            ),
            SectionId::Albums => albums::AlbumsPage::new(ctx, toggle),
            SectionId::Tags => tags::TagsPage::new(ctx, toggle),
            SectionId::Places => places::PlacesPage::new(ctx, toggle),
            SectionId::Folders => folders::FoldersPage::new(ctx, toggle),
            SectionId::Review => review::ReviewPage::new(ctx, toggle),
            SectionId::Health => health::HealthPage::new(ctx, toggle),
        };
        self.stack.add_named(&section.widget(), Some(id.key()));
        self.sections.borrow_mut().insert(id, Rc::clone(&section));
        section
    }

    /// A button that reveals the sidebar when it is collapsed.
    fn sidebar_toggle(&self) -> gtk::ToggleButton {
        let toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(tr("Show Sidebar"))
            .build();
        self.split
            .bind_property("show-sidebar", &toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        self.split
            .bind_property("collapsed", &toggle, "visible")
            .sync_create()
            .build();
        toggle
    }

    fn show(self: &Rc<Self>, id: SectionId) {
        let section = self.section(id);
        *self.current.borrow_mut() = id;
        self.stack.set_visible_child_name(id.key());
        section.shown();
        if let Some(row) = self.rows.get(&id) {
            if self.list.selected_row().as_ref() != Some(&row.row) {
                self.list.select_row(Some(&row.row));
            }
        }
    }

    fn handle(self: &Rc<Self>, event: &Event) {
        match event {
            Event::ThumbnailReady(_, uuid) => self.ctx.loader.thumbnail_ready(*uuid),
            Event::ThumbnailFailed(id) => self.ctx.loader.thumbnail_failed(*id),
            Event::Job(job) => {
                self.activity.update(job);
                if job.state.is_finished() && job.lane == Lane::Foreground {
                    self.job_finished(job);
                }
            }
            Event::ContentChanged
            | Event::AssetsChanged(_)
            | Event::ReviewChanged
            | Event::SourcesChanged => {
                self.schedule_counts();
            }
            Event::TagsChanged | Event::AlbumsChanged => {}
        }
        if !matches!(event, Event::ThumbnailReady(..) | Event::ThumbnailFailed(_)) {
            let current = *self.current.borrow();
            for (id, section) in self.sections.borrow().iter() {
                section.event(event, *id == current);
            }
        }
    }

    fn job_finished(&self, job: &medianest_service::JobSnapshot) {
        match job.state {
            JobState::Completed if !job.summary.is_empty() => self.ctx.toast(&job.summary),
            JobState::Failed => self.ctx.error(
                &job.title,
                job.error.as_deref().unwrap_or(&tr("something went wrong")),
            ),
            _ => {}
        }
    }

    fn schedule_counts(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.counts_debounce.call(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_counts();
            }
        });
    }

    fn refresh_counts(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let ctx = Rc::clone(&self.ctx);
        util::spawn(async move {
            let result = ctx
                .call(|library| {
                    let stats = library.read(medianest_service::catalog::Db::stats)?;
                    let review: u64 = library.review_counts()?.iter().map(|(_, n)| n).sum();
                    Ok((stats, review))
                })
                .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            let Ok((stats, review)) = result else {
                return;
            };
            let set = |id: SectionId, value: u64| {
                if let Some(row) = this.rows.get(&id) {
                    row.count.set_label(&util::number(value));
                    row.count.set_visible(value > 0);
                }
            };
            set(
                SectionId::Photos,
                stats.assets.saturating_sub(stats.hidden + stats.trashed),
            );
            set(SectionId::Favorites, stats.favorites);
            set(SectionId::Videos, stats.videos);
            set(SectionId::Review, review);
            set(SectionId::Trash, stats.trashed);
            let mut line = trf(
                "{} · {}",
                &[
                    &util::items(stats.assets.saturating_sub(stats.trashed)),
                    &util::bytes(stats.managed_bytes + stats.linked_bytes),
                ],
            );
            if review > 0 {
                line.push('\n');
                line.push_str(&crate::i18n::trn(
                    "{} item needs attention",
                    "{} items need attention",
                    review,
                ));
            }
            this.health_line.set_label(&line);
        });
    }
}

fn library_title(ctx: &Ctx) -> String {
    ctx.library
        .root()
        .file_name()
        .map_or_else(|| tr("Library"), |name| name.to_string_lossy().into_owned())
}
