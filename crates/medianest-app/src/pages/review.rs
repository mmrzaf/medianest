//! Review: everything that needs a person's decision, explained plainly.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::catalog::{ReviewItem, View};
use medianest_service::core::ReviewKind;
use medianest_service::core::query::Query;
use medianest_service::{Event, Library, ReviewAction};

use super::Section;
use super::similar::SimilarPage;
use super::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::context::Ctx;
use crate::i18n::{tr, trf, trn};
use crate::util::{self, Debounce};

/// Items listed per kind before "and N more".
const SHOWN_PER_KIND: usize = 50;

fn explain(kind: ReviewKind) -> String {
    match kind {
        ReviewKind::NeedsDate => tr(
            "These have no reliable date, so they may appear on the wrong day. Set the date \
             and they move to the right place.",
        ),
        ReviewKind::MissingFile => tr(
            "These files were moved or deleted outside MediaNest, or their disk is not connected.",
        ),
        ReviewKind::ChangedFile => tr(
            "The content of these files changed since they were checked. If you edited them on \
             purpose, use the new version. Otherwise the file may be damaged; restore it from a \
             backup.",
        ),
        ReviewKind::FailedImport => {
            tr("These could not be imported. Try again once the problem is fixed.")
        }
        ReviewKind::PendingRemoval => tr(
            "These were moved into the library. Their copies are verified; approve removing the \
             originals, or keep them.",
        ),
        ReviewKind::InterruptedOperation => tr(
            "MediaNest was stopped while working on these. Nothing was lost; check them again to finish.",
        ),
        ReviewKind::Unreadable => {
            tr("These files could not be read. They may be damaged or unsupported.")
        }
    }
}

fn icon(kind: ReviewKind) -> &'static str {
    match kind {
        ReviewKind::NeedsDate => "x-office-calendar-symbolic",
        ReviewKind::MissingFile => "image-missing-symbolic",
        ReviewKind::ChangedFile | ReviewKind::Unreadable => "dialog-warning-symbolic",
        ReviewKind::FailedImport => "process-stop-symbolic",
        ReviewKind::PendingRemoval => "user-trash-symbolic",
        ReviewKind::InterruptedOperation => "media-playback-pause-symbolic",
    }
}

pub struct ReviewPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    page: adw::PreferencesPage,
    stack: gtk::Stack,
    groups: std::cell::RefCell<Vec<adw::PreferencesGroup>>,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl ReviewPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        header.pack_start(&toggle);
        let page = adw::PreferencesPage::new();
        let empty = adw::StatusPage::builder()
            .icon_name("mn-check-badge-symbolic")
            .title(tr("All Clear"))
            .description(tr("Nothing needs your attention."))
            .build();
        let similar = gtk::Button::builder()
            .label(tr("Find _Similar Photos"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        empty.set_child(Some(&similar));
        let stack = gtk::Stack::new();
        stack.add_named(&page, Some("items"));
        stack.add_named(&empty, Some("empty"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let nav = adw::NavigationView::new();
        nav.add(
            &adw::NavigationPage::builder()
                .title(tr("Review"))
                .tag("review")
                .child(&toolbar)
                .build(),
        );
        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            nav,
            page,
            stack,
            groups: std::cell::RefCell::default(),
            stale: Cell::new(true),
            debounce: Debounce::new(400),
        });
        let weak = this.this.clone();
        similar.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open_similar();
            }
        });
        this
    }

    fn open_similar(&self) {
        let page = SimilarPage::new(Rc::clone(&self.ctx));
        self.nav.push(&page.navigation_page());
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let data = ctx
                .call(|library| {
                    let counts = library.review_counts()?;
                    let stats = library.read(medianest_service::catalog::Db::stats)?;
                    let mut items: BTreeMap<i64, Vec<ReviewItem>> = BTreeMap::new();
                    for item in library.review_items(None)? {
                        items.entry(item.kind.code()).or_default().push(item);
                    }
                    Ok((counts, items, stats.unknown_dates, stats.uncertain_dates))
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match data {
                Ok((counts, items, unknown, uncertain)) => {
                    this.show(&counts, items, unknown, uncertain)
                }
                Err(error) => this.ctx.error(&tr("Could not load the review"), &error),
            }
        });
    }

    fn show(
        &self,
        counts: &[(ReviewKind, u64)],
        mut items: BTreeMap<i64, Vec<ReviewItem>>,
        unknown: u64,
        uncertain: u64,
    ) {
        for group in self.groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }
        self.stack
            .set_visible_child_name(if counts.is_empty() { "empty" } else { "items" });
        for (kind, count) in counts {
            let group = adw::PreferencesGroup::builder()
                .title(format!("{} ({})", tr(kind.title()), util::number(*count)))
                .description(explain(*kind))
                .build();
            if *kind == ReviewKind::NeedsDate {
                if unknown > 0 {
                    group.add(&self.timeline_row(
                        trn("{} item without a date", "{} items without a date", unknown),
                        "date:unknown",
                        tr("No Date"),
                    ));
                }
                if uncertain > 0 {
                    group.add(&self.timeline_row(
                        trn(
                            "{} item with a guessed date",
                            "{} items with a guessed date",
                            uncertain,
                        ),
                        "date:uncertain",
                        tr("Guessed Dates"),
                    ));
                }
            } else {
                let list = items.remove(&kind.code()).unwrap_or_default();
                let (primary, secondary) = Library::review_actions(*kind);
                if list.len() > 1 {
                    let all = gtk::Box::builder().spacing(6).build();
                    let primary_all = gtk::Button::builder()
                        .label(trf("{} for All", &[&tr(primary)]))
                        .css_classes(["flat"])
                        .build();
                    let (ctx, weak, kind) = (Rc::clone(&self.ctx), self.this.clone(), *kind);
                    primary_all.connect_clicked(move |_| {
                        if let Some(this) = weak.upgrade() {
                            this.resolve_all(&ctx, kind, ReviewAction::Primary);
                        }
                    });
                    all.append(&primary_all);
                    group.set_header_suffix(Some(&all));
                }
                for item in list.iter().take(SHOWN_PER_KIND) {
                    group.add(&self.item_row(item, primary, secondary));
                }
                if list.len() > SHOWN_PER_KIND {
                    group.add(
                        &adw::ActionRow::builder()
                            .title(trn(
                                "and {} more",
                                "and {} more",
                                (list.len() - SHOWN_PER_KIND) as u64,
                            ))
                            .css_classes(["dim-label"])
                            .build(),
                    );
                }
            }
            self.page.add(&group);
            self.groups.borrow_mut().push(group);
        }
        let more = adw::PreferencesGroup::builder()
            .title(tr("Tidy Up"))
            .build();
        let similar = adw::ActionRow::builder()
            .title(tr("Similar Photos"))
            .subtitle(tr("Bursts, resized copies and near-duplicates"))
            .activatable(true)
            .build();
        similar.add_prefix(&gtk::Image::from_icon_name("edit-copy-symbolic"));
        similar.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.this.clone();
        similar.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                this.open_similar();
            }
        });
        more.add(&similar);
        self.page.add(&more);
        self.groups.borrow_mut().push(more);
    }

    fn timeline_row(
        &self,
        title: String,
        query: &'static str,
        page_title: String,
    ) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.this.clone();
        row.connect_activated(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let page = TimelinePage::new(
                Rc::clone(&this.ctx),
                TimelineOptions {
                    title: page_title.clone(),
                    view: View::library().with_query(Query::parse(query)),
                    kind: TimelineKind::Filtered,
                    searchable: false,
                    empty_icon: "mn-check-badge-symbolic",
                    empty_title: tr("All Dated"),
                    empty_description: tr("Every item has a date now."),
                    sidebar_toggle: None,
                },
            );
            this.nav.push(&page.navigation_page());
            this.ctx
                .toast(&tr("Select items and press Ctrl+D to set their date"));
        });
        row
    }

    fn item_row(
        &self,
        item: &ReviewItem,
        primary: &str,
        secondary: Option<&str>,
    ) -> adw::ActionRow {
        let path = item
            .path
            .as_ref()
            .map(|path| path.display())
            .unwrap_or_default();
        let mut subtitle = path;
        if let Some(reason) = item.detail.get("reason").and_then(|value| value.as_str()) {
            if !subtitle.is_empty() {
                subtitle.push('\n');
            }
            subtitle.push_str(reason);
        }
        if subtitle.is_empty() {
            subtitle = util::ago(item.created_ms);
        }
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&item.subject))
            .subtitle(glib::markup_escape_text(&subtitle))
            .subtitle_lines(2)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon(item.kind)));
        let buttons = gtk::Box::builder()
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let mut actions = vec![(tr(primary), ReviewAction::Primary, true)];
        if let Some(secondary) = secondary {
            actions.push((tr(secondary), ReviewAction::Secondary, false));
        }
        for (label, action, suggested) in actions {
            let button = gtk::Button::builder().label(label).build();
            if suggested {
                button.add_css_class("suggested-action");
            }
            let (ctx, id) = (Rc::clone(&self.ctx), item.id);
            button.connect_clicked(move |button| {
                button.set_sensitive(false);
                let ctx = Rc::clone(&ctx);
                util::spawn(async move {
                    match ctx
                        .call(move |library| library.resolve_review(id, action))
                        .await
                    {
                        Ok(message) => ctx.toast(&tr(&message)),
                        Err(error) => ctx.error(&tr("Could not do that"), &error),
                    }
                });
            });
            buttons.append(&button);
        }
        let dismiss = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(tr("Dismiss"))
            .css_classes(["flat", "circular"])
            .build();
        let (ctx, id) = (Rc::clone(&self.ctx), item.id);
        dismiss.connect_clicked(move |_| {
            let ctx = Rc::clone(&ctx);
            util::spawn(async move {
                let _ = ctx
                    .call(move |library| library.resolve_review(id, ReviewAction::Dismiss))
                    .await;
            });
        });
        buttons.append(&dismiss);
        row.add_suffix(&buttons);
        row
    }

    fn resolve_all(&self, ctx: &Rc<Ctx>, kind: ReviewKind, action: ReviewAction) {
        let (primary, _) = Library::review_actions(kind);
        let inner = Rc::clone(ctx);
        let destructive = matches!(kind, ReviewKind::PendingRemoval | ReviewKind::MissingFile);
        ctx.confirm(
            &trf("{} for All?", &[&tr(primary)]),
            &explain(kind),
            &tr(primary),
            destructive,
            move || {
                let ctx = inner;
                util::spawn(async move {
                    match ctx
                        .call(move |library| library.resolve_all(kind, action))
                        .await
                    {
                        Ok((done, errors)) => {
                            ctx.toast(&trn("{} item done", "{} items done", done as u64));
                            if let Some(first) = errors.first() {
                                ctx.error(&tr("Some items could not be handled"), first);
                            }
                        }
                        Err(error) => ctx.error(&tr("Could not do that"), &error),
                    }
                });
            },
        );
    }
}

impl Section for ReviewPage {
    fn widget(&self) -> gtk::Widget {
        self.nav.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() {
            self.reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        if matches!(
            event,
            Event::ReviewChanged | Event::ContentChanged | Event::SourcesChanged
        ) {
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
