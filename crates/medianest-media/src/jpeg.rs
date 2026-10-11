//! JPEG structure: frame size, XMP packets, embedded previews and motion photos.

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Header facts read from JPEG markers without decoding pixels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JpegHeader {
    pub width: u32,
    pub height: u32,
    /// `true` for lossless (SOF3) streams, which in RAW files hold sensor data.
    pub lossless: bool,
    pub xmp: Option<String>,
}

/// Walks markers up to the first scan. Returns `None` if `data` is not a JPEG.
#[must_use]
pub fn read_header(data: &[u8]) -> Option<JpegHeader> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut header = JpegHeader::default();
    let mut pos = 2usize;
    loop {
        // Skip fill bytes before a marker.
        while data.get(pos) == Some(&0xFF) && data.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        if data.get(pos) != Some(&0xFF) {
            return None;
        }
        let marker = *data.get(pos + 1)?;
        pos += 2;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 | 0xDA => break,
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]));
        if length < 2 {
            return None;
        }
        let segment = data.get(pos + 2..pos + length)?;
        match marker {
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                if segment.len() >= 5 {
                    header.height = u32::from(u16::from_be_bytes([segment[1], segment[2]]));
                    header.width = u32::from(u16::from_be_bytes([segment[3], segment[4]]));
                    header.lossless = matches!(marker, 0xC3 | 0xC7 | 0xCB | 0xCF);
                }
            }
            0xE1 if segment.starts_with(XMP_SIGNATURE) && header.xmp.is_none() => {
                header.xmp =
                    Some(String::from_utf8_lossy(&segment[XMP_SIGNATURE.len()..]).into_owned());
            }
            _ => {}
        }
        pos += length;
        if header.width > 0 && header.xmp.is_some() {
            break;
        }
    }
    (header.width > 0 && header.height > 0).then_some(header)
}

/// The TIFF payload of a JPEG's EXIF segment, if present.
#[must_use]
pub fn exif_payload(data: &[u8]) -> Option<&[u8]> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2usize;
    while data.get(pos) == Some(&0xFF) {
        let marker = *data.get(pos + 1)?;
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([
            *data.get(pos + 2)?,
            *data.get(pos + 3)?,
        ]));
        let segment = data.get(pos + 4..pos + 2 + length)?;
        if marker == 0xE1 && segment.starts_with(b"Exif\0\0") {
            return Some(&segment[6..]);
        }
        pos += 2 + length;
    }
    None
}

/// An embedded JPEG found inside another file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedJpeg {
    pub offset: usize,
    pub width: u32,
    pub height: u32,
}

/// Finds the largest displayable JPEG stream inside `data`.
///
/// RAW files from almost every camera embed a JPEG preview, often at full
/// resolution. Scanning for start-of-image markers works across vendors and
/// container formats without parsing each one.
#[must_use]
pub fn largest_embedded(data: &[u8]) -> Option<EmbeddedJpeg> {
    let mut best: Option<EmbeddedJpeg> = None;
    let mut pos = 0;
    while let Some(found) = find_soi(data, pos) {
        pos = found + 3;
        let Some(header) = read_header(&data[found..]) else {
            continue;
        };
        if header.lossless || header.width < 64 || header.height < 64 {
            continue;
        }
        let area = u64::from(header.width) * u64::from(header.height);
        if best.is_none_or(|b| u64::from(b.width) * u64::from(b.height) < area) {
            best = Some(EmbeddedJpeg {
                offset: found,
                width: header.width,
                height: header.height,
            });
        }
    }
    best
}

fn find_soi(data: &[u8], from: usize) -> Option<usize> {
    let haystack = data.get(from..)?;
    haystack
        .windows(3)
        .position(|window| window == [0xFF, 0xD8, 0xFF])
        .map(|index| index + from)
}

fn xmp_attribute(xmp: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        if let Some(start) = xmp.find(&needle) {
            let rest = &xmp[start + needle.len()..];
            let end = rest.find(quote)?;
            return Some(rest[..end].to_owned());
        }
    }
    let open = format!("<{name}>");
    let start = xmp.find(&open)? + open.len();
    let end = xmp[start..].find('<')?;
    Some(xmp[start..start + end].trim().to_owned())
}

/// Byte length of a motion video appended to a still photo, measured from
/// the end of the file. Understands Google's Motion Photo and MicroVideo XMP
/// and Samsung's trailer.
#[must_use]
pub fn motion_video_length(xmp: Option<&str>, file: &[u8]) -> Option<u64> {
    let tail_starts_video = |length: u64| -> bool {
        usize::try_from(length)
            .ok()
            .and_then(|length| file.len().checked_sub(length))
            .and_then(|start| file.get(start + 4..start + 8))
            .is_some_and(|kind| kind == b"ftyp")
    };
    if let Some(xmp) = xmp {
        if let Some(offset) =
            xmp_attribute(xmp, "GCamera:MicroVideoOffset").and_then(|v| v.parse().ok())
        {
            if tail_starts_video(offset) {
                return Some(offset);
            }
        }
        // Container directory: the MotionPhoto item's Length is the video size.
        let mut search = xmp;
        while let Some(index) = search.find("Item:Semantic=\"MotionPhoto\"") {
            let item_start = search[..index].rfind('<').unwrap_or(0);
            let item_end = search[index..]
                .find("/>")
                .map_or(search.len(), |end| index + end);
            let item = &search[item_start..item_end];
            if let Some(length) =
                xmp_attribute(item, "Item:Length").and_then(|v| v.parse::<u64>().ok())
            {
                if tail_starts_video(length) {
                    return Some(length);
                }
            }
            search = &search[index + 1..];
        }
    }
    // Samsung stores "MotionPhoto_Data" right before the MP4 bytes.
    const SAMSUNG: &[u8] = b"MotionPhoto_Data";
    let window_start = file.len().saturating_sub(64 * 1024 * 1024);
    let haystack = &file[window_start..];
    let found = haystack
        .windows(SAMSUNG.len())
        .rposition(|window| window == SAMSUNG)?;
    let video_start = window_start + found + SAMSUNG.len();
    let length = u64::try_from(file.len() - video_start).ok()?;
    tail_starts_video(length).then_some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_jpeg(width: u16, height: u16, lossless: bool) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta GCamera:MotionPhoto=\"1\"/>";
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&u16::try_from(xmp.len() + 2).expect("len").to_be_bytes());
        out.extend_from_slice(xmp);
        out.extend_from_slice(&[0xFF, if lossless { 0xC3 } else { 0xC0 }, 0, 11, 8]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&[1, 1, 0x11, 0]);
        out.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        out
    }

    #[test]
    fn reads_frame_size_and_xmp() {
        let header = read_header(&tiny_jpeg(640, 480, false)).expect("header");
        assert_eq!((header.width, header.height), (640, 480));
        assert!(
            header
                .xmp
                .as_deref()
                .is_some_and(|x| x.contains("MotionPhoto"))
        );
        assert!(read_header(b"not a jpeg").is_none());
    }

    #[test]
    fn finds_largest_non_lossless_preview() {
        let mut raw = b"II*\0junk".to_vec();
        raw.extend(tiny_jpeg(160, 120, false));
        raw.extend(tiny_jpeg(6000, 4000, true));
        let big_offset = raw.len();
        raw.extend(tiny_jpeg(1620, 1080, false));
        let found = largest_embedded(&raw).expect("preview");
        assert_eq!(
            (found.offset, found.width, found.height),
            (big_offset, 1620, 1080)
        );
    }

    #[test]
    fn detects_motion_photo_lengths() {
        let mut file = tiny_jpeg(100, 100, false);
        let video = b"\0\0\0\x18ftypmp42\0\0\0\0isommp42moov".to_vec();
        file.extend_from_slice(&video);
        let length = video.len() as u64;
        let xmp = format!(
            "<rdf:li><Container:Item Item:Mime=\"image/jpeg\" Item:Semantic=\"Primary\"/></rdf:li>\
             <rdf:li><Container:Item Item:Mime=\"video/mp4\" Item:Semantic=\"MotionPhoto\" Item:Length=\"{length}\"/></rdf:li>"
        );
        assert_eq!(motion_video_length(Some(&xmp), &file), Some(length));
        let micro = format!("GCamera:MicroVideoOffset=\"{length}\"");
        assert_eq!(motion_video_length(Some(&micro), &file), Some(length));
        let mut samsung = tiny_jpeg(100, 100, false);
        samsung.extend_from_slice(b"MotionPhoto_Data");
        samsung.extend_from_slice(&video);
        assert_eq!(motion_video_length(None, &samsung), Some(length));
        assert_eq!(motion_video_length(None, &tiny_jpeg(10, 10, false)), None);
    }
}
