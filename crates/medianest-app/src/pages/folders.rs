//! Folders: the library's own folder and linked folders, browsable like a
//! file manager, with their status.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use medianest_service::catalog::{Scope, Sort, Source, View};
use medianest_service::core::query::Query;
use medianest_service::core::{SourceKind, SourceStatus};
use medianest_service::{Event, FolderNode, Lane};

use super::Section;
use super::timeline::{TimelineKind, TimelineOptions, TimelinePage};
use crate::context::Ctx;
use crate::i18n::{tr, trf, trn};
use crate::util::{self, Debounce};

/// Asks for a folder and links it.
pub fn choose_folder(ctx: &Rc<Ctx>) {
    let dialog = gtk::FileDialog::builder()
        .title(tr("Add a Folder"))
        .accept_label(tr("_Add"))
        .build();
    let ctx = Rc::clone(ctx);
    let window = ctx.window.clone();
    dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
        let Some(path) = result.ok().and_then(|file| file.path()) else {
            return;
        };
        let ctx = Rc::clone(&ctx);
        util::spawn(async move {
            match ctx.call(move |library| library.add_folder(&path)).await {
                Ok(_) => ctx.toast(&tr("Folder added. Its photos appear as they are found.")),
                Err(error) => ctx.error(&tr("Could not add the folder"), &error),
            }
        });
    });
}

pub struct FoldersPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    nav: adw::NavigationView,
    library_group: adw::PreferencesGroup,
    linked_group: adw::PreferencesGroup,
    rows: std::cell::RefCell<Vec<gtk::Widget>>,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl FoldersPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        header.pack_start(&toggle);
        let add = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .action_name("win.add-folder")
            .tooltip_text(tr("Add a Folder"))
            .build();
        header.pack_end(&add);

        let library_group = adw::PreferencesGroup::builder()
            .title(tr("Library"))
            .description(tr("Imported photos and videos, managed by MediaNest."))
            .build();
        let linked_group = adw::PreferencesGroup::builder()
            .title(tr("Linked Folders"))
            .description(tr(
                "Folders shown in your library without being copied. MediaNest never changes \
                 files in them unless you delete a photo and choose to move it to the trash.",
            ))
            .build();
        let add_row = adw::ButtonRow::builder()
            .title(tr("_Add a Folder…"))
            .use_underline(true)
            .start_icon_name("list-add-symbolic")
            .action_name("win.add-folder")
            .build();
        linked_group.add(&add_row);
        let page = adw::PreferencesPage::new();
        page.add(&library_group);
        page.add(&linked_group);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        let nav = adw::NavigationView::new();
        nav.add(
            &adw::NavigationPage::builder()
                .title(tr("Folders"))
                .tag("folders")
                .child(&toolbar)
                .build(),
        );
        Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            nav,
            library_group,
            linked_group,
            rows: std::cell::RefCell::default(),
            stale: Cell::new(true),
            debounce: Debounce::new(400),
        })
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let sources = ctx.call(|library| library.sources()).await;
            let Some(this) = weak.upgrade() else { return };
            match sources {
                Ok(sources) => this.show(sources),
                Err(error) => this.ctx.error(&tr("Could not load folders"), &error),
            }
        });
    }

    fn show(&self, sources: Vec<Source>) {
        for row in self.rows.borrow_mut().drain(..) {
            if let Some(group) = row
                .ancestor(adw::PreferencesGroup::static_type())
                .and_downcast::<adw::PreferencesGroup>()
            {
                group.remove(&row);
            }
        }
        for source in sources {
            let row = self.source_row(&source);
            match source.kind {
                SourceKind::Managed => self.library_group.add(&row),
                SourceKind::Linked => self.linked_group.add(&row),
            }
            self.rows.borrow_mut().push(row.upcast());
        }
    }

    fn source_row(&self, source: &Source) -> adw::ActionRow {
        let mut details = vec![
            util::items(source.file_count),
            util::bytes(source.total_bytes),
        ];
        match source.status {
            SourceStatus::Offline => details.push(tr("not connected")),
            _ if source.problem_count > 0 => {
                details.push(trn("{} problem", "{} problems", source.problem_count))
            }
            _ => {}
        }
        if let Some(scanned) = source.scanned_ms {
            details.push(trf("checked {}", &[&util::ago(scanned)]));
        }
        let title = if source.kind == SourceKind::Managed {
            tr("Library Folder")
        } else {
            source.name.clone()
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&title))
            .subtitle(glib::markup_escape_text(&format!(
                "{}\n{}",
                source.path.display(),
                details.join(" · ")
            )))
            .activatable(true)
            .build();
        let icon = match (source.kind, source.status) {
            (_, SourceStatus::Offline) => "network-offline-symbolic",
            (SourceKind::Managed, _) => "mn-library-symbolic",
            (SourceKind::Linked, _) => "folder-symbolic",
        };
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        if source.kind == SourceKind::Linked {
            let menu = gio::Menu::new();
            menu.append(Some(&tr("_Look for Changes")), Some("source.rescan"));
            menu.append(Some(&tr("_Watch for Changes")), Some("source.watch"));
            menu.append(Some(&tr("_Rename…")), Some("source.rename"));
            menu.append(
                Some(&tr("_Moved to Another Place…")),
                Some("source.relocate"),
            );
            menu.append(Some(&tr("Show in _Files")), Some("source.open"));
            let danger = gio::Menu::new();
            danger.append(Some(&tr("Remove From _Library…")), Some("source.remove"));
            menu.append_section(None, &danger);
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text(tr("Folder Actions"))
                .build();
            row.add_suffix(&more);
            row.insert_action_group("source", Some(&self.source_actions(source)));
        }
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.this.clone();
        let (id, name) = (source.id, title);
        row.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                this.browse(id, None, name.clone());
            }
        });
        row
    }

    fn source_actions(&self, source: &Source) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        let id = source.id;

        let rescan = gio::SimpleAction::new("rescan", None);
        let ctx = Rc::clone(&self.ctx);
        rescan.connect_activate(move |_, _| {
            ctx.library.scan(Some(id), Lane::Foreground);
        });
        group.add_action(&rescan);

        let watch = gio::SimpleAction::new_stateful("watch", None, &source.watch.to_variant());
        let ctx = Rc::clone(&self.ctx);
        watch.connect_activate(move |action, _| {
            let value = !action
                .state()
                .and_then(|s| s.get::<bool>())
                .unwrap_or(false);
            action.set_state(&value.to_variant());
            let ctx = Rc::clone(&ctx);
            util::spawn(async move {
                if let Err(error) = ctx
                    .call(move |library| library.set_source_watch(id, value))
                    .await
                {
                    ctx.error(&tr("Could not change watching"), &error);
                }
            });
        });
        group.add_action(&watch);

        let rename = gio::SimpleAction::new("rename", None);
        let (ctx, current) = (Rc::clone(&self.ctx), source.name.clone());
        rename.connect_activate(move |_, _| {
            let dialog = adw::AlertDialog::new(
                Some(&tr("Rename Folder")),
                Some(&tr(
                    "The name is only used in MediaNest; the folder on disk keeps its name.",
                )),
            );
            let entry = adw::EntryRow::builder()
                .title(tr("Name"))
                .text(&current)
                .activates_default(true)
                .build();
            let list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
            list.append(&entry);
            dialog.set_extra_child(Some(&list));
            dialog.add_responses(&[("cancel", &tr("_Cancel")), ("rename", &tr("_Rename"))]);
            dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("rename"));
            let ctx2 = Rc::clone(&ctx);
            dialog.connect_response(Some("rename"), move |_, _| {
                let name = entry.text().trim().to_owned();
                let ctx = Rc::clone(&ctx2);
                util::spawn(async move {
                    if let Err(error) = ctx
                        .call(move |library| library.rename_source(id, &name))
                        .await
                    {
                        ctx.error(&tr("Could not rename"), &error);
                    }
                });
            });
            dialog.present(Some(&ctx.window));
        });
        group.add_action(&rename);

        let relocate = gio::SimpleAction::new("relocate", None);
        let ctx = Rc::clone(&self.ctx);
        relocate.connect_activate(move |_, _| {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Where Is the Folder Now?"))
                .build();
            let ctx2 = Rc::clone(&ctx);
            chooser.select_folder(Some(&ctx.window), gio::Cancellable::NONE, move |result| {
                let Some(path) = result.ok().and_then(|file| file.path()) else {
                    return;
                };
                let ctx = Rc::clone(&ctx2);
                util::spawn(async move {
                    match ctx
                        .call(move |library| library.relocate_source(id, &path))
                        .await
                    {
                        Ok(_) => ctx.toast(&tr("Checking the folder in its new place")),
                        Err(error) => ctx.error(&tr("Could not use that folder"), &error),
                    }
                });
            });
        });
        group.add_action(&relocate);

        let open = gio::SimpleAction::new("open", None);
        let (ctx, path) = (Rc::clone(&self.ctx), source.path.as_path().to_path_buf());
        open.connect_activate(move |_, _| {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
            launcher.launch(Some(&ctx.window), gio::Cancellable::NONE, |_| {});
        });
        group.add_action(&open);

        let remove = gio::SimpleAction::new("remove", None);
        let (ctx, name, count) = (Rc::clone(&self.ctx), source.name.clone(), source.file_count);
        remove.connect_activate(move |_, _| {
            let ctx2 = Rc::clone(&ctx);
            ctx.confirm(
                &trf("Remove “{}” From the Library?", &[&name]),
                &trf(
                    "{} found there will no longer be shown, with their tags and albums. The \
                     files themselves are not touched.",
                    &[&util::items(count)],
                ),
                &tr("_Remove"),
                true,
                move || {
                    let ctx = ctx2;
                    util::spawn(async move {
                        if let Err(error) = ctx.call(move |library| library.remove_source(id)).await
                        {
                            ctx.error(&tr("Could not remove the folder"), &error);
                        }
                    });
                },
            );
        });
        group.add_action(&remove);
        group
    }

    /// Shows the subfolders of a folder and a way to see its photos.
    fn browse(
        &self,
        source: medianest_service::core::SourceId,
        node: Option<FolderNode>,
        title: String,
    ) {
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let node = match node {
                Some(node) => node,
                None => match ctx.call(move |library| library.folders(source)).await {
                    Ok(node) => node,
                    Err(error) => {
                        ctx.error(&tr("Could not read the folder"), &error);
                        return;
                    }
                },
            };
            if let Some(this) = weak.upgrade() {
                this.push_folder(source, node, title);
            }
        });
    }

    fn push_folder(
        &self,
        source: medianest_service::core::SourceId,
        node: FolderNode,
        title: String,
    ) {
        let page = adw::PreferencesPage::new();
        let all = adw::PreferencesGroup::new();
        let photos = adw::ActionRow::builder()
            .title(tr("All Photos and Videos"))
            .subtitle(util::items(node.total))
            .activatable(true)
            .build();
        photos.add_prefix(&gtk::Image::from_icon_name("image-x-generic-symbolic"));
        photos.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        all.add(&photos);
        page.add(&all);
        let weak = self.this.clone();
        let (relpath, folder_title) = (node.relpath.clone(), title.clone());
        photos.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                let view = View {
                    scope: Scope::Directory {
                        source,
                        relpath: relpath.clone(),
                    },
                    query: Query::default(),
                    sort: Sort::NewestFirst,
                };
                let timeline = TimelinePage::new(
                    Rc::clone(&this.ctx),
                    TimelineOptions {
                        title: folder_title.clone(),
                        view,
                        kind: TimelineKind::Filtered,
                        searchable: false,
                        empty_icon: "folder-symbolic",
                        empty_title: tr("Empty Folder"),
                        empty_description: String::new(),
                        sidebar_toggle: None,
                    },
                );
                this.nav.push(&timeline.navigation_page());
            }
        });
        if !node.children.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title(tr("Folders"))
                .build();
            for child in node.children {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&child.name))
                    .subtitle(util::items(child.total))
                    .activatable(true)
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let weak = self.this.clone();
                let name = child.name.clone();
                row.connect_activated(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.push_folder(source, child.clone(), name.clone());
                    }
                });
                group.add(&row);
            }
            page.add(&group);
        }
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        self.nav.push(
            &adw::NavigationPage::builder()
                .title(&title)
                .child(&toolbar)
                .build(),
        );
    }
}

impl Section for FoldersPage {
    fn widget(&self) -> gtk::Widget {
        self.nav.clone().upcast()
    }

    fn shown(&self) {
        if self.stale.get() {
            self.reload();
        }
    }

    fn event(&self, event: &Event, visible: bool) {
        if matches!(event, Event::SourcesChanged | Event::ContentChanged) {
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
