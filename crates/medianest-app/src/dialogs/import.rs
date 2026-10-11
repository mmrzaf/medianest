//! Importing: choose a card, phone or folder, see what would happen, then
//! copy, move or link.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use medianest_service::vault::{Control, ImportMode};
use medianest_service::{ImportOptions, ImportPlan};

use crate::context::Ctx;
use crate::i18n::{tr, trf, trn};
use crate::util;

/// Opens the import dialog at the start.
pub fn choose(ctx: &Rc<Ctx>) {
    let dialog = ImportDialog::new(Rc::clone(ctx));
    dialog.present();
}

/// Opens the import dialog for files and folders chosen elsewhere.
pub fn start(ctx: &Rc<Ctx>, paths: Vec<PathBuf>) {
    let dialog = ImportDialog::new(Rc::clone(ctx));
    dialog.present();
    dialog.plan(paths);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Copy,
    Move,
    Link,
}

struct ImportDialog {
    this: std::rc::Weak<Self>,
    ctx: Rc<Ctx>,
    dialog: adw::Dialog,
    nav: adw::NavigationView,
    devices: adw::PreferencesGroup,
    progress: gtk::ProgressBar,
    progress_label: gtk::Label,
    control: RefCell<Option<Control>>,
    paths: RefCell<Vec<PathBuf>>,
    plan_result: RefCell<Option<ImportPlan>>,
    mode: Cell<Mode>,
    keep: RefCell<Option<Rc<Self>>>,
}

impl ImportDialog {
    fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let nav = adw::NavigationView::new();
        let dialog = adw::Dialog::builder()
            .title(tr("Import"))
            .content_width(560)
            .content_height(640)
            .child(&nav)
            .build();
        let devices = adw::PreferencesGroup::builder()
            .title(tr("Cameras, Phones and Cards"))
            .visible(false)
            .build();
        let progress = gtk::ProgressBar::builder().show_text(false).build();
        let progress_label = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .wrap(true)
            .build();
        let this = Rc::new_cyclic(|weak| Self {
            this: weak.clone(),
            ctx,
            dialog,
            nav,
            devices,
            progress,
            progress_label,
            control: RefCell::default(),
            paths: RefCell::default(),
            plan_result: RefCell::default(),
            mode: Cell::new(Mode::Copy),
            keep: RefCell::default(),
        });
        *this.keep.borrow_mut() = Some(Rc::clone(&this));
        let weak = this.this.clone();
        this.dialog.connect_closed(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(control) = this.control.take() {
                    control.cancel();
                }
                this.keep.take();
            }
        });
        this.nav.add(&this.source_page());
        this.find_devices();
        this
    }

    fn present(&self) {
        self.dialog.present(Some(&self.ctx.window));
    }

    // ----- choosing what to import --------------------------------------------

    fn source_page(&self) -> adw::NavigationPage {
        let folder = adw::ActionRow::builder()
            .title(tr("Choose a Folder…"))
            .subtitle(tr("Everything inside it, including subfolders"))
            .activatable(true)
            .build();
        folder.add_prefix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        folder.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let files = adw::ActionRow::builder()
            .title(tr("Choose Files…"))
            .subtitle(tr("Individual photos and videos"))
            .activatable(true)
            .build();
        files.add_prefix(&gtk::Image::from_icon_name("image-x-generic-symbolic"));
        files.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let choose = adw::PreferencesGroup::new();
        choose.add(&folder);
        choose.add(&files);

        let weak = self.this.clone();
        folder.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                this.pick(true);
            }
        });
        let weak = self.this.clone();
        files.connect_activated(move |_| {
            if let Some(this) = weak.upgrade() {
                this.pick(false);
            }
        });

        let intro = adw::PreferencesGroup::builder()
            .description(tr(
                "MediaNest first looks at what you chose and shows what would happen. Nothing \
                 is copied until you confirm, and originals are never changed.",
            ))
            .build();
        let page = adw::PreferencesPage::new();
        page.add(&intro);
        page.add(&self.devices);
        page.add(&choose);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        adw::NavigationPage::builder()
            .title(tr("Import"))
            .tag("source")
            .child(&toolbar)
            .build()
    }

    /// Lists removable drives and cameras that have photos.
    fn find_devices(&self) {
        let monitor = gio::VolumeMonitor::get();
        let mut found = 0;
        for mount in monitor.mounts() {
            let Some(root) = mount.root().path() else {
                continue;
            };
            let removable = mount.can_unmount()
                || mount
                    .drive()
                    .is_some_and(|drive| drive.is_removable() || drive.is_media_removable());
            let has_media =
                root.join("DCIM").is_dir() || root.join("Internal shared storage/DCIM").is_dir();
            if !removable && !has_media {
                continue;
            }
            let target = if root.join("DCIM").is_dir() {
                root.join("DCIM")
            } else {
                root.clone()
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&mount.name()))
                .subtitle(glib::markup_escape_text(&root.display().to_string()))
                .activatable(true)
                .build();
            let icon = gtk::Image::from_gicon(&mount.symbolic_icon());
            row.add_prefix(&icon);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = self.this.clone();
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.plan(vec![target.clone()]);
                }
            });
            self.devices.add(&row);
            found += 1;
        }
        self.devices.set_visible(found > 0);
    }

    fn pick(&self, folders: bool) {
        let dialog = gtk::FileDialog::builder().modal(true).build();
        let weak = self.this.clone();
        let window = self.ctx.window.clone();
        if folders {
            dialog.set_title(&tr("Import Folders"));
            dialog.select_multiple_folders(Some(&window), gio::Cancellable::NONE, move |result| {
                if let (Some(this), Ok(list)) = (weak.upgrade(), result) {
                    this.plan(paths_of(&list));
                }
            });
        } else {
            dialog.set_title(&tr("Import Files"));
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&tr("Photos and Videos")));
            filter.add_mime_type("image/*");
            filter.add_mime_type("video/*");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            dialog.open_multiple(Some(&window), gio::Cancellable::NONE, move |result| {
                if let (Some(this), Ok(list)) = (weak.upgrade(), result) {
                    this.plan(paths_of(&list));
                }
            });
        }
    }

    // ----- looking -------------------------------------------------------------

    fn plan(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        *self.paths.borrow_mut() = paths.clone();
        self.nav.push(&self.scanning_page());
        let (sender, receiver) = async_channel::bounded::<(u64, u64)>(1);
        let control = Control::new();
        *self.control.borrow_mut() = Some(control.clone());
        let weak = self.this.clone();
        util::spawn(async move {
            while let Ok((done, total)) = receiver.recv().await {
                let Some(this) = weak.upgrade() else { break };
                if total > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    this.progress.set_fraction(done as f64 / total as f64);
                    this.progress_label.set_label(&trf(
                        "Examined {} of {}",
                        &[&util::number(done), &util::number(total)],
                    ));
                } else {
                    this.progress.pulse();
                }
            }
        });
        let ctx = Rc::clone(&self.ctx);
        let weak = self.this.clone();
        util::spawn(async move {
            let result = ctx
                .call(move |library| {
                    library.plan_import(&paths, &control, &move |done, total| {
                        let _ = sender.try_send((done, total));
                    })
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            this.control.take();
            match result {
                Ok(plan) => this.show_plan(plan),
                Err(error) => {
                    this.nav.pop_to_tag("source");
                    if !error.contains("stopped") {
                        this.ctx.error(&tr("Could not import"), &error);
                    }
                }
            }
        });
    }

    fn scanning_page(&self) -> adw::NavigationPage {
        self.progress.set_fraction(0.0);
        self.progress_label
            .set_label(&tr("Finding photos and videos…"));
        let spinner = adw::Spinner::builder()
            .width_request(48)
            .height_request(48)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .valign(gtk::Align::Center)
            .margin_start(48)
            .margin_end(48)
            .build();
        content.append(&spinner);
        content.append(
            &gtk::Label::builder()
                .label(tr("Looking at Your Files"))
                .css_classes(["title-2"])
                .build(),
        );
        content.append(&self.progress);
        content.append(&self.progress_label);
        let cancel = gtk::Button::builder()
            .label(tr("_Stop"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        content.append(&cancel);
        let weak = self.this.clone();
        cancel.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                if let Some(control) = this.control.take() {
                    control.cancel();
                }
                this.nav.pop_to_tag("source");
            }
        });
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&content));
        adw::NavigationPage::builder()
            .title(tr("Looking…"))
            .tag("scanning")
            .can_pop(false)
            .child(&toolbar)
            .build()
    }

    // ----- confirming ----------------------------------------------------------

    fn show_plan(self: &Rc<Self>, plan: ImportPlan) {
        let page = self.plan_page(&plan);
        *self.plan_result.borrow_mut() = Some(plan);
        self.nav.replace(&[
            self.nav
                .find_page("source")
                .unwrap_or_else(|| self.source_page()),
            page,
        ]);
    }

    fn plan_page(self: &Rc<Self>, plan: &ImportPlan) -> adw::NavigationPage {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(18)
            .margin_end(18)
            .build();

        let nothing_new = plan.new_count == 0;
        let found_nothing = plan.items.is_empty();
        let headline = gtk::Label::builder()
            .label(if found_nothing {
                tr("No Photos or Videos Found")
            } else if nothing_new {
                tr("Already in Your Library")
            } else {
                util::photos_and_videos(plan.photos, plan.videos)
            })
            .css_classes(["title-1"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let mut facts = Vec::new();
        if found_nothing {
            facts.push(tr(
                "There is nothing MediaNest can import in what you chose.",
            ));
        } else if nothing_new {
            facts.push(tr("Everything here has been imported before."));
        } else {
            facts.push(util::bytes(plan.new_bytes));
        }
        if let Some((first, last)) = plan.date_range {
            let (from, to) = (util::month_title(first), util::month_title(last));
            facts.push(if from == to {
                from
            } else {
                format!("{from} – {to}")
            });
        }
        let subtitle = gtk::Label::builder()
            .label(facts.join(" · "))
            .css_classes(["dim-label"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let heading = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        heading.append(&headline);
        heading.append(&subtitle);
        content.append(&heading);

        if plan.months.len() > 2 {
            content.append(&month_chart(&plan.months));
        }

        // What will be skipped, and why.
        let notes = adw::PreferencesGroup::new();
        let add_note = |count: u64, icon: &str, title: String, detail: String| {
            if count == 0 {
                return;
            }
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(detail)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            notes.add(&row);
        };
        add_note(
            plan.already_in_library,
            "object-select-symbolic",
            trn(
                "{} item already in the library",
                "{} items already in the library",
                plan.already_in_library,
            ),
            tr("Identical copies are recognised by their content and skipped."),
        );
        add_note(
            plan.duplicates_in_selection,
            "edit-copy-symbolic",
            trn(
                "{} duplicate in this selection",
                "{} duplicates in this selection",
                plan.duplicates_in_selection,
            ),
            tr("Only one copy of each is imported."),
        );
        add_note(
            plan.uncertain_dates,
            "x-office-calendar-symbolic",
            trn(
                "{} item without a reliable date",
                "{} items without a reliable date",
                plan.uncertain_dates,
            ),
            tr(
                "They go to the “Unsorted” folder and appear in Review, where you can set the date.",
            ),
        );
        add_note(
            plan.unreadable,
            "dialog-warning-symbolic",
            trn(
                "{} file could not be read",
                "{} files could not be read",
                plan.unreadable,
            ),
            tr("They may be damaged or still being written. They are left alone."),
        );
        add_note(
            plan.other_files,
            "text-x-generic-symbolic",
            trn("{} other file", "{} other files", plan.other_files),
            tr("Files that are not photos or videos are left alone."),
        );
        if notes.first_child().is_some() {
            content.append(&notes);
        }

        // How.
        let single_folder = {
            let paths = self.paths.borrow();
            paths.len() == 1 && paths[0].is_dir()
        };
        let toggle = adw::ToggleGroup::new();
        toggle.add(
            adw::Toggle::builder()
                .name("copy")
                .label(tr("Copy"))
                .build(),
        );
        toggle.add(
            adw::Toggle::builder()
                .name("move")
                .label(tr("Move"))
                .build(),
        );
        if single_folder {
            toggle.add(
                adw::Toggle::builder()
                    .name("link")
                    .label(tr("Keep in Place"))
                    .build(),
            );
        }
        toggle.set_active_name(Some("copy"));
        toggle.set_homogeneous(true);
        let explanation = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        let how = adw::PreferencesGroup::builder()
            .title(tr("How to Import"))
            .build();
        let how_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        how_box.append(&toggle);
        how_box.append(&explanation);
        how.add(&how_box);
        // Moving can still remove originals that are already imported.
        how.set_visible(!nothing_new || plan.already_in_library > 0);
        content.append(&how);

        let button = gtk::Button::builder()
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .use_underline(true)
            .build();
        content.append(&button);
        let again = gtk::Button::builder()
            .label(tr("_Choose Something Else"))
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .visible(nothing_new)
            .build();
        content.append(&again);
        let nav = self.nav.clone();
        again.connect_clicked(move |_| {
            nav.pop_to_tag("source");
        });

        let update = {
            let explanation = explanation.clone();
            let button = button.clone();
            let weak = self.this.clone();
            let new_count = plan.new_count;
            let already = plan.already_in_library;
            move |name: &str| {
                let Some(this) = weak.upgrade() else { return };
                let mode = match name {
                    "move" => Mode::Move,
                    "link" => Mode::Link,
                    _ => Mode::Copy,
                };
                this.mode.set(mode);
                let (text, label, enabled) = match mode {
                    Mode::Copy => (
                        tr(
                            "Originals stay where they are. MediaNest keeps its own copy, checked byte for byte.",
                        ),
                        trn("_Import {} Item", "_Import {} Items", new_count),
                        new_count > 0,
                    ),
                    Mode::Move => (
                        tr(
                            "Each original is copied and checked first. Removing the originals \
                             waits in Review until you approve it, so nothing is lost by mistake.",
                        ),
                        trn("_Move {} Item", "_Move {} Items", new_count + already),
                        new_count + already > 0,
                    ),
                    Mode::Link => (
                        tr(
                            "Nothing is copied. The folder is watched and its photos appear in your \
                             library, but they are only available while the folder is.",
                        ),
                        tr("_Add Folder"),
                        true,
                    ),
                };
                explanation.set_label(&text);
                button.set_label(&label);
                button.set_visible(enabled);
            }
        };
        update("copy");
        toggle.connect_active_name_notify(move |toggle| {
            update(toggle.active_name().as_deref().unwrap_or("copy"));
        });

        let weak = self.this.clone();
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.run();
            }
        });

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(
                &adw::Clamp::builder()
                    .maximum_size(520)
                    .child(&content)
                    .build(),
            )
            .propagate_natural_height(true)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scroller));
        adw::NavigationPage::builder()
            .title(tr("Ready to Import"))
            .tag("plan")
            .child(&toolbar)
            .build()
    }

    fn run(&self) {
        let Some(plan) = self.plan_result.take() else {
            return;
        };
        let library = &self.ctx.library;
        match self.mode.get() {
            Mode::Copy => {
                library.import(
                    plan,
                    ImportOptions {
                        mode: ImportMode::Copy,
                    },
                );
            }
            Mode::Move => {
                library.import(
                    plan,
                    ImportOptions {
                        mode: ImportMode::Move,
                    },
                );
            }
            Mode::Link => {
                let Some(folder) = self.paths.borrow().first().cloned() else {
                    return;
                };
                let ctx = Rc::clone(&self.ctx);
                util::spawn(async move {
                    if let Err(error) = ctx.call(move |library| library.add_folder(&folder)).await {
                        ctx.error(&tr("Could not add the folder"), &error);
                    }
                });
            }
        }
        self.ctx
            .toast(&tr("Import started. You can keep working while it runs."));
        self.dialog.close();
    }
}

fn paths_of(list: &gio::ListModel) -> Vec<PathBuf> {
    (0..list.n_items())
        .filter_map(|index| list.item(index).and_downcast::<gio::File>())
        .filter_map(|file| file.path())
        .collect()
}

/// A small bar chart of new items per month.
fn month_chart(months: &[(i32, u32, u64)]) -> gtk::Widget {
    let months = months.to_vec();
    let area = gtk::DrawingArea::builder()
        .content_height(72)
        .hexpand(true)
        .css_classes(["month-chart"])
        .build();
    let first = months
        .first()
        .map(|(y, m, _)| month_label(*y, *m).to_string())
        .unwrap_or_default();
    let last = months
        .last()
        .map(|(y, m, _)| month_label(*y, *m).to_string())
        .unwrap_or_default();
    let tooltip = months
        .iter()
        .rev()
        .take(24)
        .map(|(y, m, n)| format!("{}: {}", month_label(*y, *m), util::number(*n)))
        .collect::<Vec<_>>()
        .join("\n");
    area.set_tooltip_text(Some(&tooltip));
    area.set_draw_func(move |area, cr, width, height| {
        let max = months.iter().map(|(_, _, n)| *n).max().unwrap_or(1).max(1);
        let accent = adw::StyleManager::default().accent_color_rgba();
        cr.set_source_rgba(
            f64::from(accent.red()),
            f64::from(accent.green()),
            f64::from(accent.blue()),
            0.85,
        );
        #[allow(clippy::cast_precision_loss)]
        let bar = (f64::from(width) / months.len() as f64).min(18.0);
        #[allow(clippy::cast_precision_loss)]
        let left = (f64::from(width) - bar * months.len() as f64) / 2.0;
        let gap = if bar > 4.0 { 2.0 } else { 0.0 };
        for (index, (_, _, count)) in months.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let h = (*count as f64 / max as f64) * f64::from(height - 4);
            #[allow(clippy::cast_precision_loss)]
            let x = left + index as f64 * bar;
            cr.rectangle(x, f64::from(height) - h, (bar - gap).max(1.0), h.max(2.0));
        }
        let _ = cr.fill();
        let _ = area;
    });
    let labels = gtk::Label::builder()
        .label(if first == last {
            first
        } else {
            format!("{first} – {last}")
        })
        .css_classes(["caption", "dim-label"])
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    column.append(&area);
    column.append(&labels);
    column.upcast()
}

fn month_label(year: i32, month: u32) -> String {
    chrono::NaiveDate::from_ymd_opt(year, month, 15)
        .and_then(|date| date.and_hms_opt(12, 0, 0))
        .map(|date| util::month_title(date.and_utc().timestamp_millis()))
        .unwrap_or_default()
}
