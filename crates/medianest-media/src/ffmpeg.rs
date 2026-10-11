//! Video support through the external `ffprobe` and `ffmpeg` programs.
//!
//! Running them as separate processes keeps malformed videos from affecting
//! MediaNest itself. Both tools are optional: without them videos are still
//! catalogued, but without duration, size or thumbnails.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, Utc};
use medianest_core::{CaptureTime, Confidence, DateSource, GeoPoint};
use serde::Deserialize;

/// Locations of the optional video tools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoTools {
    pub ffprobe: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
}

impl VideoTools {
    /// Looks the tools up on `PATH` without running them.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            ffprobe: find_program("ffprobe"),
            ffmpeg: find_program("ffmpeg"),
        }
    }

    #[must_use]
    pub fn available(&self) -> bool {
        self.ffprobe.is_some() && self.ffmpeg.is_some()
    }
}

fn find_program(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0} is not installed")]
    Missing(&'static str),
    #[error("could not run {0}: {1}")]
    Spawn(&'static str, std::io::Error),
    #[error("{0} took too long and was stopped")]
    Timeout(&'static str),
    #[error("{0} failed: {1}")]
    Failed(&'static str, String),
}

/// Runs a command, capturing at most `limit` bytes of stdout, and kills it after `timeout`.
fn run_bounded(
    name: &'static str,
    mut command: Command,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, ToolError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ToolError::Spawn(name, error))?;
    let mut stdout = child.stdout.take().ok_or(ToolError::Missing(name))?;
    let mut stderr = child.stderr.take().ok_or(ToolError::Missing(name))?;
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let result = (&mut stdout)
            .take(u64::try_from(limit).unwrap_or(u64::MAX))
            .read_to_end(&mut out);
        // Drain anything beyond the limit so the child never blocks on a full pipe.
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        let _ = sender.send(result.map(|_| out));
    });
    let err_reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stderr).take(8192).read_to_end(&mut out);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        out
    });
    let output = match receiver.recv_timeout(timeout) {
        Ok(result) => result.map_err(|error| ToolError::Spawn(name, error)),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let _ = err_reader.join();
            return Err(ToolError::Timeout(name));
        }
    };
    let status = child
        .wait()
        .map_err(|error| ToolError::Spawn(name, error))?;
    let _ = reader.join();
    let errors = err_reader.join().unwrap_or_default();
    let output = output?;
    if status.success() {
        Ok(output)
    } else {
        Err(ToolError::Failed(
            name,
            String::from_utf8_lossy(&errors)
                .lines()
                .last()
                .unwrap_or("unknown error")
                .to_owned(),
        ))
    }
}

/// Facts about a video read by `ffprobe`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub duration_ms: Option<u64>,
    /// Displayed size, with rotation already applied.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub taken: Option<CaptureTime>,
    pub location: Option<GeoPoint>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub content_identifier: Option<String>,
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
    disposition: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

/// Parses an ISO 6709 location such as `+37.7858-122.4064+010.000/`.
#[must_use]
pub fn parse_iso6709(value: &str) -> Option<GeoPoint> {
    let value = value.trim().trim_end_matches('/');
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if (ch == '+' || ch == '-') && !current.is_empty() {
            numbers.push(current.clone());
            current.clear();
        }
        current.push(ch);
    }
    if !current.is_empty() {
        numbers.push(current);
    }
    let latitude: f64 = numbers.first()?.parse().ok()?;
    let longitude: f64 = numbers.get(1)?.parse().ok()?;
    let altitude = numbers.get(2).and_then(|text| text.parse().ok());
    GeoPoint::new(latitude, longitude, altitude)
}

fn tag<'a>(tags: &'a std::collections::HashMap<String, String>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        tags.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())
    })
}

fn creation_time(tags: &std::collections::HashMap<String, String>) -> Option<CaptureTime> {
    // Apple records local time with its offset; prefer it.
    if let Some(value) = tag(tags, &["com.apple.quicktime.creationdate"]) {
        let normalized = if value.len() > 5
            && (value.ends_with(|c: char| c.is_ascii_digit()))
            && value[value.len() - 5..].starts_with(['+', '-'])
        {
            format!(
                "{}:{}",
                &value[..value.len() - 2],
                &value[value.len() - 2..]
            )
        } else {
            value.to_owned()
        };
        if let Ok(parsed) = DateTime::<FixedOffset>::parse_from_rfc3339(&normalized) {
            return Some(CaptureTime::from_instant(
                parsed,
                DateSource::Container,
                Confidence::High,
            ));
        }
    }
    let value = tag(tags, &["creation_time", "date"])?;
    let instant = DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|naive| naive.and_utc())
        })?;
    // Many devices write 1904-01-01 or 1970-01-01 when the clock is unset.
    if instant.timestamp() < 315_532_800 {
        return None;
    }
    // Containers store UTC; show it in this computer's timezone.
    Some(CaptureTime::from_instant(
        instant.with_timezone(&Local),
        DateSource::Container,
        Confidence::Medium,
    ))
}

fn rotation(stream: &ProbeStream) -> i64 {
    let from_side_data = stream
        .side_data_list
        .iter()
        .find_map(|entry| entry.get("rotation").and_then(serde_json::Value::as_f64));
    #[allow(clippy::cast_possible_truncation)]
    let degrees = from_side_data
        .map(|value| value.round() as i64)
        .or_else(|| {
            stream
                .tags
                .get("rotate")
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0);
    degrees.rem_euclid(360)
}

/// Parses `ffprobe -print_format json -show_format -show_streams` output.
pub fn parse_probe(json: &[u8]) -> Option<VideoInfo> {
    let output: ProbeOutput = serde_json::from_slice(json).ok()?;
    let format = output.format?;
    let video = output.streams.iter().find(|stream| {
        let attached_picture = stream
            .disposition
            .as_ref()
            .and_then(|d| d.get("attached_pic"))
            .and_then(serde_json::Value::as_i64)
            == Some(1);
        stream.codec_type.as_deref() == Some("video") && !attached_picture
    });
    let (width, height) = match video {
        Some(stream) => match (stream.width, stream.height) {
            (Some(w), Some(h)) if rotation(stream) % 180 == 90 => (Some(h), Some(w)),
            (w, h) => (w, h),
        },
        None => (None, None),
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let duration_ms = format
        .duration
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|seconds| (seconds * 1000.0).round() as u64);
    let mut tags = format.tags.clone();
    if let Some(stream) = video {
        for (key, value) in &stream.tags {
            tags.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    Some(VideoInfo {
        duration_ms,
        width,
        height,
        taken: creation_time(&tags),
        location: tag(
            &tags,
            &[
                "com.apple.quicktime.location.ISO6709",
                "location",
                "location-eng",
            ],
        )
        .and_then(parse_iso6709),
        make: tag(
            &tags,
            &[
                "com.apple.quicktime.make",
                "com.android.manufacturer",
                "make",
            ],
        )
        .map(str::to_owned),
        model: tag(
            &tags,
            &["com.apple.quicktime.model", "com.android.model", "model"],
        )
        .map(str::to_owned),
        content_identifier: tag(&tags, &["com.apple.quicktime.content.identifier"])
            .map(str::to_owned),
    })
}

impl VideoTools {
    /// Reads a video's duration, size, date and location.
    pub fn probe(&self, path: &Path) -> Result<VideoInfo, ToolError> {
        let program = self.ffprobe.as_ref().ok_or(ToolError::Missing("ffprobe"))?;
        let mut command = Command::new(program);
        command
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg("--")
            .arg(path);
        let output = run_bounded("ffprobe", command, Duration::from_secs(20), 4 * 1024 * 1024)?;
        parse_probe(&output).ok_or_else(|| ToolError::Failed("ffprobe", "unreadable output".into()))
    }

    /// Extracts one frame as JPEG bytes, scaled to at most `max_edge` pixels.
    pub fn frame(&self, path: &Path, at_seconds: f64, max_edge: u32) -> Result<Vec<u8>, ToolError> {
        let program = self.ffmpeg.as_ref().ok_or(ToolError::Missing("ffmpeg"))?;
        let scale = format!(
            "scale='if(gt(iw,ih),min({max_edge},iw),-2)':'if(gt(iw,ih),-2,min({max_edge},ih))'"
        );
        let mut command = Command::new(program);
        command
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-ss"])
            .arg(format!("{at_seconds:.3}"))
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-vf"])
            .arg(scale)
            .args(["-f", "image2pipe", "-c:v", "mjpeg", "-q:v", "3", "-"]);
        let bytes = run_bounded("ffmpeg", command, Duration::from_secs(30), 32 * 1024 * 1024)?;
        if bytes.is_empty() {
            return Err(ToolError::Failed("ffmpeg", "no frame produced".into()));
        }
        Ok(bytes)
    }

    /// Copies a byte range of a file into a new file. Used to extract the
    /// video embedded in a motion photo for playback.
    pub fn extract_tail(source: &Path, length: u64, destination: &Path) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        let mut input = std::fs::File::open(source)?;
        let size = input.metadata()?.len();
        let start = size.checked_sub(length).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "embedded video is larger than the file",
            )
        })?;
        input.seek(SeekFrom::Start(start))?;
        let mut output = std::fs::File::create(destination)?;
        std::io::copy(&mut input.take(length), &mut output)?;
        output.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso6709() {
        let point = parse_iso6709("+37.7858-122.4064+010.000/").expect("point");
        assert!((point.latitude - 37.7858).abs() < 1e-9);
        assert!((point.longitude + 122.4064).abs() < 1e-9);
        assert_eq!(point.altitude, Some(10.0));
        assert!(parse_iso6709("nonsense").is_none());
    }

    #[test]
    fn parses_ffprobe_output_with_rotation_and_apple_date() {
        let json = br#"{
          "streams": [
            {"codec_type": "audio"},
            {"codec_type": "video", "width": 1920, "height": 1080,
             "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
             "tags": {"creation_time": "2023-07-01T08:15:00.000000Z"}}
          ],
          "format": {"duration": "12.345",
            "tags": {"com.apple.quicktime.creationdate": "2023-07-01T10:15:00+0200",
                     "com.apple.quicktime.location.ISO6709": "+48.8584+002.2945+035.000/",
                     "com.apple.quicktime.make": "Apple",
                     "com.apple.quicktime.model": "iPhone 14",
                     "com.apple.quicktime.content.identifier": "ABC-123"}}
        }"#;
        let info = parse_probe(json).expect("info");
        assert_eq!(info.duration_ms, Some(12_345));
        assert_eq!((info.width, info.height), (Some(1080), Some(1920)));
        let taken = info.taken.expect("taken");
        assert_eq!(taken.offset_minutes, Some(120));
        assert_eq!(taken.confidence, Confidence::High);
        assert_eq!(
            taken
                .local()
                .map(|t| t.format("%H:%M").to_string())
                .as_deref(),
            Some("10:15")
        );
        assert!(info.location.is_some());
        assert_eq!(info.model.as_deref(), Some("iPhone 14"));
        assert_eq!(info.content_identifier.as_deref(), Some("ABC-123"));
    }

    #[test]
    fn ignores_placeholder_creation_times() {
        let json =
            br#"{"streams": [], "format": {"tags": {"creation_time": "1904-01-01T00:00:00Z"}}}"#;
        assert_eq!(parse_probe(json).expect("info").taken, None);
    }
}
