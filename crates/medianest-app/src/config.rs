//! Preferences of the application itself, as opposed to a library's settings.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Libraries opened before, most recent first.
    pub recent: Vec<PathBuf>,
    /// Reopen the most recent library on start.
    pub open_last: bool,
    pub window_width: i32,
    pub window_height: i32,
    pub window_maximized: bool,
    /// Index into the grid's zoom levels.
    pub zoom: u32,
    /// Square tiles instead of the photos' own shapes.
    pub square_tiles: bool,
    /// Download map tiles from OpenStreetMap for the Places page.
    pub online_maps: bool,
    /// Play videos and Live Photos without sound until unmuted.
    pub start_muted: bool,
    /// Seconds per photo in slideshows.
    pub slideshow_seconds: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            open_last: true,
            window_width: 1200,
            window_height: 800,
            window_maximized: false,
            zoom: 2,
            square_tiles: false,
            online_maps: true,
            start_muted: false,
            slideshow_seconds: 4,
        }
    }
}

fn config_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("medianest")
        .join("config.json")
}

impl Config {
    #[must_use]
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "ignoring unreadable settings");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the configuration atomically.
    pub fn save(&self) {
        let path = config_path();
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
            let temp = path.with_extension("json.tmp");
            std::fs::write(&temp, json)?;
            std::fs::rename(&temp, &path)
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "could not save settings");
        }
    }

    pub fn remember(&mut self, library: &Path) {
        self.recent.retain(|path| path != library);
        self.recent.insert(0, library.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn forget(&mut self, library: &Path) {
        self.recent.retain(|path| path != library);
    }
}
