//! Turns image files into pixels, orientation applied.
//!
//! Pure-Rust decoders handle JPEG, PNG, WebP, GIF and TIFF. RAW files show
//! their embedded preview. HEIF and AVIF need the libheif C library and are
//! decoded in a separate helper process, and JPEG XL is decoded by ffmpeg, so
//! that a malformed file can at worst crash a helper, never MediaNest.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageReader, Limits, RgbaImage};
use medianest_core::Format;

use crate::ffmpeg::VideoTools;
use crate::jpeg;

/// Largest image MediaNest will decode, in pixels.
pub const MAX_PIXELS: u64 = 250_000_000;

/// Command-line flag that turns any MediaNest binary into a decode helper.
pub const HELPER_FLAG: &str = "--medianest-decode-helper";

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not read the file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the image is damaged or uses an unsupported variant: {0}")]
    Corrupt(String),
    #[error("the image is too large to open ({0} megapixels)")]
    TooLarge(u64),
    #[error("{0} images cannot be displayed")]
    Unsupported(&'static str),
    #[error("the image decoder stopped responding")]
    HelperTimeout,
    #[error("the image decoder failed: {0}")]
    Helper(String),
}

/// How large a decoded image needs to be. Decoders may return more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Full resolution.
    Full,
    /// At least this many pixels on the shorter side, for cropped thumbnails.
    ShortSide(u32),
    /// At least this many pixels on the longer side, for previews.
    LongSide(u32),
}

impl Size {
    fn wire(self) -> String {
        match self {
            Self::Full => "full".into(),
            Self::ShortSide(value) => format!("short:{value}"),
            Self::LongSide(value) => format!("long:{value}"),
        }
    }

    fn from_wire(text: &str) -> Option<Self> {
        match text.split_once(':') {
            None if text == "full" => Some(Self::Full),
            Some(("short", value)) => value.parse().ok().map(Self::ShortSide),
            Some(("long", value)) => value.parse().ok().map(Self::LongSide),
            _ => None,
        }
    }

    /// Whether an image of this size already satisfies the request.
    #[must_use]
    pub fn fits(self, width: u32, height: u32) -> bool {
        match self {
            Self::Full => true,
            Self::ShortSide(target) => width.min(height) >= target,
            Self::LongSide(target) => width.max(height) >= target,
        }
    }
}

/// Applies an EXIF orientation (1-8) to an image.
#[must_use]
pub fn orient(image: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Scales an image down so its shorter side is `short`, keeping the aspect ratio.
#[must_use]
pub fn shrink_short_side(image: &RgbaImage, short: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    let current = width.min(height);
    if current <= short {
        return image.clone();
    }
    let scale = f64::from(short) / f64::from(current);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (new_w, new_h) = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    imageops::resize(image, new_w, new_h, FilterType::Triangle)
}

/// Scales an image down so its longer side is at most `long`.
#[must_use]
pub fn shrink_long_side(image: &RgbaImage, long: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    let current = width.max(height);
    if current <= long {
        return image.clone();
    }
    let scale = f64::from(long) / f64::from(current);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (new_w, new_h) = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    imageops::resize(image, new_w, new_h, FilterType::Triangle)
}

fn check_pixels(width: u32, height: u32) -> Result<(), DecodeError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(DecodeError::TooLarge(pixels / 1_000_000));
    }
    Ok(())
}

/// Decodes JPEG bytes, using DCT scaling to skip work when less is needed.
pub fn decode_jpeg(reader: impl Read, size: Size) -> Result<DynamicImage, DecodeError> {
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(reader));
    decoder
        .read_info()
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| DecodeError::Corrupt("missing frame header".into()))?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    check_pixels(width, height)?;
    let request = match size {
        Size::Full => None,
        Size::ShortSide(target) => {
            let scale = f64::from(target) / f64::from(width.min(height).max(1));
            Some(scale)
        }
        Size::LongSide(target) => Some(f64::from(target) / f64::from(width.max(height).max(1))),
    };
    if let Some(scale) = request.filter(|scale| *scale < 1.0) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (w, h) = (
            (f64::from(width) * scale).ceil().max(1.0) as u16,
            (f64::from(height) * scale).ceil().max(1.0) as u16,
        );
        decoder
            .scale(w, h)
            .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    }
    let pixels = decoder
        .decode()
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| DecodeError::Corrupt("missing frame header".into()))?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let corrupt = || DecodeError::Corrupt("pixel data does not match the frame size".into());
    Ok(match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, pixels).ok_or_else(corrupt)?)
        }
        jpeg_decoder::PixelFormat::L16 => {
            let samples: Vec<u16> = pixels
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            DynamicImage::ImageLuma16(
                image::ImageBuffer::from_raw(w, h, samples).ok_or_else(corrupt)?,
            )
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, pixels).ok_or_else(corrupt)?)
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            let rgb: Vec<u8> = pixels
                .chunks_exact(4)
                .flat_map(|cmyk| {
                    let k = u16::from(cmyk[3]);
                    [cmyk[0], cmyk[1], cmyk[2]].map(|channel| {
                        u8::try_from(u16::from(channel) * k / 255).unwrap_or(u8::MAX)
                    })
                })
                .collect();
            DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, rgb).ok_or_else(corrupt)?)
        }
    })
}

fn decode_with_image_crate(path: &Path) -> Result<DynamicImage, DecodeError> {
    let mut reader = ImageReader::open(path)?
        .with_guessed_format()
        .map_err(DecodeError::Io)?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(2 * 1024 * 1024 * 1024);
    limits.max_image_width = Some(65_535);
    limits.max_image_height = Some(65_535);
    reader.limits(limits);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    check_pixels(width, height)?;
    let mut reader = ImageReader::open(path)?
        .with_guessed_format()
        .map_err(DecodeError::Io)?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(2 * 1024 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| DecodeError::Corrupt(error.to_string()))
}

/// JPEG XL is decoded by ffmpeg's libjxl decoder, in its own process.
fn decode_jxl(path: &Path, tools: &VideoTools) -> Result<DynamicImage, DecodeError> {
    let bytes = tools
        .frame(path, 0.0, 16_384)
        .map_err(|error| DecodeError::Helper(error.to_string()))?;
    decode_jpeg(bytes.as_slice(), Size::Full)
}

fn decode_raw_preview(path: &Path, size: Size) -> Result<DynamicImage, DecodeError> {
    let data = std::fs::read(path)?;
    let preview = jpeg::largest_embedded(&data).ok_or(DecodeError::Unsupported(
        "RAW files without an embedded preview",
    ))?;
    decode_jpeg(&data[preview.offset..], size)
}

/// Decodes HEIF or AVIF with libheif in this process. Only the helper calls this.
fn decode_heif_in_process(path: &Path) -> Result<RgbaImage, DecodeError> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let name = path
        .to_str()
        .ok_or_else(|| DecodeError::Corrupt("path is not valid UTF-8".into()))?;
    let library = LibHeif::new();
    let context = HeifContext::read_from_file(name)
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    let handle = context
        .primary_image_handle()
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    check_pixels(handle.width(), handle.height())?;
    let decoded = library
        .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)
        .map_err(|error| DecodeError::Corrupt(error.to_string()))?;
    let planes = decoded.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| DecodeError::Corrupt("no interleaved plane".into()))?;
    let (width, height) = (plane.width, plane.height);
    let row = usize::try_from(width).map_err(|_| DecodeError::TooLarge(0))? * 4;
    let mut pixels = Vec::with_capacity(row * usize::try_from(height).unwrap_or(0));
    for line in plane
        .data
        .chunks(plane.stride)
        .take(usize::try_from(height).unwrap_or(0))
    {
        pixels.extend_from_slice(
            line.get(..row)
                .ok_or_else(|| DecodeError::Corrupt("short row".into()))?,
        );
    }
    RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| DecodeError::Corrupt("pixel size mismatch".into()))
}

/// Where HEIF and AVIF decoding happens.
#[derive(Clone, Debug)]
pub enum HeifBackend {
    /// In helper processes started from this executable.
    Helper(Arc<HelperPool>),
    /// In this process. For tests and tools that accept the risk.
    InProcess,
}

/// Decodes images of every supported format.
#[derive(Clone, Debug)]
pub struct Decoder {
    heif: HeifBackend,
    tools: VideoTools,
}

impl Decoder {
    /// A decoder that isolates libheif in helper processes running `executable`.
    #[must_use]
    pub fn with_helper(executable: PathBuf) -> Self {
        Self {
            heif: HeifBackend::Helper(Arc::new(HelperPool::new(executable))),
            tools: VideoTools::detect(),
        }
    }

    /// A decoder that uses the current executable as its helper.
    #[must_use]
    pub fn isolated() -> Self {
        match std::env::current_exe() {
            Ok(executable) => Self::with_helper(executable),
            Err(_) => Self::in_process(),
        }
    }

    #[must_use]
    pub fn in_process() -> Self {
        Self {
            heif: HeifBackend::InProcess,
            tools: VideoTools::detect(),
        }
    }

    /// Decodes an image with its orientation applied.
    ///
    /// `orientation` is the EXIF orientation recorded in the catalog. Formats
    /// whose decoders orient by themselves (HEIF, AVIF, JPEG XL) are stored
    /// with orientation 1, so they are never rotated twice.
    pub fn decode(
        &self,
        path: &Path,
        format: Format,
        orientation: u8,
        size: Size,
    ) -> Result<RgbaImage, DecodeError> {
        let decoded = match format {
            Format::Jpeg => decode_jpeg(File::open(path)?, size)?,
            Format::Png | Format::Webp | Format::Gif | Format::Tiff => {
                decode_with_image_crate(path)?
            }
            Format::Jxl => decode_jxl(path, &self.tools)?,
            Format::Heic | Format::Avif => {
                let image = match &self.heif {
                    HeifBackend::Helper(pool) => pool.decode(path, size)?,
                    HeifBackend::InProcess => decode_heif_in_process(path)?,
                };
                DynamicImage::ImageRgba8(image)
            }
            format if format.is_raw() => decode_raw_preview(path, size)?,
            _ => return Err(DecodeError::Unsupported("Video")),
        };
        Ok(orient(decoded, orientation).into_rgba8())
    }
}

// ----- helper process -------------------------------------------------------

/// Runs the decode helper loop when the process was started with
/// [`HELPER_FLAG`]. Call this first thing in `main` of every binary that
/// creates an isolated [`Decoder`]. Returns normally otherwise.
pub fn run_helper_if_requested() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == HELPER_FLAG)
    {
        let code = match helper_loop() {
            Ok(()) => 0,
            Err(_) => 1,
        };
        std::process::exit(code);
    }
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn helper_loop() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut output = BufWriter::new(std::io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line?;
        let mut parts = line.split(' ');
        let (Some("decode"), Some(path), Some(size)) = (parts.next(), parts.next(), parts.next())
        else {
            writeln!(output, "error malformed request")?;
            output.flush()?;
            continue;
        };
        let result = hex_decode(path)
            .map(|bytes| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
            .ok_or_else(|| DecodeError::Corrupt("bad path".into()))
            .and_then(|path| {
                let size = Size::from_wire(size).unwrap_or(Size::Full);
                let image = decode_heif_in_process(&path)?;
                Ok(match size {
                    Size::Full => image,
                    Size::ShortSide(target) => shrink_short_side(&image, target),
                    Size::LongSide(target) => shrink_long_side(&image, target),
                })
            });
        match result {
            Ok(image) => {
                writeln!(output, "ok {} {}", image.width(), image.height())?;
                output.write_all(image.as_raw())?;
            }
            Err(error) => writeln!(output, "error {}", error.to_string().replace('\n', " "))?,
        }
        output.flush()?;
    }
    Ok(())
}

/// A decoded image from the helper, or its error message.
type HelperResponse = Result<(u32, u32, Vec<u8>), String>;

struct Helper {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<HelperResponse>,
}

impl Helper {
    fn spawn(executable: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(executable)
            .arg(HELPER_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("helper has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("helper has no stdout"))?;
        let (sender, responses) = channel();
        std::thread::Builder::new()
            .name("decode-helper-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let header = header.trim_end();
                    let response = if let Some(rest) = header.strip_prefix("ok ") {
                        let mut numbers = rest
                            .split(' ')
                            .filter_map(|value| value.parse::<u32>().ok());
                        match (numbers.next(), numbers.next()) {
                            (Some(width), Some(height)) => {
                                let length = u64::from(width) * u64::from(height) * 4;
                                let mut pixels = Vec::new();
                                match (&mut reader).take(length).read_to_end(&mut pixels) {
                                    Ok(read) if read as u64 == length => {
                                        Ok((width, height, pixels))
                                    }
                                    _ => Err("truncated image data".to_owned()),
                                }
                            }
                            _ => Err("malformed response".to_owned()),
                        }
                    } else {
                        Err(header.strip_prefix("error ").unwrap_or(header).to_owned())
                    };
                    let failed = response.is_err() && header.starts_with("ok");
                    if sender.send(response).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            child,
            stdin,
            responses,
        })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A small pool of decode helper processes.
pub struct HelperPool {
    executable: PathBuf,
    idle: Mutex<Vec<Helper>>,
    timeout: Duration,
}

impl std::fmt::Debug for HelperPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperPool")
            .field("executable", &self.executable)
            .finish_non_exhaustive()
    }
}

impl HelperPool {
    #[must_use]
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            idle: Mutex::new(Vec::new()),
            timeout: Duration::from_secs(60),
        }
    }

    fn decode(&self, path: &Path, size: Size) -> Result<RgbaImage, DecodeError> {
        let helper = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut helper = match helper {
            Some(helper) => helper,
            None => Helper::spawn(&self.executable)?,
        };
        let request = format!(
            "decode {} {}\n",
            hex_encode(path.as_os_str().as_bytes()),
            size.wire()
        );
        helper.stdin.write_all(request.as_bytes())?;
        helper.stdin.flush()?;
        let result = match helper.responses.recv_timeout(self.timeout) {
            Ok(Ok((width, height, pixels))) => {
                let image = RgbaImage::from_raw(width, height, pixels)
                    .ok_or_else(|| DecodeError::Helper("pixel size mismatch".into()));
                if image.is_ok() {
                    if let Ok(mut idle) = self.idle.lock() {
                        if idle.len() < 4 {
                            idle.push(helper);
                        }
                    }
                }
                return image;
            }
            Ok(Err(message)) => Err(DecodeError::Corrupt(message)),
            Err(RecvTimeoutError::Timeout) => Err(DecodeError::HelperTimeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(DecodeError::Helper("the decoder crashed".into()))
            }
        };
        if matches!(result, Err(DecodeError::Corrupt(_))) {
            if let Ok(mut idle) = self.idle.lock() {
                if idle.len() < 4 {
                    idle.push(helper);
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_jpeg(path: &Path, width: u32, height: u32) {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([
                u8::try_from(x % 256).unwrap_or(0),
                u8::try_from(y % 256).unwrap_or(0),
                128,
                255,
            ])
        });
        DynamicImage::ImageRgba8(image)
            .to_rgb8()
            .save_with_format(path, image::ImageFormat::Jpeg)
            .expect("save");
    }

    #[test]
    fn jpeg_scaled_decode_satisfies_request_cheaply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.jpg");
        write_jpeg(&path, 1600, 1200);
        let decoder = Decoder::in_process();
        let thumb = decoder
            .decode(&path, Format::Jpeg, 1, Size::ShortSide(300))
            .expect("decode");
        assert!(
            thumb.height() >= 300 && thumb.height() <= 600,
            "got {}x{}",
            thumb.width(),
            thumb.height()
        );
        let full = decoder
            .decode(&path, Format::Jpeg, 1, Size::Full)
            .expect("decode");
        assert_eq!(full.dimensions(), (1600, 1200));
    }

    #[test]
    fn orientation_rotates_dimensions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wide.jpg");
        write_jpeg(&path, 64, 32);
        let decoder = Decoder::in_process();
        for (orientation, expected) in [
            (1, (64, 32)),
            (3, (64, 32)),
            (6, (32, 64)),
            (8, (32, 64)),
            (5, (32, 64)),
        ] {
            let image = decoder
                .decode(&path, Format::Jpeg, orientation, Size::Full)
                .expect("decode");
            assert_eq!(image.dimensions(), expected, "orientation {orientation}");
        }
    }

    #[test]
    fn raw_files_use_their_largest_preview() {
        let dir = tempfile::tempdir().expect("tempdir");
        let jpeg_path = dir.path().join("preview.jpg");
        write_jpeg(&jpeg_path, 300, 200);
        let mut raw = b"II*\0 fake raw header ".to_vec();
        raw.extend(std::fs::read(&jpeg_path).expect("read"));
        raw.extend_from_slice(b" sensor data");
        let raw_path = dir.path().join("photo.nef");
        std::fs::write(&raw_path, raw).expect("write");
        let image = Decoder::in_process()
            .decode(&raw_path, Format::Nef, 6, Size::Full)
            .expect("decode");
        assert_eq!(image.dimensions(), (200, 300));
    }

    #[test]
    fn corrupt_files_fail_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("broken.jpg");
        std::fs::write(&path, b"\xff\xd8\xff\xe0 not really").expect("write");
        assert!(
            Decoder::in_process()
                .decode(&path, Format::Jpeg, 1, Size::Full)
                .is_err()
        );
        let png = dir.path().join("broken.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nnope").expect("write");
        assert!(
            Decoder::in_process()
                .decode(&png, Format::Png, 1, Size::Full)
                .is_err()
        );
    }

    #[test]
    fn size_wire_format_round_trips() {
        for size in [Size::Full, Size::ShortSide(360), Size::LongSide(2048)] {
            assert_eq!(Size::from_wire(&size.wire()), Some(size));
        }
        assert_eq!(
            hex_decode(&hex_encode(b"/tmp/caf\xe9.heic")),
            Some(b"/tmp/caf\xe9.heic".to_vec())
        );
    }
}
