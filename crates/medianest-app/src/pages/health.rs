//! Library health: is everything safe, and what can be done about it.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use medianest_service::core::SourceKind;
use medianest_service::{Event, HealthReport, Lane};

use super::Section;
use crate::context::{Ctx, Destination};
use crate::i18n::{tr, trf, trn};
use crate::util::{self, Debounce};

pub struct HealthPage {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    root: adw::ToolbarView,
    status: adw::StatusPage,
    page: adw::PreferencesPage,
    groups: std::cell::RefCell<Vec<adw::PreferencesGroup>>,
    stale: Cell<bool>,
    debounce: Debounce,
}

impl HealthPage {
    pub fn new(ctx: Rc<Ctx>, toggle: gtk::ToggleButton) -> Rc<Self> {
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(&tr("Library Health"), ""))
            .build();
        header.pack_start(&toggle);
        let status = adw::StatusPage::builder()
            .icon_name("security-high-symbolic")
            .css_classes(["compact"])
            .build();
        let page = adw::PreferencesPage::new();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&status);
        content.append(&page);
        page.set_vexpand(true);
        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(&content));
        Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            root,
            status,
            page,
            groups: std::cell::RefCell::default(),
            stale: Cell::new(true),
            debounce: Debounce::new(1000),
        })
    }

    fn reload(&self) {
        self.stale.set(false);
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let report = ctx.call(|library| library.health()).await;
            let Some(this) = weak.upgrade() else { return };
            match report {
                Ok(report) => this.show(&report),
                Err(error) => this.ctx.error(&tr("Could not check the library"), &error),
            }
        });
    }

    fn add_group(&self, group: adw::PreferencesGroup) {
        self.page.add(&group);
        self.groups.borrow_mut().push(group);
    }

    fn show(&self, report: &HealthReport) {
        for group in self.groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }
        let healthy = report.database_ok && report.issues.is_empty();
        self.status.set_title(&tr(&report.headline()));
        self.status.set_icon_name(Some(if healthy {
            "security-high-symbolic"
        } else {
            "dialog-warning-symbolic"
        }));
        if healthy {
            self.status.remove_css_class("health-warning");
            self.status.add_css_class("health-good");
        } else {
            self.status.remove_css_class("health-good");
            self.status.add_css_class("health-warning");
        }
        self.status.set_description(Some(&if healthy {
            tr("Your photos and videos are safe. MediaNest keeps checking them quietly.")
        } else {
            tr("A few things need a look. Nothing has been changed or deleted.")
        }));
        let stats = &report.stats;

        if !report.issues.is_empty() {
            let issues = adw::PreferencesGroup::builder()
                .title(tr("Needs Attention"))
                .build();
            for issue in &report.issues {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&issue.title))
                    .subtitle(glib::markup_escape_text(&issue.detail))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
                if issue.review.is_some() {
                    row.set_activatable(true);
                    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                    let ctx = Rc::clone(&self.ctx);
                    row.connect_activated(move |_| ctx.go(Destination::Review));
                } else if issue.title.contains("backup") {
                    let button = self.backup_button();
                    row.add_suffix(&button);
                } else {
                    row.set_activatable(true);
                    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                    let ctx = Rc::clone(&self.ctx);
                    row.connect_activated(move |_| ctx.go(Destination::Folders));
                }
                issues.add(&row);
            }
            self.add_group(issues);
        }

        let contents = adw::PreferencesGroup::builder()
            .title(tr("Contents"))
            .build();
        let rows = [
            (
                tr("Photos and Videos"),
                util::photos_and_videos(stats.photos, stats.videos),
            ),
            (
                tr("In the Library Folder"),
                trf(
                    "{} · {}",
                    &[
                        &trn("{} file", "{} files", stats.managed_files),
                        &util::bytes(stats.managed_bytes),
                    ],
                ),
            ),
            (
                tr("In Linked Folders"),
                trf(
                    "{} · {}",
                    &[
                        &trn("{} file", "{} files", stats.linked_files),
                        &util::bytes(stats.linked_bytes),
                    ],
                ),
            ),
            (tr("With a Location"), util::items(stats.with_location)),
            (tr("Hidden"), util::items(stats.hidden)),
            (tr("In the Trash"), util::items(stats.trashed)),
        ];
        for (title, value) in rows {
            contents.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(value)
                    .css_classes(["property"])
                    .build(),
            );
        }
        self.add_group(contents);

        let checks = adw::PreferencesGroup::builder()
            .title(tr("Checks"))
            .description(tr(
                "Every file is read again now and then and compared with what was imported.",
            ))
            .build();
        let verified = adw::ActionRow::builder()
            .title(tr("Files Verified"))
            .subtitle(match report.last_verified_ms {
                Some(ms) => trf("Oldest check {}", &[&util::ago(ms)]),
                None => tr("Not checked yet"),
            })
            .build();
        let verify = gtk::Button::builder()
            .label(tr("_Verify Now"))
            .use_underline(true)
            .valign(gtk::Align::Center)
            .build();
        let ctx = Rc::clone(&self.ctx);
        verify.connect_clicked(move |_| {
            ctx.library.verify(None, None, Lane::Foreground);
        });
        verified.add_suffix(&verify);
        checks.add(&verified);
        let database = adw::ActionRow::builder()
            .title(tr("Catalog"))
            .subtitle(if report.database_ok {
                tr("No problems found")
            } else {
                report.database_message.clone()
            })
            .build();
        checks.add(&database);
        let video = adw::ActionRow::builder()
            .title(tr("Video Support"))
            .subtitle(if report.video_tools {
                tr("Available")
            } else {
                tr("FFmpeg is not installed; videos are kept safe but show no thumbnails.")
            })
            .build();
        checks.add(&video);
        self.add_group(checks);

        let backups = adw::PreferencesGroup::builder()
            .title(tr("Catalog Backups"))
            .description(tr(
                "Backups keep your tags, albums, dates and other edits. Your photos themselves are \
                 files in the library folder; back that folder up with your usual backup tool.",
            ))
            .build();
        backups.set_header_suffix(Some(&self.backup_button()));
        if report.backups.is_empty() {
            backups.add(
                &adw::ActionRow::builder()
                    .title(tr("No backups yet"))
                    .build(),
            );
        }
        for backup in report.backups.iter().take(5) {
            let row = adw::ActionRow::builder()
                .title(util::ago(backup.created_ms))
                .subtitle(util::bytes(backup.size))
                .build();
            let restore = gtk::Button::builder()
                .label(tr("_Restore…"))
                .use_underline(true)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let path = backup.path.to_string_lossy().to_string();
            let ctx = Rc::clone(&self.ctx);
            restore.connect_clicked(move |_| {
                let inner = Rc::clone(&ctx);
                let path = path.clone();
                ctx.confirm(
                    &tr("Restore This Backup?"),
                    &tr(
                        "The library closes and reopens with the catalog from this backup. Changes \
                         made since then are lost, but the current catalog is saved first.",
                    ),
                    &tr("_Restore"),
                    true,
                    move || {
                        let _ = WidgetExt::activate_action(
                            &inner.window,
                            "win.restore-backup",
                            Some(&path.to_variant()),
                        );
                    },
                );
            });
            row.add_suffix(&restore);
            backups.add(&row);
        }
        self.add_group(backups);

        let storage = adw::PreferencesGroup::builder().title(tr("Cache")).build();
        storage.add(
            &adw::ActionRow::builder()
                .title(tr("Thumbnails and Previews"))
                .subtitle(trf(
                    "{} · {}",
                    &[
                        &util::bytes(report.cache.thumbnails),
                        &util::bytes(report.cache.previews),
                    ],
                ))
                .css_classes(["property"])
                .build(),
        );
        let failed = stats.thumbs_failed;
        if failed > 0 {
            let row = adw::ActionRow::builder()
                .title(trn(
                    "{} thumbnail could not be made",
                    "{} thumbnails could not be made",
                    failed,
                ))
                .build();
            let retry = gtk::Button::builder()
                .label(tr("Try _Again"))
                .use_underline(true)
                .valign(gtk::Align::Center)
                .build();
            let ctx = Rc::clone(&self.ctx);
            retry.connect_clicked(move |_| {
                let ctx = Rc::clone(&ctx);
                util::spawn(async move {
                    if let Ok(count) = ctx.call(|library| library.rebuild_thumbnails(true)).await {
                        ctx.loader.clear();
                        ctx.toast(&trn(
                            "Retrying {} thumbnail",
                            "Retrying {} thumbnails",
                            count as u64,
                        ));
                    }
                });
            });
            row.add_suffix(&retry);
            storage.add(&row);
        }
        self.add_group(storage);

        let sources = report
            .sources
            .iter()
            .filter(|s| s.kind == SourceKind::Linked)
            .count();
        if sources > 0 {
            let folders = adw::PreferencesGroup::new();
            let row = adw::ActionRow::builder()
                .title(trn("{} linked folder", "{} linked folders", sources as u64))
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let ctx = Rc::clone(&self.ctx);
            row.connect_activated(move |_| ctx.go(Destination::Folders));
            folders.add(&row);
            self.add_group(folders);
        }
    }

    fn backup_button(&self) -> gtk::Button {
        let button = gtk::Button::builder()
            .label(tr("_Back Up Now"))
            .use_underline(true)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let (ctx, weak) = (Rc::clone(&self.ctx), self.this.clone());
        button.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (ctx, weak, button) = (Rc::clone(&ctx), weak.clone(), button.clone());
            util::spawn(async move {
                match ctx.call(|library| library.backup_now()).await {
                    Ok(_) => ctx.toast(&tr("Catalog backed up")),
                    Err(error) => ctx.error(&tr("Could not back up"), &error),
                }
                button.set_sensitive(true);
                if let Some(this) = weak.upgrade() {
                    this.reload();
                }
            });
        });
        button
    }
}

impl Section for HealthPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn shown(&self) {
        self.reload();
    }

    fn event(&self, event: &Event, visible: bool) {
        let relevant = matches!(
            event,
            Event::ContentChanged | Event::ReviewChanged | Event::SourcesChanged
        ) || matches!(event, Event::Job(job) if job.state.is_finished());
        if relevant {
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
