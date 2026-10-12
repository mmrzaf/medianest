use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use medianest_service::catalog::{TimelineItem, flags};
use medianest_service::core::AssetId;

use super::layout::{Cell as LayoutCell, Grouping, Layout, Params, Rect, RowKind};
use super::{Sections, ZOOM_SIZES};
use crate::filmstrips::{Filmstrips, Frames};
use crate::i18n::tr;
use crate::textures::ThumbnailLoader;
use crate::util;

type DragFiles = Box<dyn Fn(&[AssetId]) -> Vec<PathBuf>>;

/// Distance before a press on empty space becomes a selection band.
const BAND_THRESHOLD: f64 = 6.0;
const HEADER_HEIGHT: f32 = 52.0;
const CHECK_SIZE: f32 = 24.0;
/// How long the pointer rests on a video before its filmstrip is fetched.
const PREVIEW_DELAY_MS: u32 = 250;
/// Narrowest tile that shows a filmstrip.
const PREVIEW_MIN_TILE: f32 = 96.0;
const SCRUB_BAR_HEIGHT: f32 = 3.0;

#[derive(Clone, Copy, Debug)]
struct Band {
    /// Start in content coordinates.
    start: (f64, f64),
    /// Pointer in widget coordinates.
    pointer: (f64, f64),
    active: bool,
    /// Extend the existing selection instead of replacing it.
    additive: bool,
}

/// The frame of a video's filmstrip under the pointer.
struct Scrub {
    frames: Frames,
    frame: usize,
    /// Pointer position across the tile, from 0 to 1.
    fraction: f32,
}

#[derive(glib::Properties)]
#[properties(wrapper_type = super::PhotoGrid)]
pub struct PhotoGrid {
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_hadjustment, nullable)]
    hadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_vadjustment, nullable)]
    vadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    hscroll_policy: Cell<gtk::ScrollablePolicy>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    vscroll_policy: Cell<gtk::ScrollablePolicy>,

    pub(super) items: RefCell<Rc<Vec<TimelineItem>>>,
    layout: RefCell<Layout>,
    layout_valid: Cell<bool>,
    layout_width: Cell<f32>,
    pub(super) zoom: Cell<u32>,
    pub(super) square: Cell<bool>,
    pub(super) sections: Cell<Sections>,

    pub(super) selected: RefCell<HashSet<AssetId>>,
    pub(super) selection_mode: Cell<bool>,
    pub(super) cursor: Cell<Option<usize>>,
    anchor: Cell<Option<usize>>,
    hover: Cell<Option<usize>>,
    hover_header: Cell<Option<usize>>,
    band: Cell<Option<Band>>,
    band_tick: RefCell<Option<gtk::TickCallbackId>>,
    pressed: Cell<Option<usize>>,

    pub(super) loader: RefCell<Option<Rc<ThumbnailLoader>>>,
    filmstrips: RefCell<Option<Rc<Filmstrips>>>,
    /// Bumped whenever the hovered tile changes, to void a pending fetch.
    hover_generation: Cell<u64>,
    hover_frame: Cell<Option<usize>>,
    pub(super) drag_files: RefCell<Option<DragFiles>>,
    icons: RefCell<HashMap<&'static str, gtk::IconPaintable>>,
    adjustment_handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
    pending_scroll: Cell<Option<f64>>,
    pointer: Cell<Option<(f64, f64)>>,
    zoom_gesture_start: Cell<u32>,
}

impl Default for PhotoGrid {
    fn default() -> Self {
        Self {
            hadjustment: Default::default(),
            vadjustment: Default::default(),
            hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            items: Default::default(),
            layout: Default::default(),
            layout_valid: Default::default(),
            layout_width: Default::default(),
            zoom: Cell::new(2),
            square: Default::default(),
            sections: Cell::new(Sections::ByDate),
            selected: Default::default(),
            selection_mode: Default::default(),
            cursor: Default::default(),
            anchor: Default::default(),
            hover: Default::default(),
            hover_header: Default::default(),
            band: Default::default(),
            band_tick: Default::default(),
            pressed: Default::default(),
            loader: Default::default(),
            filmstrips: Default::default(),
            hover_generation: Default::default(),
            hover_frame: Default::default(),
            drag_files: Default::default(),
            icons: Default::default(),
            adjustment_handlers: Default::default(),
            pending_scroll: Default::default(),
            pointer: Default::default(),
            zoom_gesture_start: Default::default(),
        }
    }
}

impl std::fmt::Debug for PhotoGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoGrid").finish_non_exhaustive()
    }
}

#[glib::object_subclass]
impl ObjectSubclass for PhotoGrid {
    const NAME: &'static str = "MnPhotoGrid";
    type Type = super::PhotoGrid;
    type ParentType = gtk::Widget;
    type Interfaces = (gtk::Scrollable,);

    fn class_init(klass: &mut Self::Class) {
        klass.set_css_name("photo-grid");
        klass.set_accessible_role(gtk::AccessibleRole::Grid);
    }
}

#[glib::derived_properties]
impl ObjectImpl for PhotoGrid {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.set_focusable(true);
        obj.set_focus_on_click(true);
        obj.set_overflow(gtk::Overflow::Hidden);
        obj.set_has_tooltip(true);
        obj.add_css_class("photo-grid");
        obj.update_property(&[gtk::accessible::Property::Label(&tr("Photos"))]);
        self.setup_input();
    }

    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                Signal::builder("item-activated")
                    .param_types([u32::static_type()])
                    .build(),
                Signal::builder("selection-changed").build(),
                Signal::builder("context-menu")
                    .param_types([f64::static_type(), f64::static_type(), i64::static_type()])
                    .build(),
                Signal::builder("zoom-changed")
                    .param_types([u32::static_type()])
                    .build(),
                Signal::builder("scrolled").build(),
            ]
        })
    }

    fn dispose(&self) {
        for (adjustment, handler) in self.adjustment_handlers.take() {
            adjustment.disconnect(handler);
        }
        if let Some(tick) = self.band_tick.take() {
            tick.remove();
        }
    }
}

impl ScrollableImpl for PhotoGrid {}

impl WidgetImpl for PhotoGrid {
    fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        match orientation {
            gtk::Orientation::Horizontal => (160, 800, -1, -1),
            _ => (120, 600, -1, -1),
        }
    }

    fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
        #[allow(clippy::cast_precision_loss)]
        let width = width as f32;
        if !self.layout_valid.get() || (width - self.layout_width.get()).abs() > 0.5 {
            self.relayout(width, None);
        }
        self.configure_adjustments(f64::from(height));
        if let Some(value) = self.pending_scroll.take() {
            self.set_offset(value);
        }
        // Context menus are children of the grid and need presenting.
        let mut child = self.obj().first_child();
        while let Some(widget) = child {
            if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
                popover.present();
            }
            child = widget.next_sibling();
        }
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        self.draw(snapshot);
    }

    fn query_tooltip(&self, x: i32, y: i32, keyboard: bool, tooltip: &gtk::Tooltip) -> bool {
        let index = if keyboard {
            self.cursor.get()
        } else {
            self.item_at(f64::from(x), f64::from(y))
        };
        let Some(item) = index.and_then(|index| self.items.borrow().get(index).copied()) else {
            return false;
        };
        let mut lines = vec![
            item.local_ms
                .map_or_else(|| tr("Unknown date"), util::date_time),
        ];
        if item.has(flags::VIDEO) {
            lines.push(format!(
                "{} · {}",
                tr("Video"),
                util::duration(u64::from(item.duration_ms))
            ));
        }
        if item.has(flags::LIVE) {
            lines.push(tr("Live Photo"));
        }
        if item.has(flags::UNCERTAIN_DATE) {
            lines.push(tr("The date may be wrong"));
        }
        if item.has(flags::MISSING) {
            lines.push(tr("The file is missing"));
        } else if item.has(flags::CHANGED) {
            lines.push(tr("The file changed on disk"));
        }
        tooltip.set_text(Some(&lines.join("\n")));
        if let Some(rect) = index.map(|index| self.layout.borrow().rects[index]) {
            let offset = self.offset();
            #[allow(clippy::cast_possible_truncation)]
            tooltip.set_tip_area(&gdk::Rectangle::new(
                rect.x as i32,
                (f64::from(rect.y) - offset) as i32,
                rect.width as i32,
                rect.height as i32,
            ));
        }
        true
    }

    fn focus(&self, direction: gtk::DirectionType) -> bool {
        let obj = self.obj();
        if obj.has_focus() {
            return false;
        }
        if self.cursor.get().is_none() && !self.items.borrow().is_empty() {
            self.cursor.set(Some(self.first_visible().unwrap_or(0)));
        }
        let _ = direction;
        obj.grab_focus()
    }
}

// ----- state ------------------------------------------------------------------

impl PhotoGrid {
    pub(super) fn set_loader(&self, loader: Rc<ThumbnailLoader>) {
        let weak = self.obj().downgrade();
        loader.connect_loaded(move || {
            if let Some(grid) = weak.upgrade() {
                grid.queue_draw();
            }
        });
        *self.loader.borrow_mut() = Some(loader);
    }

    pub(super) fn set_filmstrips(&self, filmstrips: Rc<Filmstrips>) {
        let weak = self.obj().downgrade();
        filmstrips.connect_loaded(move || {
            weak.upgrade().is_some_and(|grid| {
                grid.queue_draw();
                true
            })
        });
        *self.filmstrips.borrow_mut() = Some(filmstrips);
    }

    /// The pointer moved, or the content moved under it.
    fn update_hover(&self, x: f64, y: f64) {
        let obj = self.obj();
        self.pointer.set(Some((x, y)));
        let hover = self.item_at(x, y);
        let header = if hover.is_none() {
            self.header_at(x, y)
        } else {
            None
        };
        let changed = hover != self.hover.get() || header != self.hover_header.get();
        if changed {
            self.hover.set(hover);
            self.hover_header.set(header);
            self.hover_changed(hover);
        }
        let frame = hover
            .and_then(|index| self.scrub(index))
            .map(|scrub| scrub.frame);
        if changed || frame != self.hover_frame.get() {
            self.hover_frame.set(frame);
            obj.queue_draw();
        }
        let pointer = hover.is_some() || header.is_some();
        obj.set_cursor_from_name(pointer.then_some("pointer"));
    }

    fn clear_hover(&self) {
        self.pointer.set(None);
        self.hover.set(None);
        self.hover_header.set(None);
        self.hover_frame.set(None);
        self.hover_changed(None);
        self.obj().queue_draw();
    }

    /// Starts fetching a video's filmstrip once the pointer has rested on it.
    fn hover_changed(&self, hover: Option<usize>) {
        let generation = self.hover_generation.get() + 1;
        self.hover_generation.set(generation);
        let Some(filmstrips) = self.filmstrips.borrow().clone() else {
            return;
        };
        filmstrips.cancel();
        // Dragging a selection band sweeps over many tiles without looking at any.
        if self.band.get().is_some() {
            return;
        }
        let Some(item) = hover.and_then(|index| self.items.borrow().get(index).copied()) else {
            return;
        };
        if !item.has(flags::VIDEO) || filmstrips.get(item.uuid).is_some() {
            return;
        }
        let weak = self.obj().downgrade();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(u64::from(PREVIEW_DELAY_MS)),
            move || {
                let Some(grid) = weak.upgrade() else { return };
                if grid.imp().hover_generation.get() == generation
                    && grid.imp().band.get().is_none()
                {
                    filmstrips.request(item.id, item.uuid);
                }
            },
        );
    }

    /// The filmstrip frame to show on a hovered video tile, if its strip is loaded.
    fn scrub(&self, index: usize) -> Option<Scrub> {
        if self.band.get().is_some() {
            return None;
        }
        let item = self.items.borrow().get(index).copied()?;
        if !item.has(flags::VIDEO) {
            return None;
        }
        let tile = *self.layout.borrow().rects.get(index)?;
        if tile.width < PREVIEW_MIN_TILE {
            return None;
        }
        let (x, _) = self.pointer.get()?;
        let frames = self.filmstrips.borrow().as_ref()?.get(item.uuid)?;
        let last = frames.len().checked_sub(1)?;
        #[allow(clippy::cast_possible_truncation)]
        let fraction = ((x as f32 - tile.x) / tile.width).clamp(0.0, 1.0);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let frame = ((fraction * frames.len() as f32) as usize).min(last);
        Some(Scrub {
            frames,
            frame,
            fraction,
        })
    }

    pub(super) fn set_items(&self, items: Rc<Vec<TimelineItem>>) {
        let anchor = self.scroll_anchor();
        let cursor_id = self
            .cursor
            .get()
            .and_then(|i| self.items.borrow().get(i).map(|item| item.id));
        {
            let present: HashSet<AssetId> = items.iter().map(|item| item.id).collect();
            self.selected.borrow_mut().retain(|id| present.contains(id));
        }
        let cursor = cursor_id.and_then(|id| items.iter().position(|item| item.id == id));
        *self.items.borrow_mut() = items;
        self.cursor.set(cursor);
        self.anchor.set(None);
        self.hover.set(None);
        self.hover_frame.set(None);
        self.hover_changed(None);
        self.layout_valid.set(false);
        let width = self.layout_width.get();
        if width > 0.0 {
            self.relayout(width, anchor);
            self.configure_adjustments(f64::from(self.obj().height()));
        }
        self.obj().queue_allocate();
        self.obj().queue_draw();
        self.selection_changed();
    }

    pub(super) fn invalidate_layout(&self) {
        let anchor = self.scroll_anchor();
        let width = self.layout_width.get();
        if width > 0.0 {
            self.relayout(width, anchor);
            self.configure_adjustments(f64::from(self.obj().height()));
        } else {
            self.layout_valid.set(false);
        }
        self.obj().queue_allocate();
        self.obj().queue_draw();
    }

    pub(super) fn set_zoom(&self, zoom: u32, pointer: Option<(f64, f64)>) {
        let zoom = zoom.min(ZOOM_SIZES.len() as u32 - 1);
        if zoom == self.zoom.get() {
            return;
        }
        let anchor = match pointer.and_then(|(x, y)| self.item_at(x, y).map(|index| (index, y))) {
            Some((index, y)) => {
                let rect = self.layout.borrow().rects[index];
                let id = self.items.borrow()[index].id;
                Some((
                    id,
                    f64::from(rect.y) - (self.offset() + y - f64::from(rect.height) / 2.0),
                ))
            }
            None => self.scroll_anchor(),
        };
        self.zoom.set(zoom);
        let width = self.layout_width.get();
        if width > 0.0 {
            self.relayout(width, anchor);
            self.configure_adjustments(f64::from(self.obj().height()));
        }
        self.obj().queue_draw();
        self.obj().emit_by_name::<()>("zoom-changed", &[&zoom]);
    }

    pub(super) fn set_selection_mode(&self, enabled: bool) {
        if self.selection_mode.replace(enabled) != enabled {
            if !enabled {
                self.selected.borrow_mut().clear();
            }
            self.selection_changed();
        }
    }

    pub(super) fn selection_changed(&self) {
        self.obj().queue_draw();
        self.obj().emit_by_name::<()>("selection-changed", &[]);
    }

    fn grouping(&self) -> Grouping {
        match self.sections.get() {
            Sections::None => Grouping::None,
            Sections::ByDate if self.zoom.get() == 0 => Grouping::Month,
            Sections::ByDate => Grouping::Day,
        }
    }

    fn params(&self, width: f32) -> Params {
        let zoom = self.zoom.get() as usize;
        let small = zoom == 0;
        Params {
            width,
            target: ZOOM_SIZES[zoom.min(ZOOM_SIZES.len() - 1)],
            gap: if small { 2.0 } else { 4.0 },
            padding: if small { 6.0 } else { 18.0 },
            header_height: HEADER_HEIGHT,
            square: self.square.get(),
            grouping: self.grouping(),
        }
    }

    fn relayout(&self, width: f32, anchor: Option<(AssetId, f64)>) {
        let cells: Vec<LayoutCell> = self
            .items
            .borrow()
            .iter()
            .map(|item| LayoutCell {
                aspect: item.aspect,
                local_ms: item.local_ms,
            })
            .collect();
        let layout = Layout::compute(&cells, &self.params(width));
        *self.layout.borrow_mut() = layout;
        self.layout_width.set(width);
        self.layout_valid.set(true);
        if let Some((id, delta)) = anchor {
            let index = self.items.borrow().iter().position(|item| item.id == id);
            if let Some(index) = index {
                let y = f64::from(self.layout.borrow().rects[index].y);
                self.pending_scroll.set(Some(y - delta));
            }
        }
    }

    /// The first visible item and its distance below the top of the view.
    fn scroll_anchor(&self) -> Option<(AssetId, f64)> {
        let index = self.first_visible()?;
        let rect = self.layout.borrow().rects.get(index).copied()?;
        let id = self.items.borrow().get(index)?.id;
        Some((id, f64::from(rect.y) - self.offset()))
    }

    fn first_visible(&self) -> Option<usize> {
        let offset = self.offset();
        let layout = self.layout.borrow();
        #[allow(clippy::cast_possible_truncation)]
        let rows = layout.rows_between(offset as f32, offset as f32 + 1.0e6);
        layout.rows[rows].iter().find_map(|row| match &row.kind {
            RowKind::Items(items) if f64::from(row.y + row.height) > offset => Some(items.start),
            _ => None,
        })
    }

    fn configure_adjustments(&self, height: f64) {
        let total = f64::from(self.layout.borrow().height);
        if let Some(vadj) = self.vadjustment.borrow().as_ref() {
            let value = vadj.value().clamp(0.0, (total - height).max(0.0));
            let step = f64::from(ZOOM_SIZES[self.zoom.get() as usize]) / 2.0;
            vadj.configure(value, 0.0, total.max(height), step, height * 0.9, height);
        }
        if let Some(hadj) = self.hadjustment.borrow().as_ref() {
            let width = f64::from(self.obj().width());
            hadj.configure(0.0, 0.0, width, 0.0, 0.0, width);
        }
    }

    pub(super) fn offset(&self) -> f64 {
        self.vadjustment
            .borrow()
            .as_ref()
            .map_or(0.0, gtk::Adjustment::value)
    }

    fn set_offset(&self, value: f64) {
        if let Some(vadj) = self.vadjustment.borrow().as_ref() {
            let max = (vadj.upper() - vadj.page_size()).max(0.0);
            vadj.set_value(value.clamp(0.0, max));
        }
    }

    fn set_hadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        self.hadjustment.replace(adjustment);
    }

    fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        let mut handlers = self.adjustment_handlers.borrow_mut();
        for (old, handler) in handlers.drain(..) {
            old.disconnect(handler);
        }
        if let Some(adjustment) = &adjustment {
            let weak = self.obj().downgrade();
            let handler = adjustment.connect_value_changed(move |_| {
                if let Some(grid) = weak.upgrade() {
                    if let Some((x, y)) = grid.imp().pointer.get() {
                        grid.imp().update_hover(x, y);
                    }
                    grid.queue_draw();
                    grid.emit_by_name::<()>("scrolled", &[]);
                }
            });
            handlers.push((adjustment.clone(), handler));
        }
        drop(handlers);
        self.vadjustment.replace(adjustment);
        self.configure_adjustments(f64::from(self.obj().height()));
    }

    pub(super) fn scroll_to_item(&self, index: usize) {
        let Some(rect) = self.layout.borrow().rects.get(index).copied() else {
            return;
        };
        let offset = self.offset();
        let height = f64::from(self.obj().height());
        let header_room = if self.grouping() == Grouping::None {
            8.0
        } else {
            f64::from(HEADER_HEIGHT)
        };
        let top = f64::from(rect.y) - header_room;
        let bottom = f64::from(rect.bottom()) + 8.0;
        if height <= 0.0 {
            self.pending_scroll.set(Some(top));
        } else if top < offset {
            self.set_offset(top);
        } else if bottom > offset + height {
            self.set_offset(bottom - height);
        }
    }

    /// Section starts as (content y, capture time of the first item), and
    /// the scrollable height.
    pub(super) fn marks(&self) -> (Vec<(f32, Option<i64>)>, f32) {
        let layout = self.layout.borrow();
        let items = self.items.borrow();
        let marks = layout
            .sections
            .iter()
            .map(|section| {
                let time = items
                    .get(section.items.start)
                    .and_then(|item| item.local_ms);
                (section.header.y, time)
            })
            .collect();
        (marks, layout.height)
    }

    pub(super) fn top_title(&self) -> Option<String> {
        let index = self.first_visible()?;
        let item = *self.items.borrow().get(index)?;
        Some(match item.local_ms {
            None => tr("No date"),
            Some(ms) if self.grouping() != Grouping::Month => util::month_title(ms),
            Some(ms) => util::year_title(ms),
        })
    }

    fn item_at(&self, x: f64, y: f64) -> Option<usize> {
        #[allow(clippy::cast_possible_truncation)]
        self.layout
            .borrow()
            .item_at(x as f32, (y + self.offset()) as f32)
    }

    fn header_at(&self, x: f64, y: f64) -> Option<usize> {
        #[allow(clippy::cast_possible_truncation)]
        self.layout
            .borrow()
            .header_at(x as f32, (y + self.offset()) as f32)
    }

    /// Whether a point is on an item's check mark.
    fn on_check(&self, index: usize, x: f64, y: f64) -> bool {
        let rect = self.layout.borrow().rects[index];
        let check = check_rect(&rect);
        let y = y + self.offset();
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = (x as f32, y as f32);
        x >= check.x - 6.0
            && x <= check.x + check.width + 6.0
            && y >= check.y - 6.0
            && y <= check.y + check.height + 6.0
    }

    fn section_selected(&self, section: usize) -> bool {
        let layout = self.layout.borrow();
        let items = self.items.borrow();
        let selected = self.selected.borrow();
        layout.sections.get(section).is_some_and(|section| {
            !selected.is_empty()
                && section
                    .items
                    .clone()
                    .all(|i| selected.contains(&items[i].id))
        })
    }

    fn toggle_section(&self, section: usize) {
        let all = self.section_selected(section);
        let range = self.layout.borrow().sections[section].items.clone();
        {
            let items = self.items.borrow();
            let mut selected = self.selected.borrow_mut();
            for index in range {
                if all {
                    selected.remove(&items[index].id);
                } else {
                    selected.insert(items[index].id);
                }
            }
        }
        self.selection_mode.set(true);
        self.selection_changed();
    }

    fn toggle(&self, index: usize) {
        let Some(id) = self.items.borrow().get(index).map(|item| item.id) else {
            return;
        };
        {
            let mut selected = self.selected.borrow_mut();
            if !selected.remove(&id) {
                selected.insert(id);
            }
        }
        self.anchor.set(Some(index));
        self.cursor.set(Some(index));
        self.selection_mode.set(true);
        self.selection_changed();
    }

    fn select_range(&self, from: usize, to: usize, replace: bool) {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        {
            let items = self.items.borrow();
            let mut selected = self.selected.borrow_mut();
            if replace {
                selected.clear();
            }
            for item in items.iter().take(end + 1).skip(start) {
                selected.insert(item.id);
            }
        }
        self.selection_mode.set(true);
        self.selection_changed();
    }

    fn activate(&self, index: usize) {
        #[allow(clippy::cast_possible_truncation)]
        self.obj()
            .emit_by_name::<()>("item-activated", &[&(index as u32)]);
    }

    fn announce_cursor(&self) {
        let Some(item) = self
            .cursor
            .get()
            .and_then(|i| self.items.borrow().get(i).copied())
        else {
            return;
        };
        let mut text = item
            .local_ms
            .map_or_else(|| tr("Unknown date"), util::date_time);
        if item.has(flags::VIDEO) {
            text = format!("{}, {text}", tr("Video"));
        }
        if self.selected.borrow().contains(&item.id) {
            text = format!("{text}, {}", tr("selected"));
        }
        self.obj()
            .announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
    }
}

// ----- input ------------------------------------------------------------------

impl PhotoGrid {
    fn setup_input(&self) {
        let obj = self.obj();

        let click = gtk::GestureClick::builder().button(0).build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |gesture, presses, x, y| grid.imp().on_pressed(gesture, presses, x, y)
        ));
        click.connect_released(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |gesture, _presses, x, y| grid.imp().on_released(gesture, x, y)
        ));
        obj.add_controller(click);

        let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
        long_press.connect_pressed(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |_, x, y| {
                let index = grid.imp().item_at(x, y);
                grid.imp().context_menu(x, y, index);
            }
        ));
        obj.add_controller(long_press);

        let band = gtk::GestureDrag::new();
        band.connect_drag_begin(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |gesture, x, y| grid.imp().band_begin(gesture, x, y)
        ));
        band.connect_drag_update(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |gesture, dx, dy| grid.imp().band_update(gesture, dx, dy)
        ));
        band.connect_drag_end(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |_, _, _| grid.imp().band_end()
        ));
        obj.add_controller(band);

        let drag = gtk::DragSource::builder()
            .actions(gdk::DragAction::COPY)
            .build();
        drag.connect_prepare(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            #[upgrade_or]
            None,
            move |source, x, y| grid.imp().drag_prepare(source, x, y)
        ));
        obj.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |_, x, y| grid.imp().update_hover(x, y)
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |_| grid.imp().clear_hover()
        ));
        obj.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _dx, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                let pointer = grid.imp().pointer.get();
                let zoom = grid.imp().zoom.get();
                let next = if dy < 0.0 {
                    zoom + 1
                } else {
                    zoom.saturating_sub(1)
                };
                grid.imp().set_zoom(next, pointer);
                glib::Propagation::Stop
            }
        ));
        obj.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        pinch.connect_begin(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |_, _| grid.imp().zoom_gesture_start.set(grid.imp().zoom.get())
        ));
        pinch.connect_scale_changed(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            move |gesture, scale| {
                let steps = (scale.ln() / 1.4_f64.ln()).round();
                #[allow(clippy::cast_possible_truncation)]
                let target = (i64::from(grid.imp().zoom_gesture_start.get()) + steps as i64)
                    .clamp(0, ZOOM_SIZES.len() as i64 - 1);
                let center = gesture.bounding_box_center();
                grid.imp()
                    .set_zoom(u32::try_from(target).unwrap_or(0), center);
            }
        ));
        obj.add_controller(pinch);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = grid)]
            obj,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| grid.imp().on_key(key, state)
        ));
        obj.add_controller(keys);
    }

    fn on_pressed(&self, gesture: &gtk::GestureClick, presses: i32, x: f64, y: f64) {
        let obj = self.obj();
        if !obj.has_focus() {
            obj.grab_focus();
        }
        let index = self.item_at(x, y);
        match gesture.current_button() {
            gdk::BUTTON_SECONDARY => {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                self.context_menu(x, y, index);
            }
            gdk::BUTTON_PRIMARY => {
                self.pressed.set(index);
                if presses > 1 {
                    // A double click opens even in selection mode.
                    if let Some(index) = index {
                        if !self.selection_mode.get() {
                            return;
                        }
                        self.activate(index);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_released(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        if gesture.current_button() != gdk::BUTTON_PRIMARY {
            return;
        }
        if self.band.get().is_some_and(|band| band.active) {
            return;
        }
        let state = gesture.current_event_state();
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let index = self.item_at(x, y);
        if index != self.pressed.take() {
            return;
        }
        let Some(index) = index else {
            if let Some(section) = self.header_at(x, y) {
                self.toggle_section(section);
            }
            return;
        };
        if shift {
            let anchor = self.anchor.get().or(self.cursor.get()).unwrap_or(index);
            self.select_range(anchor, index, !ctrl);
            self.cursor.set(Some(index));
        } else if ctrl || self.selection_mode.get() || self.on_check(index, x, y) {
            self.toggle(index);
        } else {
            self.cursor.set(Some(index));
            self.anchor.set(Some(index));
            self.activate(index);
        }
    }

    fn context_menu(&self, x: f64, y: f64, index: Option<usize>) {
        if let Some(index) = index {
            let id = self.items.borrow()[index].id;
            if !self.selected.borrow().contains(&id) {
                // Act on what was clicked, not on an unrelated selection.
                {
                    let mut selected = self.selected.borrow_mut();
                    selected.clear();
                    selected.insert(id);
                }
                self.selection_changed();
            }
            self.cursor.set(Some(index));
            self.anchor.set(Some(index));
        }
        let index = index.map_or(-1, |index| i64::try_from(index).unwrap_or(-1));
        self.obj()
            .emit_by_name::<()>("context-menu", &[&x, &y, &index]);
    }

    fn band_begin(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        if gesture.current_button() != gdk::BUTTON_PRIMARY || self.item_at(x, y).is_some() {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        let state = gesture.current_event_state();
        let additive =
            state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
        self.band.set(Some(Band {
            start: (x, y + self.offset()),
            pointer: (x, y),
            active: false,
            additive,
        }));
    }

    fn band_update(&self, gesture: &gtk::GestureDrag, dx: f64, dy: f64) {
        let Some(mut band) = self.band.get() else {
            return;
        };
        let Some((x, y)) = gesture.start_point() else {
            return;
        };
        band.pointer = (x + dx, y + dy);
        if !band.active && dx.hypot(dy) >= BAND_THRESHOLD {
            band.active = true;
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if !band.additive {
                self.selected.borrow_mut().clear();
            }
            self.start_band_scrolling();
        }
        self.band.set(Some(band));
        if band.active {
            self.apply_band(band);
        }
    }

    fn apply_band(&self, band: Band) {
        let offset = self.offset();
        let (x0, y0) = band.start;
        let (x1, y1) = (band.pointer.0, band.pointer.1 + offset);
        #[allow(clippy::cast_possible_truncation)]
        let rect = Rect {
            x: x0.min(x1) as f32,
            y: y0.min(y1) as f32,
            width: (x1 - x0).abs() as f32,
            height: (y1 - y0).abs() as f32,
        };
        let hits = self.layout.borrow().items_in(rect);
        {
            let items = self.items.borrow();
            let mut selected = self.selected.borrow_mut();
            if !band.additive {
                selected.clear();
            }
            for index in hits {
                selected.insert(items[index].id);
            }
        }
        if !self.selected.borrow().is_empty() {
            self.selection_mode.set(true);
        }
        self.selection_changed();
    }

    /// Scrolls while the band is dragged near the top or bottom edge.
    fn start_band_scrolling(&self) {
        if self.band_tick.borrow().is_some() {
            return;
        }
        let tick = self.obj().add_tick_callback(|grid, _clock| {
            let imp = grid.imp();
            let Some(band) = imp.band.get().filter(|band| band.active) else {
                imp.band_tick.take();
                return glib::ControlFlow::Break;
            };
            let height = f64::from(grid.height());
            let edge = 48.0;
            let y = band.pointer.1;
            let speed = if y < edge {
                -(edge - y).min(edge) / 2.0
            } else if y > height - edge {
                (y - (height - edge)).min(edge) / 2.0
            } else {
                0.0
            };
            if speed != 0.0 {
                imp.set_offset(imp.offset() + speed);
                imp.apply_band(band);
            }
            glib::ControlFlow::Continue
        });
        *self.band_tick.borrow_mut() = Some(tick);
    }

    fn band_end(&self) {
        let was_active = self.band.get().is_some_and(|band| band.active);
        if let Some(tick) = self.band_tick.take() {
            tick.remove();
        }
        if was_active {
            // Keep the flag until the click's release has been ignored.
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                if let Some(grid) = weak.upgrade() {
                    grid.imp().band.set(None);
                    grid.queue_draw();
                }
            });
        } else {
            self.band.set(None);
        }
        self.obj().queue_draw();
    }

    fn drag_prepare(
        &self,
        source: &gtk::DragSource,
        x: f64,
        y: f64,
    ) -> Option<gdk::ContentProvider> {
        let index = self.item_at(x, y)?;
        let id = self.items.borrow().get(index)?.id;
        let ids = if self.selected.borrow().contains(&id) {
            self.obj().selection()
        } else {
            vec![id]
        };
        let paths = self.drag_files.borrow().as_ref().map(|files| files(&ids))?;
        if paths.is_empty() {
            return None;
        }
        let files: Vec<gtk::gio::File> = paths.iter().map(gtk::gio::File::for_path).collect();
        if let Some(texture) = self.obj().texture(index) {
            let rect = self.layout.borrow().rects[index];
            let scale = 96.0 / rect.width.max(rect.height).max(1.0);
            #[allow(clippy::cast_possible_truncation)]
            let (hx, hy) = (
                ((x - f64::from(rect.x)) * f64::from(scale)) as i32,
                ((y + self.offset() - f64::from(rect.y)) * f64::from(scale)) as i32,
            );
            source.set_icon(Some(&texture), hx, hy);
        }
        Some(gdk::ContentProvider::for_value(
            &gdk::FileList::from_array(&files).to_value(),
        ))
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let count = self.items.borrow().len();
        if count == 0 {
            return glib::Propagation::Proceed;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let current = self.cursor.get();
        let page_rows = {
            let height = self.obj().height().max(1);
            let tile = ZOOM_SIZES[self.zoom.get() as usize];
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let rows = ((height as f32 / tile).floor() as usize).max(1);
            rows
        };
        let target = match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(current.map_or(0, |i| i.saturating_sub(1))),
            gdk::Key::Right | gdk::Key::KP_Right => {
                Some(current.map_or(0, |i| (i + 1).min(count - 1)))
            }
            gdk::Key::Up | gdk::Key::KP_Up => Some(current.map_or(0, |i| {
                self.layout
                    .borrow()
                    .vertical_neighbour(i, false)
                    .unwrap_or(0)
            })),
            gdk::Key::Down | gdk::Key::KP_Down => Some(current.map_or(0, |i| {
                self.layout
                    .borrow()
                    .vertical_neighbour(i, true)
                    .unwrap_or(count - 1)
            })),
            gdk::Key::Page_Up | gdk::Key::KP_Page_Up => Some(current.map_or(0, |mut i| {
                let layout = self.layout.borrow();
                for _ in 0..page_rows {
                    match layout.vertical_neighbour(i, false) {
                        Some(next) => i = next,
                        None => return 0,
                    }
                }
                i
            })),
            gdk::Key::Page_Down | gdk::Key::KP_Page_Down => Some(current.map_or(0, |mut i| {
                let layout = self.layout.borrow();
                for _ in 0..page_rows {
                    match layout.vertical_neighbour(i, true) {
                        Some(next) => i = next,
                        None => return count - 1,
                    }
                }
                i
            })),
            gdk::Key::Home | gdk::Key::KP_Home => Some(0),
            gdk::Key::End | gdk::Key::KP_End => Some(count - 1),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                if let Some(index) = current {
                    if self.selection_mode.get() && !ctrl {
                        self.toggle(index);
                    } else {
                        self.activate(index);
                    }
                }
                return glib::Propagation::Stop;
            }
            gdk::Key::space | gdk::Key::KP_Space => {
                if let Some(index) = current {
                    self.toggle(index);
                    self.announce_cursor();
                }
                return glib::Propagation::Stop;
            }
            gdk::Key::Escape if self.selection_mode.get() => {
                self.set_selection_mode(false);
                return glib::Propagation::Stop;
            }
            gdk::Key::Menu => {
                if let Some(index) = current {
                    let rect = self.layout.borrow().rects[index];
                    let y = f64::from(rect.y) - self.offset() + f64::from(rect.height) / 2.0;
                    self.context_menu(f64::from(rect.center_x()), y, Some(index));
                }
                return glib::Propagation::Stop;
            }
            _ => None,
        };
        let Some(target) = target else {
            return glib::Propagation::Proceed;
        };
        if shift {
            let anchor = self.anchor.get().or(current).unwrap_or(target);
            self.anchor.set(Some(anchor));
            self.select_range(anchor, target, !ctrl);
        } else if !ctrl {
            self.anchor.set(Some(target));
        }
        self.cursor.set(Some(target));
        self.scroll_to_item(target);
        self.announce_cursor();
        self.obj().queue_draw();
        glib::Propagation::Stop
    }
}

// ----- drawing ----------------------------------------------------------------

fn rect(r: &Rect, dy: f32) -> graphene::Rect {
    graphene::Rect::new(r.x, r.y - dy, r.width, r.height)
}

/// A thin bar along the bottom of a tile showing how far into the video the
/// pointer is.
fn draw_scrub_bar(snapshot: &gtk::Snapshot, bounds: &graphene::Rect, fraction: f32) {
    let y = bounds.y() + bounds.height() - SCRUB_BAR_HEIGHT;
    snapshot.append_color(
        &gdk::RGBA::new(1.0, 1.0, 1.0, 0.35),
        &graphene::Rect::new(bounds.x(), y, bounds.width(), SCRUB_BAR_HEIGHT),
    );
    snapshot.append_color(
        &gdk::RGBA::WHITE,
        &graphene::Rect::new(bounds.x(), y, bounds.width() * fraction, SCRUB_BAR_HEIGHT),
    );
}

fn check_rect(tile: &Rect) -> Rect {
    let size = if tile.width < 110.0 { 20.0 } else { CHECK_SIZE };
    Rect {
        x: tile.x + 8.0,
        y: tile.y + 8.0,
        width: size,
        height: size,
    }
}

fn rgba(color: u32, alpha: f32) -> gdk::RGBA {
    #[allow(clippy::cast_precision_loss)]
    let channel = |shift: u32| ((color >> shift) & 0xff) as f32 / 255.0;
    gdk::RGBA::new(channel(16), channel(8), channel(0), alpha)
}

fn circle(
    snapshot: &gtk::Snapshot,
    bounds: &graphene::Rect,
    fill: &gdk::RGBA,
    border: Option<&gdk::RGBA>,
) {
    let radius = bounds.width() / 2.0;
    let rounded = gsk::RoundedRect::from_rect(*bounds, radius);
    snapshot.push_rounded_clip(&rounded);
    snapshot.append_color(fill, bounds);
    snapshot.pop();
    if let Some(border) = border {
        snapshot.append_border(&rounded, &[2.0; 4], &[*border; 4]);
    }
}

impl PhotoGrid {
    fn icon(&self, name: &'static str) -> gtk::IconPaintable {
        if let Some(icon) = self.icons.borrow().get(name) {
            return icon.clone();
        }
        let obj = self.obj();
        let theme = gtk::IconTheme::for_display(&obj.display());
        let icon = theme.lookup_icon(
            name,
            &[],
            16,
            obj.scale_factor(),
            gtk::TextDirection::None,
            gtk::IconLookupFlags::empty(),
        );
        self.icons.borrow_mut().insert(name, icon.clone());
        icon
    }

    fn draw_icon(
        &self,
        snapshot: &gtk::Snapshot,
        name: &'static str,
        x: f32,
        y: f32,
        size: f32,
        color: &gdk::RGBA,
    ) {
        snapshot.save();
        snapshot.translate(&graphene::Point::new(x, y));
        self.icon(name)
            .snapshot_symbolic(snapshot, f64::from(size), f64::from(size), &[*color]);
        snapshot.restore();
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        let width = obj.width();
        let height = obj.height();
        if width <= 0 || height <= 0 {
            return;
        }
        let offset = self.offset();
        #[allow(clippy::cast_possible_truncation)]
        let top = offset as f32;
        #[allow(clippy::cast_precision_loss)]
        let bottom = top + height as f32;
        let layout = self.layout.borrow();
        let items = self.items.borrow();
        let selected = self.selected.borrow();
        let foreground = obj.color();
        let accent = adw::StyleManager::default().accent_color_rgba();
        let dark = adw::StyleManager::default().is_dark();
        let placeholder = gdk::RGBA::new(
            foreground.red(),
            foreground.green(),
            foreground.blue(),
            if dark { 0.10 } else { 0.08 },
        );
        let focus_visible = obj.has_visible_focus();
        let loader = self.loader.borrow().clone();
        let selection_mode = self.selection_mode.get();
        let radius = if self.zoom.get() == 0 { 3.0 } else { 6.0 };

        let rows = layout.rows_between(top, bottom);
        let mut visible = Vec::new();
        for row in &layout.rows[rows.clone()] {
            match &row.kind {
                RowKind::Header(section) => {
                    self.draw_header(
                        snapshot,
                        &layout,
                        *section,
                        &items,
                        top,
                        &foreground,
                        &accent,
                    );
                }
                RowKind::Items(range) => {
                    for index in range.clone() {
                        let item = &items[index];
                        visible.push((item.id, item.uuid));
                        let tile = layout.rects[index];
                        let is_selected = selected.contains(&item.id);
                        let mut bounds = rect(&tile, top);
                        if is_selected {
                            let inset = (tile.width.min(tile.height) * 0.06).min(12.0);
                            bounds = bounds.inset_r(inset, inset);
                        }
                        let rounded = gsk::RoundedRect::from_rect(bounds, radius);
                        snapshot.push_rounded_clip(&rounded);
                        let base = if item.color == 0 {
                            placeholder
                        } else {
                            rgba(item.color, 1.0)
                        };
                        snapshot.append_color(&base, &bounds);
                        let scrub = if self.hover.get() == Some(index) {
                            self.scrub(index)
                        } else {
                            None
                        };
                        let texture = match &scrub {
                            Some(scrub) => Some(scrub.frames[scrub.frame].clone()),
                            None => loader.as_ref().and_then(|loader| loader.get(item.uuid)),
                        };
                        match texture {
                            Some(texture) => {
                                draw_texture(snapshot, &texture, &bounds, item.quarter_turns)
                            }
                            None => {
                                let failed = item.has(flags::THUMB_FAILED)
                                    || loader.as_ref().is_some_and(|l| l.is_failed(item.uuid));
                                if failed {
                                    let size = 24.0_f32.min(bounds.width() / 3.0);
                                    let color = gdk::RGBA::new(
                                        foreground.red(),
                                        foreground.green(),
                                        foreground.blue(),
                                        0.5,
                                    );
                                    let icon = if item.has(flags::VIDEO) {
                                        "video-x-generic-symbolic"
                                    } else {
                                        "image-missing-symbolic"
                                    };
                                    self.draw_icon(
                                        snapshot,
                                        icon,
                                        bounds.x() + (bounds.width() - size) / 2.0,
                                        bounds.y() + (bounds.height() - size) / 2.0,
                                        size,
                                        &color,
                                    );
                                }
                            }
                        }
                        self.draw_badges(snapshot, item, &bounds);
                        if let Some(scrub) = &scrub {
                            draw_scrub_bar(snapshot, &bounds, scrub.fraction);
                        }
                        snapshot.pop();
                        if is_selected {
                            snapshot.append_border(&rounded, &[3.0; 4], &[accent; 4]);
                        }
                        if self.band.get().is_none()
                            && (selection_mode || is_selected || self.hover.get() == Some(index))
                        {
                            self.draw_check(snapshot, &tile, top, is_selected, &accent);
                        }
                        if focus_visible && self.cursor.get() == Some(index) {
                            let ring = gsk::RoundedRect::from_rect(
                                rect(&tile, top).inset_r(-3.0, -3.0),
                                radius + 3.0,
                            );
                            snapshot.append_border(&ring, &[2.0; 4], &[accent; 4]);
                        }
                    }
                }
            }
        }

        if let Some(band) = self.band.get().filter(|band| band.active) {
            #[allow(clippy::cast_possible_truncation)]
            let (x0, y0) = (band.start.0 as f32, (band.start.1 - offset) as f32);
            #[allow(clippy::cast_possible_truncation)]
            let (x1, y1) = (band.pointer.0 as f32, band.pointer.1 as f32);
            let area =
                graphene::Rect::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs());
            let fill = gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.18);
            snapshot.append_color(&fill, &area);
            let edge = gsk::RoundedRect::from_rect(area, 0.0);
            snapshot.append_border(&edge, &[1.0; 4], &[accent; 4]);
        }
        drop((layout, items, selected));

        // Load what is on screen first, then a screen ahead and behind.
        if let Some(loader) = loader {
            let layout = self.layout.borrow();
            let items = self.items.borrow();
            #[allow(clippy::cast_precision_loss)]
            let screen = height as f32;
            for range in [
                layout.rows_between(bottom, bottom + screen),
                layout.rows_between(top - screen, top),
            ] {
                for row in &layout.rows[range] {
                    if let RowKind::Items(range) = &row.kind {
                        visible.extend(range.clone().map(|i| (items[i].id, items[i].uuid)));
                    }
                }
            }
            loader.want(&visible);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_header(
        &self,
        snapshot: &gtk::Snapshot,
        layout: &Layout,
        section: usize,
        items: &[TimelineItem],
        top: f32,
        foreground: &gdk::RGBA,
        accent: &gdk::RGBA,
    ) {
        let Some(info) = layout.sections.get(section) else {
            return;
        };
        let first = items.get(info.items.start).and_then(|item| item.local_ms);
        let title = match (first, self.grouping()) {
            (None, _) => tr("No date"),
            (Some(ms), Grouping::Month) => util::month_title(ms),
            (Some(ms), _) => util::day_title(ms),
        };
        let header = rect(&info.header, top);
        let obj = self.obj();
        let text = obj.create_pango_layout(Some(&title));
        let mut font = obj.pango_context().font_description().unwrap_or_default();
        font.set_weight(pango::Weight::Bold);
        font.set_size(font.size() * 6 / 5);
        text.set_font_description(Some(&font));
        let (_, logical) = text.pixel_extents();
        #[allow(clippy::cast_precision_loss)]
        let text_y = header.y() + (header.height() - logical.height() as f32) / 2.0 + 4.0;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(header.x() + 2.0, text_y));
        snapshot.append_layout(&text, foreground);
        snapshot.restore();

        let count = info.items.len() as u64;
        let detail = obj.create_pango_layout(Some(&util::items(count)));
        let dim = gdk::RGBA::new(
            foreground.red(),
            foreground.green(),
            foreground.blue(),
            0.55,
        );
        let (_, detail_rect) = detail.pixel_extents();
        #[allow(clippy::cast_precision_loss)]
        let detail_x = header.x() + 2.0 + logical.width() as f32 + 12.0;
        #[allow(clippy::cast_precision_loss)]
        let detail_y = header.y() + (header.height() - detail_rect.height() as f32) / 2.0 + 5.0;
        snapshot.save();
        snapshot.translate(&graphene::Point::new(detail_x, detail_y));
        snapshot.append_layout(&detail, &dim);
        snapshot.restore();

        let show_check = self.selection_mode.get() || self.hover_header.get() == Some(section);
        if show_check {
            let all = self.section_selected(section);
            let size = 22.0;
            let bounds = graphene::Rect::new(
                header.x() + header.width() - size - 4.0,
                header.y() + (header.height() - size) / 2.0 + 4.0,
                size,
                size,
            );
            if all {
                circle(snapshot, &bounds, accent, None);
                self.draw_icon(
                    snapshot,
                    "object-select-symbolic",
                    bounds.x() + 3.0,
                    bounds.y() + 3.0,
                    16.0,
                    &gdk::RGBA::WHITE,
                );
            } else {
                let fill = gdk::RGBA::new(
                    foreground.red(),
                    foreground.green(),
                    foreground.blue(),
                    0.06,
                );
                circle(snapshot, &bounds, &fill, Some(&dim));
            }
        }
    }

    fn draw_check(
        &self,
        snapshot: &gtk::Snapshot,
        tile: &Rect,
        top: f32,
        selected: bool,
        accent: &gdk::RGBA,
    ) {
        let check = check_rect(tile);
        let bounds = rect(&check, top);
        if selected {
            circle(snapshot, &bounds, accent, Some(&gdk::RGBA::WHITE));
            let icon = bounds.width() - 8.0;
            self.draw_icon(
                snapshot,
                "object-select-symbolic",
                bounds.x() + 4.0,
                bounds.y() + 4.0,
                icon,
                &gdk::RGBA::WHITE,
            );
        } else {
            circle(
                snapshot,
                &bounds,
                &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25),
                Some(&gdk::RGBA::new(1.0, 1.0, 1.0, 0.9)),
            );
        }
    }

    fn draw_badges(&self, snapshot: &gtk::Snapshot, item: &TimelineItem, bounds: &graphene::Rect) {
        if bounds.width() < 64.0 {
            return;
        }
        let white = gdk::RGBA::WHITE;
        let video = item.has(flags::VIDEO);
        let bottom_badges = video || item.has(flags::FAVORITE);
        if bottom_badges {
            let shade = graphene::Rect::new(
                bounds.x(),
                bounds.y() + bounds.height() - 36.0,
                bounds.width(),
                36.0,
            );
            snapshot.append_linear_gradient(
                &shade,
                &graphene::Point::new(0.0, shade.y()),
                &graphene::Point::new(0.0, shade.y() + shade.height()),
                &[
                    gsk::ColorStop::new(0.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.0)),
                    gsk::ColorStop::new(1.0, gdk::RGBA::new(0.0, 0.0, 0.0, 0.55)),
                ],
            );
        }
        let base = bounds.y() + bounds.height() - 22.0;
        if item.has(flags::FAVORITE) {
            self.draw_icon(
                snapshot,
                "mn-heart-filled-symbolic",
                bounds.x() + 8.0,
                base,
                14.0,
                &white,
            );
        }
        if video {
            let text = self
                .obj()
                .create_pango_layout(Some(&util::duration(u64::from(item.duration_ms))));
            let mut font = self
                .obj()
                .pango_context()
                .font_description()
                .unwrap_or_default();
            font.set_weight(pango::Weight::Bold);
            font.set_size(font.size() * 9 / 10);
            text.set_font_description(Some(&font));
            let attributes = pango::AttrList::new();
            attributes.insert(pango::AttrFontFeatures::new("tnum"));
            text.set_attributes(Some(&attributes));
            let (_, logical) = text.pixel_extents();
            #[allow(clippy::cast_precision_loss)]
            let x = bounds.x() + bounds.width() - logical.width() as f32 - 8.0;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x, base - 1.0));
            snapshot.append_layout(&text, &white);
            snapshot.restore();
        }
        let mut corner_x = bounds.x() + bounds.width() - 22.0;
        let corner_y = bounds.y() + 7.0;
        if item.has(flags::MISSING) || item.has(flags::CHANGED) {
            let back = graphene::Rect::new(corner_x - 3.0, corner_y - 3.0, 20.0, 20.0);
            circle(
                snapshot,
                &back,
                &gdk::RGBA::new(0.75, 0.11, 0.16, 0.9),
                None,
            );
            self.draw_icon(
                snapshot,
                "dialog-warning-symbolic",
                corner_x,
                corner_y,
                14.0,
                &white,
            );
            corner_x -= 24.0;
        }
        if item.has(flags::LIVE) {
            self.draw_icon(
                snapshot,
                "mn-live-symbolic",
                corner_x,
                corner_y,
                14.0,
                &white,
            );
        }
    }
}

/// Draws a texture to cover `bounds`, cropping the excess and applying the
/// user's rotation.
pub(crate) fn draw_texture(
    snapshot: &gtk::Snapshot,
    texture: &gdk::Texture,
    bounds: &graphene::Rect,
    quarter_turns: u8,
) {
    #[allow(clippy::cast_precision_loss)]
    let (tw, th) = (texture.width() as f32, texture.height() as f32);
    let turned = quarter_turns % 2 == 1;
    // Size of the target in the texture's own orientation.
    let (bw, bh) = if turned {
        (bounds.height(), bounds.width())
    } else {
        (bounds.width(), bounds.height())
    };
    let scale = (bw / tw).max(bh / th);
    let (dw, dh) = (tw * scale, th * scale);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(
        bounds.x() + bounds.width() / 2.0,
        bounds.y() + bounds.height() / 2.0,
    ));
    if quarter_turns % 4 != 0 {
        snapshot.rotate(90.0 * f32::from(quarter_turns % 4));
    }
    let target = graphene::Rect::new(-dw / 2.0, -dh / 2.0, dw, dh);
    let filter = if scale < 0.5 {
        gsk::ScalingFilter::Trilinear
    } else {
        gsk::ScalingFilter::Linear
    };
    snapshot.append_scaled_texture(texture, filter, &target);
    snapshot.restore();
}
