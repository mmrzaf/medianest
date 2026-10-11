//! All tags, with counts; rename, merge and delete.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::Event;
use medianest_service::catalog::{Tag, View};
use medianest_service::core::query::Query;

use super::Section;
use super::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::context::Ctx;
use crate::i18n::{tr, trf};
use crate::util::{self, Debounce};

pub struct TagsPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    list: gtk::ListBox,
    filter: gtk::SearchEntry,
    stack: gtk::Stack,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl TagsPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        header.pack_start(&toggle);
        let filter = gtk::SearchEntry::builder()
            .placeholder_text(tr("Filter tags"))
            .build();
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&filter);
        content.append(&list);
        let clamp = adw::Clamp::builder()
            .maximum_size(640)
            .child(&content)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("mn-tag-symbolic")
            .title(tr("No Tags"))
            .description(tr(
                "Select photos and press T to tag them. Tags make photos easy to find.",
            ))
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("tags"));
        stack.add_named(&empty, Some("empty"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let nav = adw::NavigationView::new();
        nav.add(
            &adw::NavigationPage::builder()
                .title(tr("Tags"))
                .tag("tags")
                .child(&toolbar)
                .build(),
        );
        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            nav,
            list,
            filter,
            stack,
            stale: Cell::new(true),
            debounce: Debounce::new(300),
        });
        let list = this.list.clone();
        this.filter.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            let mut child = list.first_child();
            while let Some(widget) = child {
                if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
                    row.set_visible(
                        needle.is_empty() || row.title().to_lowercase().contains(&needle),
                    );
                }
                child = widget.next_sibling();
            }
        });
        this
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let tags = ctx.call(|library| library.read(|db| db.tags())).await;
            let Some(this) = weak.upgrade() else { return };
            match tags {
                Ok(tags) => this.show(tags),
                Err(error) => this.ctx.error(&tr("Could not load tags"), &error),
            }
        });
    }

    fn show(&self, tags: Vec<Tag>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        self.stack
            .set_visible_child_name(if tags.is_empty() { "empty" } else { "tags" });
        self.filter.set_visible(tags.len() > 12);
        let names: Rc<Vec<Tag>> = Rc::new(tags.clone());
        for tag in tags {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&tag.name))
                .subtitle(util::items(tag.count))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("mn-tag-symbolic"));
            let menu = gtk::gio::Menu::new();
            menu.append(Some(&tr("_Rename…")), Some("tag.rename"));
            menu.append(Some(&tr("_Merge Into…")), Some("tag.merge"));
            menu.append(Some(&tr("_Delete…")), Some("tag.delete"));
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text(tr("Tag Actions"))
                .build();
            row.add_suffix(&more);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            row.insert_action_group("tag", Some(&self.tag_actions(&tag, Rc::clone(&names))));
            let weak = self.this.clone();
            let name = tag.name.clone();
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.open(&name);
                }
            });
            self.list.append(&row);
        }
    }

    fn open(&self, name: &str) {
        let query = format!("tag:\"{}\"", name.replace('"', ""));
        let page = TimelinePage::new(
            Rc::clone(&self.ctx),
            TimelineOptions {
                title: name.to_owned(),
                view: View::library().with_query(Query::parse(&query)),
                kind: TimelineKind::Filtered,
                searchable: false,
                empty_icon: "mn-tag-symbolic",
                empty_title: tr("Nothing Tagged"),
                empty_description: String::new(),
                sidebar_toggle: None,
            },
        );
        self.nav.push(&page.navigation_page());
    }

    fn tag_actions(&self, tag: &Tag, all: Rc<Vec<Tag>>) -> gtk::gio::SimpleActionGroup {
        let group = gtk::gio::SimpleActionGroup::new();
        let rename = gtk::gio::SimpleAction::new("rename", None);
        let (ctx, tag_ref) = (Rc::clone(&self.ctx), tag.clone());
        rename.connect_activate(move |_, _| {
            let dialog = adw::AlertDialog::new(Some(&tr("Rename Tag")), None);
            let entry = adw::EntryRow::builder()
                .title(tr("Name"))
                .text(&tag_ref.name)
                .activates_default(true)
                .build();
            let list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
            list.append(&entry);
            dialog.set_extra_child(Some(&list));
            dialog.add_responses(&[("cancel", &tr("_Cancel")), ("rename", &tr("_Rename"))]);
            dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("rename"));
            let (ctx2, id) = (Rc::clone(&ctx), tag_ref.id);
            dialog.connect_response(Some("rename"), move |_, _| {
                let name = entry.text().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let ctx = Rc::clone(&ctx2);
                util::spawn(async move {
                    if let Err(error) = ctx.call(move |library| library.rename_tag(id, &name)).await
                    {
                        ctx.error(&tr("Could not rename the tag"), &error);
                    }
                });
            });
            dialog.present(Some(&ctx.window));
        });
        group.add_action(&rename);

        let merge = gtk::gio::SimpleAction::new("merge", None);
        let (ctx, tag_ref) = (Rc::clone(&self.ctx), tag.clone());
        merge.connect_activate(move |_, _| {
            let others: Vec<Tag> = all.iter().filter(|t| t.id != tag_ref.id).cloned().collect();
            if others.is_empty() {
                return;
            }
            let dialog = adw::AlertDialog::new(
                Some(&trf("Merge “{}” Into…", &[&tag_ref.name])),
                Some(&tr("Photos keep the other tag; this one is removed.")),
            );
            let names: Vec<String> = others.iter().map(|t| t.name.clone()).collect();
            let model = gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
            let combo = adw::ComboRow::builder()
                .title(tr("Tag"))
                .model(&model)
                .enable_search(true)
                .build();
            let list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
            list.append(&combo);
            dialog.set_extra_child(Some(&list));
            dialog.add_responses(&[("cancel", &tr("_Cancel")), ("merge", &tr("_Merge"))]);
            dialog.set_response_appearance("merge", adw::ResponseAppearance::Suggested);
            let (ctx2, from) = (Rc::clone(&ctx), tag_ref.id);
            dialog.connect_response(Some("merge"), move |_, _| {
                let Some(into) = others.get(combo.selected() as usize).map(|t| t.id) else {
                    return;
                };
                let ctx = Rc::clone(&ctx2);
                util::spawn(async move {
                    if let Err(error) = ctx
                        .call(move |library| library.merge_tags(from, into))
                        .await
                    {
                        ctx.error(&tr("Could not merge the tags"), &error);
                    }
                });
            });
            dialog.present(Some(&ctx.window));
        });
        group.add_action(&merge);

        let delete = gtk::gio::SimpleAction::new("delete", None);
        let (ctx, tag_ref) = (Rc::clone(&self.ctx), tag.clone());
        delete.connect_activate(move |_, _| {
            let (ctx2, id) = (Rc::clone(&ctx), tag_ref.id);
            ctx.confirm(
                &trf("Delete the Tag “{}”?", &[&tag_ref.name]),
                &tr("The tag is removed from every photo. The photos stay in your library."),
                &tr("_Delete"),
                true,
                move || {
                    let ctx = ctx2;
                    util::spawn(async move {
                        if let Err(error) = ctx.call(move |library| library.delete_tag(id)).await {
                            ctx.error(&tr("Could not delete the tag"), &error);
                        }
                    });
                },
            );
        });
        group.add_action(&delete);
        group
    }
}

impl Section for TagsPage {
    fn widget(&self) -> gtk::Widget {
        self.nav.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() {
            self.reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        if matches!(event, Event::TagsChanged | Event::ContentChanged) {
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
