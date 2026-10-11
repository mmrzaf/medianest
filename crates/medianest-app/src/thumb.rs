//! A fixed-size, rounded thumbnail that crops its image to fill the square.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::{gdk, graphene, gsk};

    use super::*;

    #[derive(Debug, Default)]
    pub struct Thumb {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        pub(super) size: Cell<i32>,
        pub(super) radius: Cell<f32>,
        pub(super) quarter_turns: Cell<u8>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Thumb {
        const NAME: &'static str = "MnThumb";
        type Type = super::Thumb;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("thumb");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Thumb {}

    impl WidgetImpl for Thumb {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let size = self.size.get();
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            #[allow(clippy::cast_precision_loss)]
            let bounds = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            let rounded = gsk::RoundedRect::from_rect(bounds, self.radius.get());
            snapshot.push_rounded_clip(&rounded);
            match self.texture.borrow().as_ref() {
                Some(texture) => {
                    crate::grid::draw_cover(snapshot, texture, &bounds, self.quarter_turns.get())
                }
                None => {
                    let color = obj.color();
                    snapshot.append_color(
                        &gdk::RGBA::new(color.red(), color.green(), color.blue(), 0.08),
                        &bounds,
                    );
                }
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Thumb(ObjectSubclass<imp::Thumb>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Thumb {
    #[must_use]
    pub fn new(size: i32, radius: f32) -> Self {
        let thumb: Self = glib::Object::new();
        thumb.imp().size.set(size);
        thumb.imp().radius.set(radius);
        thumb.set_overflow(gtk::Overflow::Hidden);
        thumb
    }

    pub fn set_texture(&self, texture: Option<&gtk::gdk::Texture>) {
        self.imp().texture.replace(texture.cloned());
        self.queue_draw();
    }

    #[must_use]
    pub fn has_texture(&self) -> bool {
        self.imp().texture.borrow().is_some()
    }

    pub fn set_quarter_turns(&self, turns: u8) {
        self.imp().quarter_turns.set(turns % 4);
        self.queue_draw();
    }
}
