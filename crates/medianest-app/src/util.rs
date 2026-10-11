//! Formatting and small helpers shared by the interface.

use std::future::Future;

use gtk::glib;
use medianest_service::core::now_ms;

use crate::i18n::{tr, trf, trn};

/// A whole number with locale-neutral thin grouping: 12 345.
#[must_use]
pub fn number(value: u64) -> String {
    medianest_service::group_digits(value)
}

/// A size in bytes for people: 4.2 MB.
#[must_use]
pub fn bytes(value: u64) -> String {
    glib::format_size(value).to_string()
}

/// Local wall-clock milliseconds as a GLib date, without time zone shifts.
fn wall(local_ms: i64) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_utc(local_ms.div_euclid(1000)).ok()
}

fn format(local_ms: i64, pattern: &str) -> String {
    wall(local_ms)
        .and_then(|date| date.format(pattern).ok())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// Today's local wall-clock time in milliseconds.
#[must_use]
pub fn now_local_ms() -> i64 {
    glib::DateTime::now_local().map_or_else(
        |_| now_ms(),
        |now| (now.to_unix() + now.utc_offset().as_seconds()) * 1000,
    )
}

const DAY_MS: i64 = 86_400_000;

fn same_year(a: i64, b: i64) -> bool {
    match (wall(a), wall(b)) {
        (Some(a), Some(b)) => a.year() == b.year(),
        _ => false,
    }
}

/// Header of a day: "Today", "Yesterday", "Saturday, 14 July" or
/// "14 July 2019".
#[must_use]
pub fn day_title(local_ms: i64) -> String {
    let today = now_local_ms().div_euclid(DAY_MS);
    let day = local_ms.div_euclid(DAY_MS);
    match today - day {
        0 => tr("Today"),
        1 => tr("Yesterday"),
        2..=6 => format(local_ms, "%A"),
        _ if same_year(local_ms, now_local_ms()) => format(local_ms, &tr("%A, %-e %B")),
        _ => format(local_ms, &tr("%-e %B %Y")),
    }
    .trim()
    .to_owned()
}

/// "July 2023".
#[must_use]
pub fn month_title(local_ms: i64) -> String {
    format(local_ms, &tr("%B %Y"))
}

#[must_use]
pub fn year_title(local_ms: i64) -> String {
    format(local_ms, "%Y")
}

/// A full date and time: "Saturday, 14 July 2023 at 10:15".
#[must_use]
pub fn date_time(local_ms: i64) -> String {
    format(local_ms, &tr("%A, %-e %B %Y at %H:%M"))
        .trim()
        .to_owned()
}

/// A time of day: "10:15".
#[must_use]
pub fn format_time(local_ms: i64) -> String {
    format(local_ms, "%H:%M")
}

/// A short date: "14 Jul 2023".
#[must_use]
pub fn short_date(local_ms: i64) -> String {
    format(local_ms, &tr("%-e %b %Y")).trim().to_owned()
}

/// Playback length: "0:42" or "1:02:03".
#[must_use]
pub fn duration(ms: u64) -> String {
    let seconds = ms.div_ceil(1000);
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// "just now", "5 minutes ago", "3 days ago", or a date.
#[must_use]
pub fn ago(utc_ms: i64) -> String {
    let elapsed = (now_ms() - utc_ms).max(0) / 1000;
    let elapsed = u64::try_from(elapsed).unwrap_or(0);
    match elapsed {
        0..60 => tr("just now"),
        60..3600 => trn("{} minute ago", "{} minutes ago", elapsed / 60),
        3600..86_400 => trn("{} hour ago", "{} hours ago", elapsed / 3600),
        86_400..2_592_000 => trn("{} day ago", "{} days ago", elapsed / 86_400),
        _ => {
            let offset = glib::DateTime::now_local()
                .map(|now| now.utc_offset().as_seconds() * 1000)
                .unwrap_or(0);
            short_date(utc_ms + offset)
        }
    }
}

/// "3 photos and 2 videos", "1 item".
#[must_use]
pub fn photos_and_videos(photos: u64, videos: u64) -> String {
    match (photos, videos) {
        (0, 0) => tr("No items"),
        (p, 0) => trn("{} photo", "{} photos", p),
        (0, v) => trn("{} video", "{} videos", v),
        (p, v) => trf(
            "{} and {}",
            &[
                &trn("{} photo", "{} photos", p),
                &trn("{} video", "{} videos", v),
            ],
        ),
    }
}

#[must_use]
pub fn items(count: u64) -> String {
    trn("{} item", "{} items", count)
}

/// Runs a closure once things have been quiet for a while.
#[derive(Debug)]
pub struct Debounce {
    delay_ms: u64,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
}

impl Debounce {
    #[must_use]
    pub fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            generation: std::rc::Rc::default(),
        }
    }

    /// Schedules `run`, replacing anything scheduled before.
    pub fn call(&self, run: impl FnOnce() + 'static) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let current = std::rc::Rc::clone(&self.generation);
        glib::timeout_add_local_once(std::time::Duration::from_millis(self.delay_ms), move || {
            if current.get() == generation {
                run();
            }
        });
    }
}

/// Runs a future on the main loop.
pub fn spawn<F: Future<Output = ()> + 'static>(future: F) {
    glib::spawn_future_local(future);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(42_000), "0:42");
        assert_eq!(duration(41_200), "0:42");
        assert_eq!(duration(3_723_000), "1:02:03");
    }

    #[test]
    fn wall_clock_formatting_ignores_the_time_zone() {
        // 2023-07-14 10:15 as wall-clock milliseconds.
        let ms = 1_689_329_700_000;
        assert_eq!(format(ms, "%Y-%m-%d %H:%M"), "2023-07-14 10:15");
        assert_eq!(format(ms - 13 * 86_400_000, "%-e %B"), "1 July");
    }
}
