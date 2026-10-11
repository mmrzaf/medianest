//! Running work at the bottom of the sidebar: imports, scans, checks.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use medianest_service::{JobId, JobSnapshot, JobState, Lane};

use crate::context::Ctx;
use crate::i18n::{tr, trf};
use crate::util;

pub struct Activity {
    ctx: Rc<Ctx>,
    root: gtk::Revealer,
    title: gtk::Label,
    detail: gtk::Label,
    bar: gtk::ProgressBar,
    list: gtk::ListBox,
    jobs: RefCell<BTreeMap<JobId, JobSnapshot>>,
}

impl Activity {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption-heading"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["caption", "dim-label", "numeric"])
            .build();
        let bar = gtk::ProgressBar::new();
        let summary = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        summary.append(&title);
        summary.append(&bar);
        summary.append(&detail);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .width_request(340)
            .build();
        popover_content.append(
            &gtk::Label::builder()
                .label(tr("Activity"))
                .css_classes(["heading"])
                .xalign(0.0)
                .build(),
        );
        popover_content.append(&list);
        let popover = gtk::Popover::builder().child(&popover_content).build();
        let button = gtk::MenuButton::builder()
            .child(&summary)
            .popover(&popover)
            .css_classes(["flat", "activity"])
            .tooltip_text(tr("Show Activity"))
            .build();
        let root = gtk::Revealer::builder()
            .child(&button)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .build();
        Rc::new(Self {
            ctx,
            root,
            title,
            detail,
            bar,
            list,
            jobs: RefCell::default(),
        })
    }

    pub fn widget(&self) -> &gtk::Revealer {
        &self.root
    }

    pub fn update(self: &Rc<Self>, job: &JobSnapshot) {
        {
            let mut jobs = self.jobs.borrow_mut();
            if job.state.is_finished() {
                jobs.remove(&job.id);
            } else {
                jobs.insert(job.id, job.clone());
            }
        }
        self.refresh();
    }

    fn refresh(self: &Rc<Self>) {
        let jobs = self.jobs.borrow();
        // Foreground work first: it is what the user asked for.
        let main = jobs
            .values()
            .find(|job| job.lane == Lane::Foreground)
            .or_else(|| jobs.values().next());
        self.root.set_reveal_child(main.is_some());
        let Some(main) = main else {
            return;
        };
        let mut title = main.title.clone();
        if jobs.len() > 1 {
            title = trf("{} (+{} more)", &[&title, &(jobs.len() - 1).to_string()]);
        }
        self.title.set_label(&title);
        self.detail.set_label(&describe(main));
        match main.fraction() {
            Some(fraction) => self.bar.set_fraction(fraction),
            None => self.bar.pulse(),
        }

        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        for job in jobs.values() {
            let row = adw::ActionRow::builder()
                .title(gtk::glib::markup_escape_text(&job.title))
                .subtitle(gtk::glib::markup_escape_text(&describe(job)))
                .build();
            let bar = gtk::ProgressBar::builder()
                .valign(gtk::Align::Center)
                .width_request(60)
                .build();
            if let Some(fraction) = job.fraction() {
                bar.set_fraction(fraction);
            }
            row.add_suffix(&bar);
            let paused = job.state == JobState::Paused;
            let pause = gtk::Button::builder()
                .icon_name(if paused {
                    "media-playback-start-symbolic"
                } else {
                    "media-playback-pause-symbolic"
                })
                .tooltip_text(if paused { tr("Resume") } else { tr("Pause") })
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            let stop = gtk::Button::builder()
                .icon_name("process-stop-symbolic")
                .tooltip_text(tr("Stop"))
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            row.add_suffix(&pause);
            row.add_suffix(&stop);
            let (ctx, id) = (Rc::clone(&self.ctx), job.id);
            pause.connect_clicked(move |_| ctx.library.pause_job(id, !paused));
            let (ctx, id) = (Rc::clone(&self.ctx), job.id);
            stop.connect_clicked(move |_| ctx.library.cancel_job(id));
            self.list.append(&row);
        }
    }
}

fn describe(job: &JobSnapshot) -> String {
    if job.state == JobState::Paused {
        return tr("Paused");
    }
    if job.state == JobState::Queued {
        return tr("Waiting…");
    }
    let mut parts = Vec::new();
    if job.total > 0 {
        parts.push(trf(
            "{} of {}",
            &[&util::number(job.done), &util::number(job.total)],
        ));
    }
    if let Some(remaining) = job.remaining_ms().filter(|ms| *ms > 5_000) {
        let minutes = remaining / 60_000;
        parts.push(if minutes >= 1 {
            trf("about {} min left", &[&minutes.to_string()])
        } else {
            tr("less than a minute left")
        });
    }
    if parts.is_empty() && !job.current.is_empty() {
        parts.push(job.current.clone());
    }
    parts.join(" · ")
}
