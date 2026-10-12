//! MediaNest: a photo and video library for GNOME.

mod actions;
mod application;
mod config;
mod context;
mod dialogs;
mod filmstrips;
mod grid;
mod i18n;
mod library_view;
mod pages;
mod textures;
mod thumb;
mod util;
mod viewer;
mod welcome;
mod window;

use std::process::ExitCode;

use gtk::glib;

pub const APP_ID: &str = "io.github.mmrzaf.MediaNest";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const RESOURCE_BASE: &str = "/io/github/mmrzaf/MediaNest";

fn main() -> ExitCode {
    // Decoding of risky formats runs in copies of this executable.
    medianest_service::media::run_helper_if_requested();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MEDIANEST_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,medianest=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    i18n::init();
    glib::set_application_name("MediaNest");
    glib::set_prgname(Some(APP_ID));

    if let Err(error) = gtk::gio::resources_register_include!("medianest.gresource") {
        tracing::error!(%error, "could not load the bundled resources");
        return ExitCode::FAILURE;
    }

    let app = application::Application::new();
    let status = app.run();
    if status == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
