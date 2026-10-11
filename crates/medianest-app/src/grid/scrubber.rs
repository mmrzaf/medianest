//! A date scrubber along the right edge of the grid: years at a glance,
//! the month under the pointer, and dragging to jump through time.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::PhotoGrid;

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::{gdk, glib, graphene, gsk, pango};

    use super::*;
    use crate::util;

    const WIDTH: i32 = 76;
    /// Seconds the scrubber stays visible after scrolling.
    const LINGER_MS: u64 = 1400;

    #[derive(Default)]
    pub struct Scrubber {
        pub(super) grid: RefCell<Option<PhotoGrid>>,
        pub(super) visible: Cell<bool>,
        hovering: Cell<bool>,
        dragging: Cell<bool>,
        pointer_y: Cell<Option<f64>>,
        hide_source: RefCell<Option<glib::SourceId>>,
    }

    impl std::fmt::Debug for Scrubber {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Scrubber").finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Scrubber {
        const NAME: &'static str = "MnScrubber";
        type Type = super::Scrubber;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("scrubber");
            klass.set_accessible_role(gtk::AccessibleRole::Scrollbar);
        }
    }

    impl ObjectImpl for Scrubber {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_halign(gtk::Align::End);
            obj.set_valign(gtk::Align::Fill);
            obj.set_can_target(true);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_enter(glib::clone!(
                #[weak]
                obj,
                move |_, _, y| {
                    obj.imp().hovering.set(true);
                    obj.imp().pointer_y.set(Some(y));
                    obj.imp().show();
                }
            ));
            motion.connect_motion(glib::clone!(
                #[weak]
                obj,
                move |_, _, y| {
                    obj.imp().pointer_y.set(Some(y));
                    obj.queue_draw();
                }
            ));
            motion.connect_leave(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.imp().hovering.set(false);
                    obj.imp().pointer_y.set(None);
                    obj.imp().schedule_hide();
                }
            ));
            obj.add_controller(motion);

            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                obj,
                move |_, _, y| {
                    obj.imp().dragging.set(true);
                    obj.imp().jump(y);
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak]
                obj,
                move |gesture, _, dy| {
                    if let Some((_, y)) = gesture.start_point() {
                        obj.imp().pointer_y.set(Some(y + dy));
                        obj.imp().jump(y + dy);
                    }
                }
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak]
                obj,
                move |_, _, _| {
                    obj.imp().dragging.set(false);
                    obj.imp().schedule_hide();
                }
            ));
            obj.add_controller(drag);
        }
    }

    impl WidgetImpl for Scrubber {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (WIDTH, WIDTH, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if self.visible.get() || self.hovering.get() || self.dragging.get() {
                self.draw(snapshot);
            }
        }
    }

    impl Scrubber {
        pub(super) fn show(&self) {
            self.visible.set(true);
            self.obj().queue_draw();
            self.schedule_hide();
        }

        fn schedule_hide(&self) {
            if let Some(source) = self.hide_source.take() {
                source.remove();
            }
            let weak = self.obj().downgrade();
            let source = glib::timeout_add_local_once(
                std::time::Duration::from_millis(LINGER_MS),
                move || {
                    if let Some(obj) = weak.upgrade() {
                        let imp = obj.imp();
                        imp.hide_source.take();
                        if !imp.hovering.get() && !imp.dragging.get() {
                            imp.visible.set(false);
                            obj.queue_draw();
                        }
                    }
                },
            );
            *self.hide_source.borrow_mut() = Some(source);
        }

        /// Fraction of the scrollable range for a pointer position.
        fn fraction(&self, y: f64) -> f64 {
            let height = f64::from(self.obj().height()).max(1.0);
            ((y - 12.0) / (height - 24.0).max(1.0)).clamp(0.0, 1.0)
        }

        fn jump(&self, y: f64) {
            let Some(grid) = self.grid.borrow().clone() else {
                return;
            };
            if let Some(adjustment) = grid.vadjustment() {
                let range = (adjustment.upper() - adjustment.page_size()).max(0.0);
                adjustment.set_value(self.fraction(y) * range);
            }
            self.obj().queue_draw();
        }

        fn draw(&self, snapshot: &gtk::Snapshot) {
            let Some(grid) = self.grid.borrow().clone() else {
                return;
            };
            let Some(adjustment) = grid.vadjustment() else {
                return;
            };
            let obj = self.obj();
            #[allow(clippy::cast_precision_loss)]
            let (width, height) = (obj.width() as f32, obj.height() as f32);
            let range = (adjustment.upper() - adjustment.page_size()).max(1.0);
            let (marks, _) = grid.marks();
            if marks.is_empty() {
                return;
            }
            let foreground = obj.color();
            let accent = adw::StyleManager::default().accent_color_rgba();
            let track = height - 24.0;
            #[allow(clippy::cast_possible_truncation)]
            let to_y = |content_y: f32| {
                12.0 + (f64::from(content_y) / range).clamp(0.0, 1.0) as f32 * track
            };

            // Years, spaced so labels never overlap.
            let mut last_year = None;
            let mut last_y = f32::MIN;
            for (content_y, time) in &marks {
                let Some(ms) = time else { continue };
                let year = util::year_title(*ms);
                if last_year.as_ref() == Some(&year) {
                    continue;
                }
                last_year = Some(year.clone());
                let y = to_y(*content_y);
                if y - last_y < 22.0 {
                    continue;
                }
                last_y = y;
                let layout = obj.create_pango_layout(Some(&year));
                let (_, extents) = layout.pixel_extents();
                #[allow(clippy::cast_precision_loss)]
                let x = width - extents.width() as f32 - 14.0;
                let dim =
                    gdk::RGBA::new(foreground.red(), foreground.green(), foreground.blue(), 0.7);
                let back = graphene::Rect::new(
                    x - 6.0,
                    y - 2.0,
                    extents.width() as f32 + 12.0,
                    extents.height() as f32 + 4.0,
                );
                let pill = gsk::RoundedRect::from_rect(back, back.height() / 2.0);
                snapshot.push_rounded_clip(&pill);
                snapshot.append_color(&scrim(&foreground, 0.25), &back);
                snapshot.pop();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, y));
                snapshot.append_layout(&layout, &dim);
                snapshot.restore();
            }

            // Current position.
            #[allow(clippy::cast_possible_truncation)]
            let current = 12.0 + (adjustment.value() / range).clamp(0.0, 1.0) as f32 * track;
            let thumb = graphene::Rect::new(width - 6.0, current - 16.0, 4.0, 32.0);
            let rounded = gsk::RoundedRect::from_rect(thumb, 2.0);
            snapshot.push_rounded_clip(&rounded);
            snapshot.append_color(&accent, &thumb);
            snapshot.pop();

            // The month under the pointer.
            if let Some(pointer) = self.pointer_y.get() {
                #[allow(clippy::cast_possible_truncation)]
                let target = (self.fraction(pointer) * range) as f32;
                let at = marks
                    .iter()
                    .take_while(|(content_y, _)| *content_y <= target + 1.0)
                    .last()
                    .or_else(|| marks.first());
                if let Some((_, Some(ms))) = at {
                    let text = util::month_title(*ms);
                    let layout = obj.create_pango_layout(Some(&text));
                    let mut font = obj.pango_context().font_description().unwrap_or_default();
                    font.set_weight(pango::Weight::Bold);
                    layout.set_font_description(Some(&font));
                    let (_, extents) = layout.pixel_extents();
                    #[allow(clippy::cast_precision_loss)]
                    let (tw, th) = (extents.width() as f32, extents.height() as f32);
                    #[allow(clippy::cast_possible_truncation)]
                    let y = (pointer as f32).clamp(th, height - th);
                    let bubble = graphene::Rect::new(
                        width - tw - 34.0,
                        y - th / 2.0 - 5.0,
                        tw + 20.0,
                        th + 10.0,
                    );
                    let pill = gsk::RoundedRect::from_rect(bubble, 8.0);
                    snapshot.push_rounded_clip(&pill);
                    snapshot.append_color(&accent, &bubble);
                    snapshot.pop();
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(bubble.x() + 10.0, bubble.y() + 5.0));
                    snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
                    snapshot.restore();
                }
            }
        }
    }

    /// A background that contrasts with the foreground color.
    fn scrim(foreground: &gdk::RGBA, alpha: f32) -> gdk::RGBA {
        let light = foreground.red() + foreground.green() + foreground.blue() > 1.5;
        if light {
            gdk::RGBA::new(0.0, 0.0, 0.0, alpha * 2.0)
        } else {
            gdk::RGBA::new(1.0, 1.0, 1.0, alpha * 3.0)
        }
    }
}

glib::wrapper! {
    pub struct Scrubber(ObjectSubclass<imp::Scrubber>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Scrubber {
    #[must_use]
    pub fn new(grid: &PhotoGrid) -> Self {
        let scrubber: Self = glib::Object::new();
        *scrubber.imp().grid.borrow_mut() = Some(grid.clone());
        let weak = scrubber.downgrade();
        grid.connect_scrolled(move |_| {
            if let Some(scrubber) = weak.upgrade() {
                scrubber.imp().show();
            }
        });
        scrubber
    }
}
