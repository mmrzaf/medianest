//! The `gtk::MediaStream` over a [`Pipeline`].

use std::path::Path;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

use super::pipeline::{Event, Frame, Info, Pipeline};
use crate::i18n::tr;

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Debug, Default)]
    pub struct PlayerStream {
        pub(super) pipeline: RefCell<Option<Pipeline>>,
        texture: RefCell<Option<gdk::Texture>>,
        ended: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerStream {
        const NAME: &'static str = "MnPlayerStream";
        type Type = super::PlayerStream;
        type ParentType = gtk::MediaStream;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for PlayerStream {
        fn dispose(&self) {
            // Stops the pipeline and frees the file, threads and watches.
            self.pipeline.take();
        }
    }

    impl PaintableImpl for PlayerStream {
        fn intrinsic_width(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.height())
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            self.texture
                .borrow()
                .as_ref()
                .map_or(0.0, |t| f64::from(t.width()) / f64::from(t.height().max(1)))
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(texture) = self.texture.borrow().as_ref() {
                texture.snapshot(snapshot, width, height);
            }
        }
    }

    impl MediaStreamImpl for PlayerStream {
        fn play(&self) -> bool {
            let pipeline = self.pipeline.borrow();
            let Some(pipeline) = pipeline.as_ref() else {
                return false;
            };
            // Playing a finished video starts it again.
            if self.ended.replace(false) {
                pipeline.seek(0);
            }
            pipeline.play()
        }

        fn pause(&self) {
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                pipeline.pause();
            }
        }

        fn seek(&self, timestamp: i64) {
            self.ended.set(false);
            let started = self
                .pipeline
                .borrow()
                .as_ref()
                .is_some_and(|pipeline| pipeline.seek(timestamp));
            if !started {
                self.obj().seek_failed();
            }
        }

        // Frames are copied to memory, so there is no surface to attach to.
        fn realize(&self, _surface: gdk::Surface) {}

        fn unrealize(&self, _surface: gdk::Surface) {}

        fn update_audio(&self, muted: bool, volume: f64) {
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                pipeline.set_audio(muted, volume);
            }
        }
    }

    impl PlayerStream {
        pub(super) fn handle(&self, event: Event) {
            let stream = self.obj();
            match event {
                Event::Prepared(Info {
                    duration,
                    seekable,
                    has_audio,
                    has_video,
                }) => {
                    let duration = duration.map_or(0, |d| i64::try_from(d.useconds()).unwrap_or(0));
                    stream.stream_prepared(has_audio, has_video, seekable, duration);
                }
                Event::Frame(frame) => self.show(&frame),
                // Only seeks GTK asked for are acknowledged; restarting a
                // finished video or looping seeks on its own.
                Event::Seeked => {
                    if stream.is_seeking() {
                        stream.seek_success();
                    }
                }
                Event::Ended => {
                    // A seek to the very end finishes by ending, not by an
                    // acknowledgement, and GTK would wait for it forever.
                    if stream.is_seeking() {
                        stream.seek_success();
                    }
                    if stream.is_loop() {
                        if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                            pipeline.seek(0);
                        }
                    } else {
                        self.ended.set(true);
                        stream.stream_ended();
                    }
                }
                Event::Failed(message) => {
                    stream.set_error(glib::Error::new(gdk::GLError::NotAvailable, &message));
                }
            }
        }

        fn show(&self, frame: &Frame) {
            let texture = gdk::MemoryTexture::new(
                frame.width,
                frame.height,
                gdk::MemoryFormat::B8g8r8x8,
                &frame.bytes,
                frame.stride,
            );
            let resized = self
                .texture
                .borrow()
                .as_ref()
                .is_none_or(|old| (old.width(), old.height()) != (frame.width, frame.height));
            *self.texture.borrow_mut() = Some(texture.upcast());
            let stream = self.obj();
            if resized {
                stream.invalidate_size();
            }
            stream.invalidate_contents();
            if stream.is_prepared() {
                if let Some(position) = frame.position {
                    stream.update(i64::try_from(position.useconds()).unwrap_or(0));
                }
            }
        }
    }
}

glib::wrapper! {
    /// A video file as a `gtk::MediaStream`.
    pub struct PlayerStream(ObjectSubclass<imp::PlayerStream>)
        @extends gtk::MediaStream,
        @implements gdk::Paintable;
}

impl PlayerStream {
    /// Opens a video. A file that cannot be played leaves the stream in its
    /// error state, which `gtk::Video` shows.
    #[must_use]
    pub fn for_file(path: &Path) -> Self {
        let stream: Self = glib::Object::new();
        let weak = stream.downgrade();
        let handler: Rc<dyn Fn(Event)> = Rc::new(move |event| {
            if let Some(stream) = weak.upgrade() {
                stream.imp().handle(event);
            }
        });
        if crate::resources::low() {
            stream.set_error(glib::Error::new(
                gdk::GLError::NotAvailable,
                &tr("Too many files are open right now; videos cannot open"),
            ));
            return stream;
        }
        match Pipeline::open(path, true, handler) {
            Ok(pipeline) => *stream.imp().pipeline.borrow_mut() = Some(pipeline),
            Err(error) => stream.set_error(error),
        }
        stream
    }
}
