//! Video playback on GStreamer.
//!
//! [`PlayerStream`] is a `gtk::MediaStream`, so `gtk::Video`, its controls
//! and `gtk::Picture` play it like any other stream. It replaces GTK's own
//! `MediaFile`, whose GStreamer backend leaks descriptors and threads for
//! every video opened.

mod pipeline;
mod stream;

pub use stream::PlayerStream;
