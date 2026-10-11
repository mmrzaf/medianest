//! The sections of a library and the pages they push.

pub mod activity;
pub mod albums;
pub mod folders;
pub mod health;
pub mod places;
pub mod review;
pub mod similar;
pub mod tags;
pub mod timeline;

use medianest_service::Event;

/// A section shown from the sidebar.
pub trait Section {
    fn widget(&self) -> gtk::Widget;

    /// The section became visible.
    fn shown(&self) {}

    /// Something changed in the library.
    fn event(&self, _event: &Event, _visible: bool) {}

    /// Start searching for `text`.
    fn search(&self, _text: &str) {}

    /// A window shortcut such as `select-all` or `zoom-in`.
    fn command(&self, _name: &str) {}
}
