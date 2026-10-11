//! Recognises file formats from their first bytes.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use medianest_core::Format;

/// Bytes read from the start of a file for format detection.
pub const SNIFF_LEN: usize = 64;

/// Identifies a format from leading bytes. Returns `None` when unknown.
#[must_use]
pub fn sniff(header: &[u8]) -> Option<Format> {
    let starts = |magic: &[u8]| header.starts_with(magic);
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Some(Format::Jpeg);
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some(Format::Gif);
    }
    if header.len() >= 12 && &header[..4] == b"RIFF" {
        return match &header[8..12] {
            b"WEBP" => Some(Format::Webp),
            b"AVI " => Some(Format::Avi),
            _ => None,
        };
    }
    if starts(&[0xFF, 0x0A]) || starts(b"\x00\x00\x00\x0cJXL \r\n\x87\n") {
        return Some(Format::Jxl);
    }
    if starts(b"FUJIFILMCCD-RAW") {
        return Some(Format::Raf);
    }
    if starts(b"IIRO") || starts(b"IIRS") || starts(b"MMOR") {
        return Some(Format::Orf);
    }
    if starts(b"IIU\x00") {
        return Some(Format::Rw2);
    }
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(if header.windows(4).any(|w| w == b"webm") {
            Format::Webm
        } else {
            Format::Mkv
        });
    }
    if starts(b"II*\x00") || starts(b"MM\x00*") {
        // TIFF container: plain TIFF or one of several RAW formats. The
        // extension decides between them; report the generic container.
        if header.len() >= 10 && &header[8..10] == b"CR" {
            return Some(Format::Cr2);
        }
        return Some(Format::Tiff);
    }
    if header.len() >= 12 && &header[4..8] == b"ftyp" {
        return ftyp_format(&header[8..]);
    }
    if header.len() >= 5 && header[0] == 0x47 && header.get(188) == Some(&0x47) {
        return Some(Format::M2ts);
    }
    if header.len() >= 5 && header[4] == 0x47 && header.get(192) == Some(&0x47) {
        return Some(Format::M2ts);
    }
    None
}

fn ftyp_format(rest: &[u8]) -> Option<Format> {
    let major = rest.get(..4)?;
    let brands: Vec<&[u8]> = std::iter::once(major)
        .chain(rest.get(8..).unwrap_or(&[]).chunks_exact(4))
        .collect();
    let has = |brand: &[u8]| brands.contains(&brand);
    if has(b"crx ") {
        return Some(Format::Cr3);
    }
    if has(b"avif") || has(b"avis") {
        return Some(Format::Avif);
    }
    if has(b"heic")
        || has(b"heix")
        || has(b"heim")
        || has(b"heis")
        || has(b"hevc")
        || has(b"hevx")
        || has(b"mif1")
        || has(b"msf1")
    {
        return Some(Format::Heic);
    }
    if major == b"qt  " {
        return Some(Format::Mov);
    }
    if major.starts_with(b"3g") {
        return Some(Format::ThreeGp);
    }
    Some(Format::Mp4)
}

/// Decides a file's format from its content, falling back to its extension.
///
/// Content wins when the two disagree, which catches photos saved with the
/// wrong extension (for example HEIF data in a `.jpg`). TIFF-based RAW
/// formats are told apart by extension, since they share a container.
#[must_use]
pub fn detect(path: &Path) -> Option<Format> {
    let by_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(Format::from_extension);
    let mut header = [0u8; 200];
    let read = File::open(path)
        .and_then(|mut file| read_up_to(&mut file, &mut header))
        .unwrap_or(0);
    let by_content = sniff(&header[..read]);
    match (by_content, by_extension) {
        (Some(Format::Tiff | Format::Cr2), Some(ext))
            if ext.is_tiff_based() || ext == Format::Cr2 =>
        {
            Some(ext)
        }
        (Some(Format::Mp4), Some(ext))
            if matches!(ext, Format::Mov | Format::ThreeGp | Format::Mp4) =>
        {
            Some(ext)
        }
        (Some(content), _) => Some(content),
        (None, Some(ext)) if ext == Format::M2ts || ext.is_raw() => Some(ext),
        (None, _) => None,
    }
}

fn read_up_to(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        match file.read(&mut buffer[total..])? {
            0 => break,
            count => total += count,
        }
    }
    Ok(total)
}

/// Whether a path has an extension MediaNest considers, without reading it.
#[must_use]
pub fn has_media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(Format::from_extension)
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_common_signatures() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE1]), Some(Format::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Format::Png));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(
            sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
            Some(Format::Heic)
        );
        assert_eq!(
            sniff(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1"),
            Some(Format::Avif)
        );
        assert_eq!(sniff(b"\0\0\0\x14ftypqt  \0\0\0\0qt  "), Some(Format::Mov));
        assert_eq!(
            sniff(b"\0\0\0\x18ftypisom\0\0\0\0isomiso2"),
            Some(Format::Mp4)
        );
        assert_eq!(
            sniff(b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom"),
            Some(Format::Cr3)
        );
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn content_overrides_wrong_extension() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("actually-heic.jpg");
        std::fs::write(&path, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic").expect("write");
        assert_eq!(detect(&path), Some(Format::Heic));
        let raw = dir.path().join("DSC_0001.NEF");
        std::fs::write(&raw, b"MM\0*\0\0\0\x08rest").expect("write");
        assert_eq!(detect(&raw), Some(Format::Nef));
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"plain").expect("write");
        assert_eq!(detect(&text), None);
    }
}
