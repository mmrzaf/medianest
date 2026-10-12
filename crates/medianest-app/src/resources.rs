//! Keeping the process away from its file descriptor limit.
//!
//! Running out of descriptors cannot be survived: GLib aborts the process
//! when it cannot create its internal pipes. So the application watches its
//! own count, stops opening videos before the limit is reached and offers to
//! quit cleanly. The library is journaled, so quitting is always safe.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use crate::context::Ctx;
use crate::i18n::tr;

/// Share of the limit at which the application stops taking on more files.
const LOW_SHARE: u64 = 80;
/// Share it must fall back under before it takes on files again.
const RECOVERED_SHARE: u64 = 60;
const CHECK_EVERY: Duration = Duration::from_secs(10);

thread_local! {
    static LOW: Cell<bool> = const { Cell::new(false) };
}

/// Whether descriptors are scarce enough that nothing new should be opened
/// for playback.
pub fn low() -> bool {
    LOW.with(Cell::get)
}

/// Checks the descriptor count regularly for as long as `ctx` lives.
pub fn watch(ctx: &Rc<Ctx>) {
    let weak = Rc::downgrade(ctx);
    glib::timeout_add_local(CHECK_EVERY, move || {
        let Some(ctx) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some((open, limit)) = usage() else {
            return glib::ControlFlow::Break;
        };
        if low() {
            if open.saturating_mul(100) < limit.saturating_mul(RECOVERED_SHARE) {
                LOW.with(|low| low.set(false));
            }
            return glib::ControlFlow::Continue;
        }
        if !is_low(open, limit) {
            return glib::ControlFlow::Continue;
        }
        LOW.with(|low| low.set(true));
        tracing::error!(open, limit, "open files are close to the limit");
        let application = ctx.window.application();
        ctx.confirm(
            &tr("MediaNest Is Running Out of File Handles"),
            &tr(
                "Too many files are open, and MediaNest could stop suddenly if it keeps going. \
                 Your library is safe. Videos will not open until this clears; restarting \
                 MediaNest always does.",
            ),
            &tr("_Quit MediaNest"),
            false,
            move || {
                if let Some(application) = application {
                    application.quit();
                }
            },
        );
        glib::ControlFlow::Continue
    });
}

/// Open descriptors and the soft limit on them.
fn usage() -> Option<(u64, u64)> {
    let open = u64::try_from(std::fs::read_dir("/proc/self/fd").ok()?.count()).ok()?;
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    Some((open, soft_limit(&limits)?))
}

/// The soft "Max open files" limit from the text of `/proc/self/limits`.
fn soft_limit(limits: &str) -> Option<u64> {
    let line = limits
        .lines()
        .find(|line| line.starts_with("Max open files"))?;
    // "Max open files            1024                 524288               files"
    match line.split_whitespace().nth(3)? {
        "unlimited" => Some(u64::MAX),
        soft => soft.parse().ok(),
    }
}

fn is_low(open: u64, limit: u64) -> bool {
    open.saturating_mul(100) >= limit.saturating_mul(LOW_SHARE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_soft_limit() {
        let text = "Limit                     Soft Limit           Hard Limit           Units\n\
                    Max open files            1024                 524288               files\n\
                    Max locked memory         8388608              8388608              bytes\n";
        assert_eq!(soft_limit(text), Some(1024));
        assert_eq!(
            soft_limit(
                "Max open files            unlimited            unlimited            files\n"
            ),
            Some(u64::MAX)
        );
        assert_eq!(
            soft_limit("Max cpu time  unlimited unlimited seconds\n"),
            None
        );
    }

    #[test]
    fn descriptors_are_low_at_four_fifths_of_the_limit() {
        assert!(!is_low(819, 1024));
        assert!(is_low(820, 1024));
        assert!(is_low(1024, 1024));
        assert!(!is_low(400_000, 524_288));
        assert!(is_low(430_000, 524_288));
        assert!(!is_low(5_000, u64::MAX));
    }

    #[test]
    fn this_process_can_read_its_own_usage() {
        let (open, limit) = usage().expect("usage");
        assert!(open > 0 && limit > open);
    }
}
