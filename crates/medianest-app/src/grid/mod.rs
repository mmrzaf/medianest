//! The photo grid: a scrollable, virtualised view of a timeline.
//!
//! The grid draws every tile itself with GTK snapshots instead of creating
//! a widget per photo, so it stays smooth with hundreds of thousands of
//! items. Layout lives in [`layout`] and is computed once per width.

mod imp;
pub mod layout;
mod scrubber;

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use medianest_service::catalog::TimelineItem;
use medianest_service::core::AssetId;

pub(crate) use imp::draw_texture as draw_cover;
pub use scrubber::Scrubber;

use crate::textures::ThumbnailLoader;

/// Tile sizes of the zoom levels, in logical pixels.
pub const ZOOM_SIZES: [f32; 5] = [88.0, 136.0, 192.0, 256.0, 328.0];

glib::wrapper! {
    pub struct PhotoGrid(ObjectSubclass<imp::PhotoGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Default for PhotoGrid {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// How the grid chooses its section headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sections {
    /// Days when zoomed in, months when zoomed out.
    ByDate,
    /// One continuous run without headers, for album order.
    None,
}

impl PhotoGrid {
    #[must_use]
    pub fn new(loader: Rc<ThumbnailLoader>) -> Self {
        let grid: Self = glib::Object::new();
        grid.imp().set_loader(loader);
        grid
    }

    /// Replaces the items. Selection and scroll position are kept where
    /// the same assets are still present.
    pub fn set_items(&self, items: Rc<Vec<TimelineItem>>) {
        self.imp().set_items(items);
    }

    #[must_use]
    pub fn items(&self) -> Rc<Vec<TimelineItem>> {
        self.imp().items.borrow().clone()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.imp().items.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn set_sections(&self, sections: Sections) {
        self.imp().sections.set(sections);
        self.imp().invalidate_layout();
    }

    #[must_use]
    pub fn zoom(&self) -> u32 {
        self.imp().zoom.get()
    }

    /// Sets the zoom level, keeping the item at `anchor_y` (widget
    /// coordinates) in place.
    pub fn set_zoom(&self, zoom: u32) {
        self.imp().set_zoom(zoom, None);
    }

    pub fn zoom_in(&self) {
        self.set_zoom((self.zoom() + 1).min(ZOOM_SIZES.len() as u32 - 1));
    }

    pub fn zoom_out(&self) {
        self.set_zoom(self.zoom().saturating_sub(1));
    }

    pub fn set_square(&self, square: bool) {
        self.imp().square.set(square);
        self.imp().invalidate_layout();
    }

    /// Selected assets, in display order.
    #[must_use]
    pub fn selection(&self) -> Vec<AssetId> {
        let imp = self.imp();
        let selected = imp.selected.borrow();
        if selected.is_empty() {
            return Vec::new();
        }
        imp.items
            .borrow()
            .iter()
            .filter(|item| selected.contains(&item.id))
            .map(|item| item.id)
            .collect()
    }

    #[must_use]
    pub fn selection_count(&self) -> usize {
        self.imp().selected.borrow().len()
    }

    #[must_use]
    pub fn is_selection_mode(&self) -> bool {
        self.imp().selection_mode.get()
    }

    /// Shows check marks on every tile; clicking toggles instead of opening.
    pub fn set_selection_mode(&self, enabled: bool) {
        self.imp().set_selection_mode(enabled);
    }

    pub fn select_all(&self) {
        let imp = self.imp();
        let all: HashSet<AssetId> = imp.items.borrow().iter().map(|item| item.id).collect();
        *imp.selected.borrow_mut() = all;
        imp.set_selection_mode(true);
        imp.selection_changed();
    }

    pub fn clear_selection(&self) {
        let imp = self.imp();
        imp.selected.borrow_mut().clear();
        imp.selection_changed();
    }

    pub fn select(&self, ids: &[AssetId]) {
        let imp = self.imp();
        imp.selected.borrow_mut().extend(ids.iter().copied());
        imp.set_selection_mode(true);
        imp.selection_changed();
    }

    /// Index of an asset in the grid.
    #[must_use]
    pub fn index_of(&self, id: AssetId) -> Option<usize> {
        self.imp()
            .items
            .borrow()
            .iter()
            .position(|item| item.id == id)
    }

    /// Scrolls so the item is visible and puts the keyboard cursor on it.
    pub fn reveal(&self, index: usize) {
        let imp = self.imp();
        imp.cursor.set(Some(index));
        imp.scroll_to_item(index);
        self.queue_draw();
    }

    /// The item under the keyboard cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.imp().cursor.get()
    }

    /// Title of the section at the top of the view, for the floating date.
    #[must_use]
    pub fn top_title(&self) -> Option<String> {
        self.imp().top_title()
    }

    /// Section starts in content coordinates with their dates, and the
    /// total content height. For the scrubber.
    #[must_use]
    pub fn marks(&self) -> (Vec<(f32, Option<i64>)>, f32) {
        self.imp().marks()
    }

    pub fn scroll_to_top(&self) {
        if let Some(adjustment) = self.vadjustment() {
            adjustment.set_value(0.0);
        }
    }

    /// Supplies the files to hand over when photos are dragged out.
    pub fn set_drag_files(&self, files: impl Fn(&[AssetId]) -> Vec<PathBuf> + 'static) {
        *self.imp().drag_files.borrow_mut() = Some(Box::new(files));
    }

    /// The grid's thumbnail texture for an asset, when loaded.
    #[must_use]
    pub fn texture(&self, index: usize) -> Option<gtk::gdk::Texture> {
        let item = *self.imp().items.borrow().get(index)?;
        self.imp().loader.borrow().as_ref()?.get(item.uuid)
    }

    pub fn connect_item_activated(
        &self,
        callback: impl Fn(&Self, usize) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "item-activated",
            false,
            glib::closure_local!(move |grid: &Self, index: u32| callback(grid, index as usize)),
        )
    }

    pub fn connect_selection_changed(
        &self,
        callback: impl Fn(&Self) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "selection-changed",
            false,
            glib::closure_local!(move |grid: &Self| callback(grid)),
        )
    }

    /// Right click or long press at widget coordinates, on an item or not.
    pub fn connect_context_menu(
        &self,
        callback: impl Fn(&Self, f64, f64, Option<usize>) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "context-menu",
            false,
            glib::closure_local!(move |grid: &Self, x: f64, y: f64, index: i64| {
                callback(grid, x, y, usize::try_from(index).ok());
            }),
        )
    }

    pub fn connect_zoom_changed(
        &self,
        callback: impl Fn(&Self, u32) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "zoom-changed",
            false,
            glib::closure_local!(move |grid: &Self, zoom: u32| callback(grid, zoom)),
        )
    }

    /// The scroll position moved.
    pub fn connect_scrolled(&self, callback: impl Fn(&Self) + 'static) -> glib::SignalHandlerId {
        self.connect_closure(
            "scrolled",
            false,
            glib::closure_local!(move |grid: &Self| callback(grid)),
        )
    }
}
