//! A photo that fits the window and can be zoomed and panned with the
//! mouse, keyboard, touchpad or touchscreen.

use adw::prelude::AnimationExt;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Largest zoom, as a multiple of the image's own pixels.
pub const MAX_ZOOM: f64 = 16.0;

mod imp {
    use std::cell::{Cell, RefCell};
    use std::sync::OnceLock;

    use gtk::glib::subclass::Signal;
    use gtk::{gdk, graphene, gsk};

    use super::*;

    #[derive(Default)]
    pub struct ZoomImage {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        /// Size of the original image in pixels, before rotation.
        pub(super) image_size: Cell<(f64, f64)>,
        pub(super) quarter_turns: Cell<u8>,
        /// Logical pixels per image pixel.
        pub(super) scale: Cell<f64>,
        /// Follows the window size while true.
        pub(super) fit: Cell<bool>,
        /// Image point (in rotated image pixels) at the center of the view.
        pub(super) center: Cell<(f64, f64)>,
        drag_start: Cell<(f64, f64)>,
        pinch_start: Cell<f64>,
        pointer: Cell<Option<(f64, f64)>>,
        animation: RefCell<Option<adw::TimedAnimation>>,
    }

    impl std::fmt::Debug for ZoomImage {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ZoomImage").finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomImage {
        const NAME: &'static str = "MnZoomImage";
        type Type = super::ZoomImage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("zoom-image");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }

        fn new() -> Self {
            Self {
                fit: Cell::new(true),
                scale: Cell::new(1.0),
                ..Self::default()
            }
        }
    }

    impl ObjectImpl for ZoomImage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(false);
            self.setup_gestures();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("zoom-changed").build(),
                    // Swiped left (+1) or right (-1) while not zoomed in.
                    Signal::builder("swipe")
                        .param_types([i32::static_type()])
                        .build(),
                ]
            })
        }
    }

    impl WidgetImpl for ZoomImage {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 200, -1, -1)
        }

        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
            if self.fit.get() {
                self.scale.set(self.fit_scale());
                self.center_image();
                self.obj().emit_by_name::<()>("zoom-changed", &[]);
            } else {
                self.clamp_center();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(texture) = self.texture.borrow().clone() else {
                return;
            };
            let (iw, ih) = self.image_size.get();
            if iw <= 0.0 || ih <= 0.0 {
                return;
            }
            let obj = self.obj();
            let (width, height) = (f64::from(obj.width()), f64::from(obj.height()));
            let scale = self.scale.get();
            let (cx, cy) = self.center.get();
            let turns = self.quarter_turns.get() % 4;
            // Rotated image size in image pixels.
            let (rw, rh) = if turns % 2 == 1 { (ih, iw) } else { (iw, ih) };
            // Top-left of the rotated image in widget coordinates.
            let left = width / 2.0 - cx * scale;
            let top = height / 2.0 - cy * scale;
            snapshot.save();
            #[allow(clippy::cast_possible_truncation)]
            snapshot.translate(&graphene::Point::new(
                (left + rw * scale / 2.0) as f32,
                (top + rh * scale / 2.0) as f32,
            ));
            if turns != 0 {
                snapshot.rotate(90.0 * f32::from(turns));
            }
            #[allow(clippy::cast_possible_truncation)]
            let target = graphene::Rect::new(
                (-iw * scale / 2.0) as f32,
                (-ih * scale / 2.0) as f32,
                (iw * scale) as f32,
                (ih * scale) as f32,
            );
            let device_scale = f64::from(obj.scale_factor());
            #[allow(clippy::cast_precision_loss)]
            let texture_scale = scale * iw / texture.width().max(1) as f64 * device_scale;
            let filter = if texture_scale >= 3.0 {
                gsk::ScalingFilter::Nearest
            } else if texture_scale < 0.5 {
                gsk::ScalingFilter::Trilinear
            } else {
                gsk::ScalingFilter::Linear
            };
            snapshot.append_scaled_texture(&texture, filter, &target);
            snapshot.restore();
        }
    }

    impl ZoomImage {
        fn rotated_size(&self) -> (f64, f64) {
            let (iw, ih) = self.image_size.get();
            if self.quarter_turns.get() % 2 == 1 {
                (ih, iw)
            } else {
                (iw, ih)
            }
        }

        /// The scale at which the whole image fits, never enlarging small
        /// images beyond their pixels.
        pub(super) fn fit_scale(&self) -> f64 {
            let obj = self.obj();
            let (width, height) = (f64::from(obj.width()), f64::from(obj.height()));
            let (rw, rh) = self.rotated_size();
            if rw <= 0.0 || rh <= 0.0 || width <= 0.0 || height <= 0.0 {
                return 1.0;
            }
            let device = f64::from(obj.scale_factor());
            (width / rw).min(height / rh).min(1.0 / device)
        }

        pub(super) fn center_image(&self) {
            let (rw, rh) = self.rotated_size();
            self.center.set((rw / 2.0, rh / 2.0));
        }

        /// Keeps the image from being panned off screen.
        pub(super) fn clamp_center(&self) {
            let obj = self.obj();
            let (width, height) = (f64::from(obj.width()), f64::from(obj.height()));
            let (rw, rh) = self.rotated_size();
            let scale = self.scale.get();
            let clamp = |center: f64, image: f64, view: f64| {
                let half = view / 2.0 / scale;
                if image * scale <= view {
                    image / 2.0
                } else {
                    center.clamp(half, image - half)
                }
            };
            let (cx, cy) = self.center.get();
            self.center
                .set((clamp(cx, rw, width), clamp(cy, rh, height)));
        }

        /// Zooms to `scale`, keeping the image point under `anchor` (widget
        /// coordinates) in place.
        pub(super) fn zoom_to(&self, scale: f64, anchor: Option<(f64, f64)>) {
            let obj = self.obj();
            let fit = self.fit_scale();
            let device = f64::from(obj.scale_factor());
            let scale = scale.clamp(fit.min(1.0 / device), MAX_ZOOM / device);
            let (width, height) = (f64::from(obj.width()), f64::from(obj.height()));
            let (ax, ay) = anchor.unwrap_or((width / 2.0, height / 2.0));
            let old = self.scale.get();
            let (cx, cy) = self.center.get();
            // Image point under the anchor stays under the anchor.
            let px = cx + (ax - width / 2.0) / old;
            let py = cy + (ay - height / 2.0) / old;
            self.scale.set(scale);
            self.center.set((
                px - (ax - width / 2.0) / scale,
                py - (ay - height / 2.0) / scale,
            ));
            self.fit.set((scale - fit).abs() < 1e-6);
            if self.fit.get() {
                self.center_image();
            } else {
                self.clamp_center();
            }
            obj.set_cursor_from_name(if self.fit.get() { None } else { Some("grab") });
            obj.queue_draw();
            obj.emit_by_name::<()>("zoom-changed", &[]);
        }

        /// Zooms with an animation.
        pub(super) fn animate_zoom(&self, target: f64, anchor: Option<(f64, f64)>) {
            if let Some(animation) = self.animation.take() {
                animation.skip();
            }
            let obj = self.obj();
            let start = self.scale.get();
            let weak = obj.downgrade();
            let animation_target = adw::CallbackAnimationTarget::new(move |value| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().zoom_to(value, anchor);
                }
            });
            let animation = adw::TimedAnimation::builder()
                .widget(&*obj)
                .value_from(start)
                .value_to(target)
                .duration(200)
                .easing(adw::Easing::EaseOutCubic)
                .target(&animation_target)
                .build();
            animation.play();
            *self.animation.borrow_mut() = Some(animation);
        }

        fn setup_gestures(&self) {
            let obj = self.obj();

            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::BOTH_AXES
                    | gtk::EventControllerScrollFlags::KINETIC,
            );
            scroll.connect_scroll(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |controller, dx, dy| {
                    let imp = obj.imp();
                    let ctrl = controller
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK);
                    let wheel = controller.unit() == gdk::ScrollUnit::Wheel;
                    // A mouse wheel zooms; touchpads pan, and zoom with Ctrl or a pinch.
                    if ctrl || wheel {
                        let factor = if wheel {
                            1.25_f64.powf(-dy)
                        } else {
                            (1.0 - dy / 200.0).max(0.5)
                        };
                        imp.zoom_to(imp.scale.get() * factor, imp.pointer.get());
                        return glib::Propagation::Stop;
                    }
                    if imp.fit.get() {
                        return glib::Propagation::Proceed;
                    }
                    let scale = imp.scale.get();
                    let (cx, cy) = imp.center.get();
                    imp.center.set((cx + dx / scale, cy + dy / scale));
                    imp.clamp_center();
                    obj.queue_draw();
                    glib::Propagation::Stop
                }
            ));
            obj.add_controller(scroll);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak]
                obj,
                move |_, x, y| obj.imp().pointer.set(Some((x, y)))
            ));
            motion.connect_leave(glib::clone!(
                #[weak]
                obj,
                move |_| obj.imp().pointer.set(None)
            ));
            obj.add_controller(motion);

            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                obj,
                move |gesture, _, _| {
                    let imp = obj.imp();
                    if imp.fit.get() {
                        gesture.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    imp.drag_start.set(imp.center.get());
                    obj.set_cursor_from_name(Some("grabbing"));
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak]
                obj,
                move |gesture, dx, dy| {
                    let imp = obj.imp();
                    if dx.hypot(dy) > 3.0 {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                    let scale = imp.scale.get();
                    let (sx, sy) = imp.drag_start.get();
                    imp.center.set((sx - dx / scale, sy - dy / scale));
                    imp.clamp_center();
                    obj.queue_draw();
                }
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak]
                obj,
                move |_, _, _| {
                    let fit = obj.imp().fit.get();
                    obj.set_cursor_from_name(if fit { None } else { Some("grab") });
                }
            ));
            obj.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |_, presses, x, y| {
                    if presses == 2 {
                        let imp = obj.imp();
                        if imp.fit.get() {
                            let device = f64::from(obj.scale_factor());
                            let target = (1.0 / device).max(imp.fit_scale() * 2.5);
                            imp.animate_zoom(target, Some((x, y)));
                        } else {
                            imp.animate_zoom(imp.fit_scale(), Some((x, y)));
                        }
                    }
                }
            ));
            obj.add_controller(click);

            let pinch = gtk::GestureZoom::new();
            pinch.connect_begin(glib::clone!(
                #[weak]
                obj,
                move |_, _| obj.imp().pinch_start.set(obj.imp().scale.get())
            ));
            pinch.connect_scale_changed(glib::clone!(
                #[weak]
                obj,
                move |gesture, delta| {
                    let imp = obj.imp();
                    imp.zoom_to(imp.pinch_start.get() * delta, gesture.bounding_box_center());
                }
            ));
            obj.add_controller(pinch);

            let swipe = gtk::GestureSwipe::new();
            swipe.set_touch_only(true);
            swipe.connect_swipe(glib::clone!(
                #[weak]
                obj,
                move |_, vx, vy| {
                    if obj.imp().fit.get() && vx.abs() > 400.0 && vx.abs() > vy.abs() {
                        let direction: i32 = if vx < 0.0 { 1 } else { -1 };
                        obj.emit_by_name::<()>("swipe", &[&direction]);
                    }
                }
            ));
            obj.add_controller(swipe);
        }
    }
}

glib::wrapper! {
    pub struct ZoomImage(ObjectSubclass<imp::ZoomImage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ZoomImage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ZoomImage {
    /// Shows a new image, fitted to the window.
    pub fn set_image(
        &self,
        texture: Option<&gtk::gdk::Texture>,
        size: (u32, u32),
        quarter_turns: u8,
    ) {
        let imp = self.imp();
        imp.texture.replace(texture.cloned());
        imp.image_size
            .set((f64::from(size.0.max(1)), f64::from(size.1.max(1))));
        imp.quarter_turns.set(quarter_turns % 4);
        imp.fit.set(true);
        imp.scale.set(imp.fit_scale());
        imp.center_image();
        self.set_cursor_from_name(None);
        self.queue_draw();
        self.emit_by_name::<()>("zoom-changed", &[]);
    }

    /// Replaces the texture with a sharper one of the same image, keeping
    /// the zoom and position.
    pub fn upgrade_texture(&self, texture: &gtk::gdk::Texture) {
        self.imp().texture.replace(Some(texture.clone()));
        self.queue_draw();
    }

    pub fn set_quarter_turns(&self, quarter_turns: u8) {
        let imp = self.imp();
        imp.quarter_turns.set(quarter_turns % 4);
        imp.fit.set(true);
        imp.scale.set(imp.fit_scale());
        imp.center_image();
        self.queue_draw();
        self.emit_by_name::<()>("zoom-changed", &[]);
    }

    #[must_use]
    pub fn texture_width(&self) -> i32 {
        self.imp()
            .texture
            .borrow()
            .as_ref()
            .map_or(0, gtk::gdk::Texture::width)
    }

    /// Zoom relative to the original's pixels on screen: 1.0 is 100 %.
    #[must_use]
    pub fn zoom(&self) -> f64 {
        self.imp().scale.get() * f64::from(self.scale_factor())
    }

    #[must_use]
    pub fn is_fitted(&self) -> bool {
        self.imp().fit.get()
    }

    /// Whether the screen shows more detail than the current texture has.
    #[must_use]
    pub fn needs_more_pixels(&self) -> bool {
        let imp = self.imp();
        let (iw, _) = imp.image_size.get();
        let texture = f64::from(self.texture_width());
        texture > 0.0
            && texture < iw
            && imp.scale.get() * f64::from(self.scale_factor()) * iw > texture * 1.05
    }

    pub fn zoom_in(&self) {
        let imp = self.imp();
        imp.animate_zoom(imp.scale.get() * 1.5, None);
    }

    pub fn zoom_out(&self) {
        let imp = self.imp();
        imp.animate_zoom(imp.scale.get() / 1.5, None);
    }

    pub fn zoom_fit(&self) {
        let imp = self.imp();
        imp.animate_zoom(imp.fit_scale(), None);
    }

    pub fn zoom_original(&self) {
        let device = f64::from(self.scale_factor());
        self.imp().animate_zoom(1.0 / device, None);
    }

    pub fn connect_zoom_changed(
        &self,
        callback: impl Fn(&Self) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "zoom-changed",
            false,
            glib::closure_local!(move |image: &Self| callback(image)),
        )
    }

    pub fn connect_swipe(&self, callback: impl Fn(&Self, i32) + 'static) -> glib::SignalHandlerId {
        self.connect_closure(
            "swipe",
            false,
            glib::closure_local!(move |image: &Self, direction: i32| callback(image, direction)),
        )
    }
}
