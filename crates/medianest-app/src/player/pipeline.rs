//! A GStreamer pipeline that decodes a video file into frames in memory.
//!
//! Dropping a [`Pipeline`] stops it and releases everything it holds: its
//! file, threads, bus watch and sink callbacks. GTK's own GStreamer media
//! backend does not do this reliably, and leaks descriptors and threads for
//! every file it opens, which eventually exhausts the process.

use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt;
use gtk::glib;

/// Longest side of a decoded frame. Larger videos are scaled down inside the
/// pipeline, so a 4K clip is not copied to the GPU at full size every frame.
const MAX_EDGE: u32 = 2560;

/// Something the pipeline reports, always on the main thread.
#[derive(Debug)]
pub enum Event {
    /// The file is open: its first frame is ready and it can be played.
    Prepared(Info),
    /// The newest decoded frame. Frames that were not shown in time are skipped.
    Frame(Frame),
    /// A seek finished.
    Seeked,
    /// Playback reached the end.
    Ended,
    Failed(String),
}

#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub duration: Option<gst::ClockTime>,
    pub seekable: bool,
    pub has_audio: bool,
    pub has_video: bool,
}

/// One decoded picture, as BGRx pixels.
#[derive(Debug)]
pub struct Frame {
    pub bytes: glib::Bytes,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub position: Option<gst::ClockTime>,
}

pub struct Pipeline {
    playbin: gst::Element,
    sink: gst_app::AppSink,
    _watch: gst::bus::BusWatchGuard,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline").finish_non_exhaustive()
    }
}

/// The video sink: scales and converts frames to square pixels in a format
/// GDK uploads without conversion, turns them upright, then hands them over.
///
/// With a GPU post-processor the decoder's frames stay on the GPU until they
/// are final. That matters: software decoding of a 4K 10-bit HEVC clip runs
/// at about half the speed it needs, the GPU path at several times it. The
/// GPU element must not do the rotation itself: given a rotated phone video
/// it strips the frames' timestamps, so the clip plays several times too fast
/// and its clock never moves.
fn sink_description() -> String {
    const LIMIT: &str = "capsfilter name=limit caps=video/x-raw,format=BGRx,pixel-aspect-ratio=1/1";
    const SINK: &str = "appsink name=frames sync=true max-buffers=1 enable-last-sample=false";
    if gst::ElementFactory::find("vapostproc").is_some() {
        format!("vapostproc ! {LIMIT} ! videoflip video-direction=auto ! {SINK}")
    } else {
        format!("videoconvert ! videoscale ! {LIMIT} ! videoflip video-direction=auto ! {SINK}")
    }
}

fn failure(message: impl std::fmt::Display) -> glib::Error {
    glib::Error::new(gst::CoreError::Failed, &message.to_string())
}

impl Pipeline {
    /// Opens a video, paused on its first frame. `audio` is off only for
    /// tests, which run without a sound card.
    pub fn open(path: &Path, audio: bool, handler: Rc<dyn Fn(Event)>) -> Result<Self, glib::Error> {
        // Initialising is cheap after the first call. Doing it here, on the
        // main thread, keeps startup fast and turns a broken installation
        // into an error the viewer shows, instead of a panic.
        gst::init()?;
        let uri = glib::filename_to_uri(path, None)?;
        let bin = gst::parse::bin_from_description(&sink_description(), true)?;
        let limit = bin
            .by_name("limit")
            .ok_or_else(|| failure("no capsfilter in the video sink"))?;
        let sink = bin
            .by_name("frames")
            .and_downcast::<gst_app::AppSink>()
            .ok_or_else(|| failure("no appsink in the video sink"))?;

        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", uri.as_str())
            .property("video-sink", &bin)
            .build()
            .map_err(failure)?;
        if !audio {
            playbin.set_property_from_str("flags", "video+soft-volume");
        }

        // Frames come from streaming threads. Only the newest is kept, and
        // the main thread is woken at most once for it.
        let latest: Arc<Mutex<Option<Frame>>> = Arc::default();
        let (wake, woken) = async_channel::bounded::<()>(1);
        let publish = {
            let latest = Arc::clone(&latest);
            move |sample: &gst::Sample| {
                if let Some(frame) = frame_from(sample) {
                    *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
                    let _ = wake.try_send(());
                }
            }
        };
        let publish = Arc::new(publish);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample({
                    let publish = Arc::clone(&publish);
                    move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        publish(&sample);
                        Ok(gst::FlowSuccess::Ok)
                    }
                })
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                    publish(&sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        glib::spawn_future_local({
            let handler = Rc::clone(&handler);
            async move {
                while woken.recv().await.is_ok() {
                    let frame = latest.lock().unwrap_or_else(PoisonError::into_inner).take();
                    if let Some(frame) = frame {
                        handler(Event::Frame(frame));
                    }
                }
            }
        });

        let bus = playbin.bus().ok_or_else(|| failure("no message bus"))?;
        let mut prepared = false;
        let watch = bus
            .add_watch_local({
                let playbin = playbin.clone();
                move |_, message| {
                    use gst::MessageView;
                    match message.view() {
                        // The first one is the file opening; later ones are seeks.
                        MessageView::AsyncDone(_) if !prepared => {
                            prepared = true;
                            limit_size(&limit);
                            handler(Event::Prepared(info(&playbin)));
                        }
                        MessageView::AsyncDone(_) => handler(Event::Seeked),
                        MessageView::Eos(_) => handler(Event::Ended),
                        MessageView::Error(error) => {
                            handler(Event::Failed(error.error().to_string()))
                        }
                        _ => {}
                    }
                    glib::ControlFlow::Continue
                }
            })
            .map_err(failure)?;

        playbin
            .set_state(gst::State::Paused)
            .map_err(|error| failure(format!("could not open the video: {error}")))?;
        Ok(Self {
            playbin,
            sink,
            _watch: watch,
        })
    }

    pub fn play(&self) -> bool {
        self.playbin.set_state(gst::State::Playing).is_ok()
    }

    pub fn pause(&self) {
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    /// Seeks to a position in microseconds. [`Event::Seeked`] follows.
    pub fn seek(&self, microseconds: i64) -> bool {
        let target = gst::ClockTime::from_useconds(u64::try_from(microseconds).unwrap_or(0));
        self.playbin
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, target)
            .is_ok()
    }

    pub fn set_audio(&self, muted: bool, volume: f64) {
        self.playbin.set_property("mute", muted);
        self.playbin.set_property("volume", volume);
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
        // Releases the channel the main thread waits on.
        self.sink
            .set_callbacks(gst_app::AppSinkCallbacks::builder().build());
    }
}

fn info(playbin: &gst::Element) -> Info {
    let mut seeking = gst::query::Seeking::new(gst::Format::Time);
    let seekable = playbin.query(&mut seeking) && seeking.result().0;
    let streams = |name: &str| playbin.property::<i32>(name) > 0;
    Info {
        duration: playbin.query_duration::<gst::ClockTime>(),
        seekable,
        has_audio: streams("n-audio"),
        has_video: streams("n-video"),
    }
}

/// Scales oversized videos down, now that their real size is known. The size
/// is read where it is limited, before any rotation.
fn limit_size(limit: &gst::Element) {
    let Some(caps) = limit.static_pad("src").and_then(|pad| pad.current_caps()) else {
        return;
    };
    let Ok(video) = gst_video::VideoInfo::from_caps(&caps) else {
        return;
    };
    let Some((width, height)) = limited_size(video.width(), video.height(), MAX_EDGE) else {
        return;
    };
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "BGRx")
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .field("width", i32::try_from(width).unwrap_or(i32::MAX))
        .field("height", i32::try_from(height).unwrap_or(i32::MAX))
        .build();
    limit.set_property("caps", &caps);
}

/// The size to scale a frame to so its longer side is at most `max`, with
/// even sides, or `None` when it already fits.
fn limited_size(width: u32, height: u32, max: u32) -> Option<(u32, u32)> {
    if width.max(height) <= max {
        return None;
    }
    let scale = f64::from(max) / f64::from(width.max(height));
    let even = |side: u32| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scaled = (f64::from(side) * scale / 2.0).round() as u32 * 2;
        scaled.max(2)
    };
    Some((even(width), even(height)))
}

fn frame_from(sample: &gst::Sample) -> Option<Frame> {
    let video = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let buffer = sample.buffer()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &video).ok()?;
    Some(Frame {
        bytes: glib::Bytes::from(frame.plane_data(0).ok()?),
        width: i32::try_from(video.width()).ok()?,
        height: i32::try_from(video.height()).ok()?,
        stride: usize::try_from(frame.plane_stride()[0]).ok()?,
        position: buffer.pts(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    /// Tests count the process's descriptors, so they run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Runs `body` with its own main context, which the pipeline's watches
    /// and wake-ups attach to.
    fn with_main_context(body: impl FnOnce()) {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        glib::MainContext::new()
            .with_thread_default(body)
            .expect("main context");
    }

    fn clip() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../medianest-media/tests/fixtures/clip.mp4")
    }

    /// Whether this machine can decode the test clip.
    fn can_play() -> bool {
        gst::init().is_ok()
            && ["playbin", "appsink"]
                .iter()
                .all(|name| gst::ElementFactory::find(name).is_some())
            && (gst::ElementFactory::find("vapostproc").is_some()
                || ["videoconvert", "videoflip"]
                    .iter()
                    .all(|name| gst::ElementFactory::find(name).is_some()))
            && ["avdec_h264", "vah264dec", "openh264dec"]
                .iter()
                .any(|name| gst::ElementFactory::find(name).is_some())
    }

    fn pump_until(mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::ref_thread_default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            if Instant::now() > deadline {
                return false;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Lets queued main-loop work, such as the end of a wake-up loop, run.
    fn settle() {
        let context = glib::MainContext::ref_thread_default();
        let until = Instant::now() + Duration::from_millis(150);
        while Instant::now() < until {
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn descriptors() -> usize {
        std::fs::read_dir("/proc/self/fd").map_or(0, Iterator::count)
    }

    /// Opens the clip, plays a few frames, then drops the pipeline.
    fn play_once() -> usize {
        let events = Rc::new(RefCell::new(Vec::new()));
        let pipeline = Pipeline::open(&clip(), false, {
            let events = Rc::clone(&events);
            Rc::new(move |event| events.borrow_mut().push(event))
        })
        .expect("open");
        assert!(
            pump_until(|| {
                events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, Event::Prepared(_)))
            }),
            "the clip never became ready"
        );
        assert!(pipeline.play());
        assert!(
            pump_until(|| {
                events
                    .borrow()
                    .iter()
                    .filter(|event| matches!(event, Event::Frame(_)))
                    .count()
                    >= 3
            }),
            "no frames arrived"
        );
        let shapes: Vec<_> = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                Event::Frame(frame) => Some((frame.width, frame.height, frame.stride)),
                _ => None,
            })
            .collect();
        for (width, height, stride) in shapes {
            assert!(width > 0 && height > 0);
            assert!(stride >= usize::try_from(width).expect("width") * 4);
        }
        drop(pipeline);
        settle();
        descriptors()
    }

    #[test]
    fn playing_and_dropping_videos_does_not_leak_descriptors() {
        if !can_play() {
            return;
        }
        with_main_context(|| {
            // Warm up lazily created process-wide resources first.
            for _ in 0..3 {
                play_once();
            }
            let before = descriptors();
            for _ in 0..12 {
                play_once();
            }
            let after = descriptors();
            assert!(
                after <= before + 2,
                "descriptors grew from {before} to {after} over 12 videos"
            );
        });
    }

    /// A 2 second, 20 fps clip that is 3000x1688 as coded and tagged to be
    /// shown rotated, like a phone video held upright. `None` without ffmpeg.
    fn rotated_clip(dir: &Path) -> Option<PathBuf> {
        let ffmpeg = |args: &[&str]| {
            std::process::Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y"])
                .args(args)
                .status()
                .is_ok_and(|status| status.success())
        };
        let plain = dir.join("plain.mp4");
        let rotated = dir.join("rotated.mp4");
        let made = ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=3000x1688:rate=20",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            plain.to_str()?,
        ]) && ffmpeg(&[
            "-display_rotation",
            "90",
            "-i",
            plain.to_str()?,
            "-c",
            "copy",
            rotated.to_str()?,
        ]);
        made.then_some(rotated)
    }

    /// Rotated phone videos once played several times too fast with their
    /// clock stuck at zero, because their frames lost their timestamps.
    #[test]
    fn rotated_videos_keep_their_timestamps_and_play_in_real_time() {
        if !can_play() {
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let Some(clip) = rotated_clip(dir.path()) else {
            return;
        };
        with_main_context(|| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let events = Rc::new(RefCell::new(Vec::new()));
            let pipeline = Pipeline::open(&clip, false, {
                let seen = Rc::clone(&seen);
                let events = Rc::clone(&events);
                Rc::new(move |event| match event {
                    Event::Frame(frame) => seen.borrow_mut().push((
                        Instant::now(),
                        frame.position,
                        frame.width,
                        frame.height,
                    )),
                    other => events.borrow_mut().push(other),
                })
            })
            .expect("open");
            assert!(pump_until(|| {
                events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, Event::Prepared(_)))
            }));
            assert!(pipeline.play());
            assert!(
                pump_until(|| {
                    events
                        .borrow()
                        .iter()
                        .any(|event| matches!(event, Event::Ended))
                }),
                "the clip never ended"
            );
            let seen = seen.borrow();
            assert!(seen.len() >= 10, "only {} frames", seen.len());
            let positions: Vec<_> = seen
                .iter()
                .map(|frame| frame.1.expect("a frame without a timestamp"))
                .collect();
            // The paused first frame is shown again when playing starts.
            assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
            assert!(positions[0] < positions[positions.len() - 1]);
            // Upright, within the size limit, in the shape of the video.
            let (_, _, width, height) = *seen.last().expect("frames");
            assert!(height > width, "{width}x{height} is not upright");
            assert!(u32::try_from(height).expect("height") <= MAX_EDGE);
            let shape = f64::from(width) / f64::from(height);
            assert!((shape - 1688.0 / 3000.0).abs() < 0.01, "{width}x{height}");
            // The video's own time and the clock agree.
            let shown = seen.last().expect("frames").0.duration_since(seen[0].0);
            let played = positions[positions.len() - 1] - positions[0];
            let ratio = shown.as_secs_f64() / played.mseconds() as f64 * 1000.0;
            assert!(
                (0.7..1.5).contains(&ratio),
                "{played} of video took {shown:?}"
            );
        });
    }

    #[test]
    fn seeking_reports_when_it_finishes() {
        if !can_play() {
            return;
        }
        with_main_context(seeking_reports_when_it_finishes_in_context);
    }

    fn seeking_reports_when_it_finishes_in_context() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let pipeline = Pipeline::open(&clip(), false, {
            let events = Rc::clone(&events);
            Rc::new(move |event| events.borrow_mut().push(event))
        })
        .expect("open");
        let seekable = |events: &RefCell<Vec<Event>>| {
            events.borrow().iter().find_map(|event| match event {
                Event::Prepared(info) => Some(*info),
                _ => None,
            })
        };
        assert!(pump_until(|| seekable(&events).is_some()));
        let info = seekable(&events).expect("info");
        assert!(info.seekable && info.has_video);
        assert!(info.duration.is_some_and(|d| d.mseconds() > 1500));
        assert!(pipeline.seek(1_000_000));
        assert!(pump_until(|| {
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, Event::Seeked))
        }));
    }

    #[test]
    fn large_frames_are_limited_with_even_sides_and_their_shape() {
        assert_eq!(limited_size(1920, 1080, 2560), None);
        assert_eq!(limited_size(2560, 1440, 2560), None);
        assert_eq!(limited_size(3840, 2160, 2560), Some((2560, 1440)));
        assert_eq!(limited_size(2160, 3840, 2560), Some((1440, 2560)));
        let (width, height) = limited_size(4001, 3001, 2560).expect("limited");
        assert!(width % 2 == 0 && height % 2 == 0);
        assert!(width.max(height) <= 2560);
    }
}
