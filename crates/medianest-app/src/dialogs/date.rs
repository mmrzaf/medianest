//! Correcting capture dates: set an exact date, shift by an amount (for a
//! camera clock that was wrong), or go back to the date in the file.

use std::rc::Rc;

use adw::prelude::*;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use medianest_service::core::{AssetId, CaptureTime, Confidence, DateSource};

use crate::context::Ctx;
use crate::i18n::{tr, trn};
use crate::util;

const DAY_MS: i64 = 86_400_000;

pub fn show(ctx: &Rc<Ctx>, ids: Vec<AssetId>) {
    let count = ids.len() as u64;
    let ctx = Rc::clone(ctx);
    util::spawn(async move {
        // Start from the first item's current date.
        let first = ids[0];
        let current = ctx
            .call(move |library| Ok(library.read(|db| db.asset(first))?.taken))
            .await
            .ok()
            .and_then(|taken| taken.local_ms);
        build(&ctx, ids, count, current);
    });
}

fn spin(lower: f64, upper: f64, value: f64) -> gtk::SpinButton {
    let adjustment = gtk::Adjustment::new(value, lower, upper, 1.0, 10.0, 0.0);
    gtk::SpinButton::builder()
        .adjustment(&adjustment)
        .numeric(true)
        .valign(gtk::Align::Center)
        .build()
}

fn build(ctx: &Rc<Ctx>, ids: Vec<AssetId>, count: u64, current: Option<i64>) {
    let start = current
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.naive_utc()))
        .unwrap_or_else(|| chrono::Local::now().naive_local());

    // Set an exact date.
    let calendar = gtk::Calendar::builder().build();
    if let Ok(date) = gtk::glib::DateTime::from_local(
        chrono::Datelike::year(&start),
        i32::try_from(chrono::Datelike::month(&start)).unwrap_or(1),
        i32::try_from(chrono::Datelike::day(&start)).unwrap_or(1),
        0,
        0,
        0.0,
    ) {
        calendar.select_day(&date);
    }
    let hour = spin(0.0, 23.0, f64::from(chrono::Timelike::hour(&start)));
    let minute = spin(0.0, 59.0, f64::from(chrono::Timelike::minute(&start)));
    let time_row = adw::ActionRow::builder().title(tr("Time")).build();
    let time_box = gtk::Box::builder()
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    time_box.append(&hour);
    time_box.append(&gtk::Label::new(Some(":")));
    time_box.append(&minute);
    time_row.add_suffix(&time_box);
    let set_group = adw::PreferencesGroup::builder()
        .description(if count > 1 {
            tr("Every selected item gets this date and time.")
        } else {
            String::new()
        })
        .build();
    let calendar_box = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .margin_bottom(12)
        .build();
    calendar_box.append(&calendar);
    set_group.add(&calendar_box);
    set_group.add(&time_row);

    // Shift.
    let days = adw::SpinRow::with_range(-36_500.0, 36_500.0, 1.0);
    days.set_title(&tr("Days"));
    let hours = adw::SpinRow::with_range(-23.0, 23.0, 1.0);
    hours.set_title(&tr("Hours"));
    let minutes = adw::SpinRow::with_range(-59.0, 59.0, 1.0);
    minutes.set_title(&tr("Minutes"));
    for row in [&days, &hours, &minutes] {
        row.set_value(0.0);
    }
    let shift_group = adw::PreferencesGroup::builder()
        .description(tr(
            "Moves every selected item by the same amount, keeping their order. Useful when a \
             camera's clock was set wrong or to another time zone.",
        ))
        .build();
    shift_group.add(&days);
    shift_group.add(&hours);
    shift_group.add(&minutes);
    let preview = gtk::Label::builder()
        .css_classes(["dim-label"])
        .wrap(true)
        .xalign(0.0)
        .margin_top(12)
        .build();
    shift_group.add(&preview);

    let reset_status = adw::StatusPage::builder()
        .icon_name("edit-undo-symbolic")
        .title(tr("Use the Date From the File"))
        .description(tr(
            "Undo manual changes and go back to the date read from each file.",
        ))
        .build();
    reset_status.add_css_class("compact");

    let stack = adw::ViewStack::new();
    stack.add_titled(&set_group, Some("set"), &tr("Set"));
    stack.add_titled(&shift_group, Some("shift"), &tr("Shift"));
    stack.add_titled(&reset_status, Some("reset"), &tr("Reset"));
    stack.set_visible_child_name(if count > 1 { "shift" } else { "set" });
    let switcher = adw::InlineViewSwitcher::builder()
        .stack(&stack)
        .margin_bottom(12)
        .build();

    let apply = gtk::Button::builder()
        .label(tr("_Apply"))
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::builder()
        .label(tr("_Cancel"))
        .use_underline(true)
        .build();
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&apply);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&switcher);
    content.append(&stack);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(trn(
            "Change Date of {} Item",
            "Change Date of {} Items",
            count,
        ))
        .content_width(420)
        .child(&toolbar)
        .build();

    let shift_ms = {
        let (days, hours, minutes) = (days.clone(), hours.clone(), minutes.clone());
        move || {
            #[allow(clippy::cast_possible_truncation)]
            let value = days.value() as i64 * DAY_MS
                + hours.value() as i64 * 3_600_000
                + minutes.value() as i64 * 60_000;
            value
        }
    };
    let update_preview = {
        let preview = preview.clone();
        let shift_ms = shift_ms.clone();
        move || {
            let shift = shift_ms();
            match current {
                Some(ms) if shift != 0 => preview.set_label(&format!(
                    "{} → {}",
                    util::date_time(ms),
                    util::date_time(ms + shift)
                )),
                _ => preview.set_label(""),
            }
        }
    };
    for row in [&days, &hours, &minutes] {
        let update = update_preview.clone();
        row.connect_value_notify(move |_| update());
    }

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    let window = ctx.window.clone();
    let ctx = Rc::clone(ctx);
    let dialog_ref = dialog.clone();
    apply.connect_clicked(move |_| {
        let mode = stack.visible_child_name().unwrap_or_default().to_string();
        let ctx = Rc::clone(&ctx);
        let ids = ids.clone();
        dialog_ref.close();
        match mode.as_str() {
            "set" => {
                let date = calendar.date();
                let naive = NaiveDate::from_ymd_opt(
                    date.year(),
                    date.month().unsigned_abs(),
                    date.day_of_month().unsigned_abs(),
                )
                .and_then(|day| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let time =
                        NaiveTime::from_hms_opt(hour.value() as u32, minute.value() as u32, 0)?;
                    Some(NaiveDateTime::new(day, time))
                });
                let Some(naive) = naive else { return };
                let time = CaptureTime::floating(naive, DateSource::Manual, Confidence::High);
                util::spawn(async move {
                    match ctx.call(move |library| library.set_taken(&ids, time)).await {
                        Ok(()) => ctx.toast(&tr("Date changed")),
                        Err(error) => ctx.error(&tr("Could not change the date"), &error),
                    }
                });
            }
            "shift" => {
                let shift = shift_ms();
                if shift == 0 {
                    return;
                }
                util::spawn(async move {
                    let change = ids.clone();
                    match ctx
                        .call(move |library| library.shift_taken(&change, shift))
                        .await
                    {
                        Ok(()) => {
                            let undo_ctx = Rc::clone(&ctx);
                            ctx.toast_undo(
                                &trn("{} date shifted", "{} dates shifted", ids.len() as u64),
                                move || {
                                    let ctx = undo_ctx;
                                    util::spawn(async move {
                                        let _ = ctx
                                            .call(move |library| library.shift_taken(&ids, -shift))
                                            .await;
                                    });
                                },
                            );
                        }
                        Err(error) => ctx.error(&tr("Could not shift the dates"), &error),
                    }
                });
            }
            _ => {
                util::spawn(async move {
                    match ctx.call(move |library| library.reset_taken(&ids)).await {
                        Ok(()) => ctx.toast(&tr("Dates reset to the files’ own dates")),
                        Err(error) => ctx.error(&tr("Could not reset the dates"), &error),
                    }
                });
            }
        }
    });
    dialog.present(Some(&window));
}
