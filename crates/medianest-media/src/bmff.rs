//! Just enough ISO base media file format parsing to size HEIF and AVIF images.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Displayed size of a HEIF/AVIF primary image, after its rotation property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeifInfo {
    pub width: u32,
    pub height: u32,
    /// Counter-clockwise rotation in quarter turns, from the `irot` property.
    pub quarter_turns: u8,
}

impl HeifInfo {
    /// Width and height as displayed.
    #[must_use]
    pub fn display_size(&self) -> (u32, u32) {
        if self.quarter_turns % 2 == 1 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.pos..self.pos.checked_add(count)?)?;
        self.pos += count;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2)
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// Iterates `(type, payload)` boxes within a buffer.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let header = data.get(pos..pos + 8)?;
        let mut size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let kind = [header[4], header[5], header[6], header[7]];
        let mut body_start = pos + 8;
        if size == 1 {
            let large = data.get(pos + 8..pos + 16)?;
            size = u64::from_be_bytes(large.try_into().ok()?);
            body_start = pos + 16;
        } else if size == 0 {
            size = u64::try_from(data.len() - pos).ok()?;
        }
        let end = pos.checked_add(usize::try_from(size).ok()?)?;
        if end > data.len() || end < body_start {
            return None;
        }
        let body = &data[body_start..end];
        pos = end;
        Some((kind, body))
    })
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data)
        .find(|(found, _)| found == kind)
        .map(|(_, body)| body)
}

/// Reads the top-level `meta` box of a HEIF file into memory.
fn read_meta(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let mut pos = 0u64;
    while pos + 8 <= length {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8]).ok()?;
        let mut size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let mut header_len = 8u64;
        if size == 1 {
            file.read_exact(&mut header[8..16]).ok()?;
            size = u64::from_be_bytes(header[8..16].try_into().ok()?);
            header_len = 16;
        } else if size == 0 {
            size = length - pos;
        }
        if size < header_len {
            return None;
        }
        if &header[4..8] == b"meta" {
            // Metadata boxes are small; refuse anything absurd.
            let body = size - header_len;
            if body > 64 * 1024 * 1024 {
                return None;
            }
            let mut data = vec![0u8; usize::try_from(body).ok()?];
            file.read_exact(&mut data).ok()?;
            return Some(data);
        }
        pos = pos.checked_add(size)?;
    }
    None
}

/// Size and rotation of a HEIF or AVIF primary image.
#[must_use]
pub fn heif_info(path: &Path) -> Option<HeifInfo> {
    let meta = read_meta(path)?;
    parse_meta(&meta)
}

fn parse_meta(meta: &[u8]) -> Option<HeifInfo> {
    // `meta` is a full box: skip version and flags.
    let children = meta.get(4..)?;
    let primary = {
        let pitm = find(children, b"pitm")?;
        let mut cursor = Cursor::new(pitm);
        let version = cursor.u8()?;
        cursor.take(3)?;
        if version == 0 {
            u32::from(cursor.u16()?)
        } else {
            cursor.u32()?
        }
    };
    let iprp = find(children, b"iprp")?;
    let ipco = find(iprp, b"ipco")?;
    let properties: Vec<([u8; 4], &[u8])> = boxes(ipco).collect();
    let ipma = find(iprp, b"ipma")?;
    let mut cursor = Cursor::new(ipma);
    let version = cursor.u8()?;
    let flags = cursor.take(3)?;
    let large_index = flags[2] & 1 == 1;
    let entries = cursor.u32()?;
    let mut info = HeifInfo {
        width: 0,
        height: 0,
        quarter_turns: 0,
    };
    for _ in 0..entries {
        let item = if version < 1 {
            u32::from(cursor.u16()?)
        } else {
            cursor.u32()?
        };
        let count = cursor.u8()?;
        for _ in 0..count {
            let index = if large_index {
                usize::from(cursor.u16()? & 0x7FFF)
            } else {
                usize::from(cursor.u8()? & 0x7F)
            };
            if item != primary || index == 0 {
                continue;
            }
            let Some((kind, body)) = properties.get(index - 1) else {
                continue;
            };
            match kind {
                b"ispe" => {
                    let mut property = Cursor::new(body);
                    property.take(4)?;
                    info.width = property.u32()?;
                    info.height = property.u32()?;
                }
                b"irot" => {
                    info.quarter_turns = body.first().copied().unwrap_or(0) & 0x3;
                }
                _ => {}
            }
        }
    }
    (info.width > 0 && info.height > 0).then_some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(body.len() + 8)
            .expect("size")
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn parses_primary_item_size_and_rotation() {
        let pitm = boxed(b"pitm", &[0, 0, 0, 0, 0, 1]);
        let mut ispe_body = vec![0, 0, 0, 0];
        ispe_body.extend_from_slice(&4032u32.to_be_bytes());
        ispe_body.extend_from_slice(&3024u32.to_be_bytes());
        let ispe = boxed(b"ispe", &ispe_body);
        let irot = boxed(b"irot", &[1]);
        let thumb_ispe = {
            let mut body = vec![0, 0, 0, 0];
            body.extend_from_slice(&320u32.to_be_bytes());
            body.extend_from_slice(&240u32.to_be_bytes());
            boxed(b"ispe", &body)
        };
        let ipco = boxed(b"ipco", &[ispe, irot, thumb_ispe].concat());
        // Two entries: item 1 -> props 1, 2; item 2 -> prop 3.
        let ipma = boxed(
            b"ipma",
            &[0, 0, 0, 0, 0, 0, 0, 2, 0, 1, 2, 0x81, 2, 0, 2, 1, 3],
        );
        let iprp = boxed(b"iprp", &[ipco, ipma].concat());
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(pitm);
        meta.extend(iprp);
        let info = parse_meta(&meta).expect("info");
        assert_eq!(
            (info.width, info.height, info.quarter_turns),
            (4032, 3024, 1)
        );
        assert_eq!(info.display_size(), (3024, 4032));
    }

    #[test]
    fn malformed_boxes_do_not_panic() {
        assert!(parse_meta(&[0, 0, 0, 0, 0, 0, 0, 200, b'p', b'i', b't', b'm']).is_none());
        assert!(parse_meta(&[]).is_none());
        assert_eq!(boxes(&[0, 0, 0, 4, 1, 2, 3, 4]).count(), 0);
    }
}
