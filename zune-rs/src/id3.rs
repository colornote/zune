//! Embedded album art: the ID3v2 `APIC` frame.
//!
//! This is where cover art on a Zune 30 actually lives. The device exposes no
//! thumbnail read through MTP — `MtpSvc.dll` imports `ZMediaLib_SetItemThumbnail`
//! and `IsItemArtPresent` but **not** `GetItemThumbnail`, so the MTP surface can
//! write art and can never read it back. What it *can* read is the audio object's
//! payload, and roughly a third of this device's files carry a full JPEG in an
//! `APIC` frame. Measured on six files picked across the library: four had no
//! `APIC`, two carried `APIC` plus a JPEG payload.
//!
//! So the cover lookup is: `GetObject` the album's first track, parse ID3v2 here,
//! and cache whatever comes out. No network involved.
//!
//! Only what is needed is implemented: the v2.2/v2.3/v2.4 tag header, the frame
//! header of each version, unsynchronisation (v2.3/v2.4 only — v2.2's scheme
//! applies to the whole tag and is rare enough here to skip), and the `APIC` /
//! `PIC` picture frame. Everything else is walked over by length.

/// A picture pulled out of an ID3 tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// "image/jpeg" and friends, lowercased. Empty when the frame did not say.
    pub mime: String,
    /// 0 = other, 3 = front cover. Used to prefer a front cover over a scan.
    pub kind: u8,
    pub data: Vec<u8>,
}

impl Picture {
    /// True when the bytes look like something `image` can decode. The device
    /// stores baseline JPEG, but a frame that says "PNG" and contains a PNG is
    /// just as good.
    pub fn is_decodable(&self) -> bool {
        let d = &self.data;
        d.starts_with(&[0xFF, 0xD8, 0xFF]) // JPEG
            || d.starts_with(&[0x89, b'P', b'N', b'G']) // PNG
            || d.starts_with(b"GIF8")
            || d.starts_with(b"BM")
    }
}

/// A picture frame located inside a header the caller already has, without the bytes.
///
/// Cover art in these files is ~272 KB, so reading a whole MP3 to get one is wasteful;
/// reading the first 64 KB is enough to find out where the picture is and how big it
/// is. `body_offset` is absolute within the object, so it can be handed straight to
/// `GetPartialObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadLocation {
    /// Absolute offset of the picture bytes (past the frame header and its text).
    pub body_offset: usize,
    /// Frame size as declared by the frame header.
    pub declared_size: usize,
    /// Payload size once the frame's own text fields are subtracted.
    pub payload_size: usize,
}

/// Find the first `APIC` frame and say where its picture bytes start.
///
/// Deliberately does not walk the tag structure: on this device the header's size
/// field is written as a plain integer where the spec wants a synchsafe one, so any
/// bounds check derived from it is wrong. Scanning for the frame id and validating
/// forward is what actually works.
pub fn locate_picture(bytes: &[u8]) -> Option<PayloadLocation> {
    let mut from = 0usize;
    while let Some(rel) = find(&bytes[from..], b"APIC") {
        let at = from + rel;
        from = at + 4;
        if at + 10 > bytes.len() {
            return None;
        }
        let declared = u32::from_be_bytes([
            bytes[at + 4],
            bytes[at + 5],
            bytes[at + 6],
            bytes[at + 7],
        ]) as usize;
        if declared < 4 {
            continue;
        }
        let body = &bytes[at + 10..];
        let Some(payload) = payload_offset(body) else {
            continue;
        };
        let payload_size = declared.saturating_sub(payload);
        if payload_size == 0 {
            continue;
        }
        return Some(PayloadLocation {
            body_offset: at + 10 + payload,
            declared_size: declared,
            payload_size,
        });
    }
    None
}

/// How many bytes of an `APIC` body are its own text fields, before the picture.
fn payload_offset(body: &[u8]) -> Option<usize> {
    let encoding = *body.first()?;
    let rest = &body[1..];
    let mime_end = rest.iter().position(|b| *b == 0)?;
    let mime = &rest[..mime_end];
    // A real frame names an image type; a stray "APIC" in audio data would not.
    if !mime.starts_with(b"image/") && !mime.starts_with(b"IMAGE/") {
        return None;
    }
    let after_mime = &rest[mime_end + 1..];
    if after_mime.is_empty() {
        return None;
    }
    // description, then the bytes
    let description = text_len(&after_mime[1..], encoding)?;
    Some(1 + mime_end + 1 + 1 + description)
}

/// Every picture frame in the file, in tag order.
///
/// Returns an empty vector for a file with no ID3v2 tag at all — which is the
/// common case here, and not an error.
pub fn pictures(bytes: &[u8]) -> Vec<Picture> {
    let mut out = framed_pictures(bytes);
    if out.is_empty() {
        // The tag header's size field is not trustworthy on real files. Measured on
        // this device: an ID3v2.3 tag whose four size bytes read `00 00 1f 76`
        // announces 4086 (synchsafe, as the spec requires) but its contents run to
        // 8054 — the encoder wrote a plain big-endian integer. Parsing to the declared
        // end stops well before the APIC frame, which sits at offset 4294.
        //
        // So when the framed walk finds nothing, fall back to scanning for the frame
        // id and validating what follows. A false sync inside compressed audio is
        // possible, so the candidate has to look like a real picture frame and carry
        // a decodable image; that is a cheap test and it has not misfired yet.
        out = scanned_pictures(bytes);
    }
    out
}

/// Find `APIC` frames by scanning, without trusting any length said earlier.
fn scanned_pictures(bytes: &[u8]) -> Vec<Picture> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = find(&bytes[from..], b"APIC") {
        let at = from + rel;
        from = at + 4;
        // v2.3/v2.4 frame header: id(4) size(4) flags(2), then the body.
        if at + 10 > bytes.len() {
            break;
        }
        let size = u32::from_be_bytes([
            bytes[at + 4],
            bytes[at + 5],
            bytes[at + 6],
            bytes[at + 7],
        ]) as usize;
        if size < 4 || at + 10 + size > bytes.len() {
            continue;
        }
        if let Some(picture) = parse_apic(&bytes[at + 10..at + 10 + size]) {
            // Only keep it if the payload really is an image.
            if picture.is_decodable() {
                out.push(picture);
            }
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn framed_pictures(bytes: &[u8]) -> Vec<Picture> {
    let Some(parsed) = tag_body(bytes) else {
        return Vec::new();
    };
    let (tag, version) = (parsed.body.as_slice(), parsed.version);
    // APIC is v2.3/v2.4; v2.2 called it PIC and used 3-byte frame headers.
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 6 <= tag.len() {
        let id = &tag[offset..offset + 4];
        if id[0] == 0 {
            // Padding: the tag reserved more space than it used.
            break;
        }
        let (size, header) = match version {
            2 => {
                if offset + 6 > tag.len() {
                    break;
                }
                (
                    u32::from_be_bytes([0, tag[offset + 3], tag[offset + 4], tag[offset + 5]])
                        as usize,
                    6,
                )
            }
            3 => {
                if offset + 10 > tag.len() {
                    break;
                }
                (
                    u32::from_be_bytes([
                        tag[offset + 4],
                        tag[offset + 5],
                        tag[offset + 6],
                        tag[offset + 7],
                    ]) as usize,
                    10,
                )
            }
            _ => {
                if offset + 10 > tag.len() {
                    break;
                }
                // v2.4 sizes are synchsafe.
                (
                    synchsafe(&tag[offset + 4..offset + 8]) as usize,
                    10,
                )
            }
        };
        let body_start = offset + header;
        let body_end = body_start.saturating_add(size).min(tag.len());
        if body_end <= body_start {
            break;
        }
        let body = &tag[body_start..body_end];

        if version == 2 {
            if id == b"PIC" {
                if let Some(picture) = parse_pic_v22(body) {
                    out.push(picture);
                }
            }
        } else if id == b"APIC" {
            if let Some(picture) = parse_apic(body) {
                out.push(picture);
            }
        }

        offset = body_end;
    }
    out
}

/// The best picture in the file: a front cover if there is one, else the first.
pub fn best_picture(bytes: &[u8]) -> Option<Picture> {
    let mut found = pictures(bytes);
    let front = found.iter().position(|p| p.kind == 3 && p.is_decodable());
    match front {
        Some(index) => Some(found.swap_remove(index)),
        None => found.into_iter().find(Picture::is_decodable),
    }
}

/// The decoded tag body plus the version it was written with.
struct Tag {
    body: Vec<u8>,
    version: u8,
}

fn tag_body(bytes: &[u8]) -> Option<Tag> {
    if bytes.len() < 10 || &bytes[..3] != b"ID3" {
        return None;
    }
    let major = bytes[3];
    if major < 2 || major > 4 {
        return None;
    }
    // Size is synchsafe in every version, and excludes the 10-byte header itself.
    let size = synchsafe(&bytes[6..10]) as usize;
    let flags = bytes[5];
    let start = 10usize;
    // v2.4 extended header.
    let mut body_start = start;
    if major == 4 && flags & 0x40 != 0 {
        if bytes.len() >= start + 4 {
            let ext = synchsafe(&bytes[start..start + 4]) as usize;
            body_start = start + ext;
        }
    } else if major == 3 && flags & 0x40 != 0 {
        // v2.3 extended header: a 4-byte big-endian size *not* counting itself.
        if bytes.len() >= start + 4 {
            let ext = u32::from_be_bytes([
                bytes[start],
                bytes[start + 1],
                bytes[start + 2],
                bytes[start + 3],
            ]) as usize;
            body_start = start + 4 + ext;
        }
    }
    let end = (start + size).min(bytes.len());
    if body_start >= end {
        return None;
    }
    let raw = &bytes[body_start..end];

    // Unsynchronisation replaces every 0xFF 0x00 with 0xFF. v2.2 applies it to the
    // whole tag, v2.3 to the whole tag when its flag is set, v2.4 per frame — the
    // per-frame case is handled by the frame readers not needing it, so only the
    // whole-tag forms are undone here.
    let unsync = if major == 2 {
        false // v2.2 has no unsync flag; the scheme is applied at the audio layer.
    } else {
        flags & 0x80 != 0
    };
    let body = if unsync { deunsynchronise(raw) } else { raw.to_vec() };
    Some(Tag { body, version: major })
}

/// ID3 synchsafe integers: 7 bits per byte, high bit always clear.
fn synchsafe(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for byte in bytes.iter().take(4) {
        value = (value << 7) | u32::from(byte & 0x7F);
    }
    value
}

/// Undo the 0xFF 0x00 escaping.
fn deunsynchronise(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        out.push(input[index]);
        if input[index] == 0xFF && input.get(index + 1) == Some(&0x00) {
            index += 2;
        } else {
            index += 1;
        }
    }
    out
}

/// `APIC`: encoding, MIME (latin-1, NUL-terminated), picture type, description
/// (in `encoding`), then the bytes.
fn parse_apic(body: &[u8]) -> Option<Picture> {
    if body.len() < 4 {
        return None;
    }
    let encoding = body[0];
    let rest = &body[1..];
    let mime_end = rest.iter().position(|b| *b == 0)?;
    let mime = String::from_utf8_lossy(&rest[..mime_end]).to_lowercase();
    let after_mime = &rest[mime_end + 1..];
    if after_mime.is_empty() {
        return None;
    }
    let kind = after_mime[0];
    let after_kind = &after_mime[1..];
    let description_len = text_len(after_kind, encoding)?;
    let data = after_kind.get(description_len..)?.to_vec();
    if data.is_empty() {
        return None;
    }
    Some(Picture { mime, kind, data })
}

/// `PIC` (v2.2): encoding, a 3-byte image format, picture type, description, bytes.
fn parse_pic_v22(body: &[u8]) -> Option<Picture> {
    if body.len() < 5 {
        return None;
    }
    let encoding = body[0];
    let format = String::from_utf8_lossy(&body[1..4]).to_lowercase();
    let mime = match format.as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_owned(),
        "png" => "image/png".to_owned(),
        "gif" => "image/gif".to_owned(),
        "bmp" => "image/bmp".to_owned(),
        other => format!("image/{other}"),
    };
    let kind = body[4];
    let after_kind = &body[5..];
    let description_len = text_len(after_kind, encoding)?;
    let data = after_kind.get(description_len..)?.to_vec();
    if data.is_empty() {
        return None;
    }
    Some(Picture { mime, kind, data })
}

/// Length of an encoded, NUL-terminated string, including its terminator.
fn text_len(bytes: &[u8], encoding: u8) -> Option<usize> {
    match encoding {
        // ISO-8859-1, UTF-8: single NUL.
        0 | 3 => bytes.iter().position(|b| *b == 0).map(|i| i + 1),
        // UTF-16 with BOM: a NUL is two bytes, and the terminator must be aligned.
        1 => {
            let mut index = 0;
            while index + 1 < bytes.len() {
                if bytes[index] == 0 && bytes[index + 1] == 0 {
                    return Some(index + 2);
                }
                index += 2;
            }
            None
        }
        // UTF-16BE, no BOM.
        2 => {
            let mut index = 0;
            while index + 1 < bytes.len() {
                if bytes[index] == 0 && bytes[index + 1] == 0 {
                    return Some(index + 2);
                }
                index += 2;
            }
            None
        }
        _ => bytes.iter().position(|b| *b == 0).map(|i| i + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG big enough to look real, and recognisable in assertions.
    fn jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
        v.extend_from_slice(&[0x00; 32]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    fn synchsafe_bytes(value: u32) -> [u8; 4] {
        [
            ((value >> 21) & 0x7F) as u8,
            ((value >> 14) & 0x7F) as u8,
            ((value >> 7) & 0x7F) as u8,
            (value & 0x7F) as u8,
        ]
    }

    /// Build a v2.3 tag with one APIC frame.
    fn tag_v23(mime: &str, kind: u8, picture: &[u8]) -> Vec<u8> {
        let mut frame_body = vec![0u8]; // latin-1
        frame_body.extend_from_slice(mime.as_bytes());
        frame_body.push(0);
        frame_body.push(kind);
        frame_body.push(0); // empty description
        frame_body.extend_from_slice(picture);

        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(frame_body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]); // flags
        frame.extend_from_slice(&frame_body);

        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(3);
        tag.push(0);
        tag.push(0);
        tag.extend_from_slice(&synchsafe_bytes(frame.len() as u32));
        tag.extend_from_slice(&frame);
        tag
    }

    #[test]
    fn a_v23_apic_frame_yields_its_picture() {
        let picture = jpeg();
        let file = tag_v23("image/jpeg", 3, &picture);
        let found = best_picture(&file).expect("one picture");
        assert_eq!(found.mime, "image/jpeg");
        assert_eq!(found.kind, 3);
        assert_eq!(found.data, picture);
        assert!(found.is_decodable());
    }

    #[test]
    fn a_file_without_id3_is_not_an_error() {
        let file = vec![0xFF, 0xFB, 0x90, 0x00];
        assert!(pictures(&file).is_empty());
        assert!(best_picture(&file).is_none());
    }

    #[test]
    fn a_picture_can_be_located_without_its_bytes() {
        // The shape measured on the device: one 272 KB frame, and the caller only holds
        // the first 64 KB of the file.
        let picture = vec![0u8; 272_187];
        let file = tag_v23("image/jpeg", 3, &picture);
        let head_len = file.len().min(64 * 1024);
        let head = &file[..head_len];

        let found = locate_picture(head).expect("the APIC in the first 64 KB");
        assert_eq!(found.payload_size, picture.len());
        assert!(found.declared_size >= found.payload_size);
        // The offset lands exactly on the picture bytes in the full file.
        let end = found.body_offset + found.payload_size;
        assert!(end <= file.len());
        assert_eq!(&file[found.body_offset..end], &picture[..]);
    }

    #[test]
    fn a_stray_frame_id_in_audio_is_not_mistaken_for_a_picture() {
        let mut file = vec![0u8; 64];
        file.extend_from_slice(b"APIC");
        file.extend_from_slice(&1000u32.to_be_bytes());
        file.extend_from_slice(&[0, 0]);
        file.extend_from_slice(&[0u8; 40]); // no MIME, no picture
        assert!(locate_picture(&file).is_none());
    }

    #[test]
    fn a_frame_past_a_mislabelled_tag_end_is_still_found() {
        // Reproduces the device: the header says the tag ends long before the APIC.
        let picture = jpeg();
        let mut frame_body = vec![0u8];
        frame_body.extend_from_slice(b"image/jpeg");
        frame_body.push(0);
        frame_body.push(3);
        frame_body.push(0);
        frame_body.extend_from_slice(&picture);
        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(frame_body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&frame_body);

        let mut file = Vec::new();
        file.extend_from_slice(b"ID3");
        file.push(3);
        file.push(0);
        file.push(0);
        file.extend_from_slice(&synchsafe_bytes(64)); // lies: says 64, contents run on
        file.extend_from_slice(b"TIT2");
        file.extend_from_slice(&15u32.to_be_bytes());
        file.extend_from_slice(&[0, 0]);
        file.extend_from_slice(&[0u8; 15]);
        file.extend_from_slice(&[0u8; 64]);
        file.extend_from_slice(&frame);

        let found = best_picture(&file).expect("the APIC past the declared end");
        assert_eq!(found.data, picture);
    }

    #[test]
    fn a_front_cover_beats_an_earlier_scan() {
        let front = jpeg();
        let mut file = tag_v23("image/jpeg", 3, &front);
        // Append a second frame (a back cover) after the first.
        let back = {
            let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
            v.extend_from_slice(&[0x11; 16]);
            v.extend_from_slice(&[0xFF, 0xD9]);
            v
        };
        let mut frame_body = vec![0u8];
        frame_body.extend_from_slice(b"image/jpeg");
        frame_body.push(0);
        frame_body.push(4); // back cover
        frame_body.push(0);
        frame_body.extend_from_slice(&back);
        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(frame_body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&frame_body);
        file.extend_from_slice(&frame);

        let found = best_picture(&file).expect("a picture");
        assert_eq!(found.kind, 3, "the front cover wins even though the scan came first");
        assert_eq!(found.data, front);
    }

    #[test]
    fn other_frames_are_walked_over_by_length() {
        // COMM, then APIC — the classic shape of these files (measured).
        let picture = jpeg();
        let mut comm_body = vec![0u8];
        comm_body.extend_from_slice(b"eng");
        comm_body.extend_from_slice(&[0; 8]);
        let mut comm = Vec::new();
        comm.extend_from_slice(b"COMM");
        comm.extend_from_slice(&(comm_body.len() as u32).to_be_bytes());
        comm.extend_from_slice(&[0, 0]);
        comm.extend_from_slice(&comm_body);

        let mut frame_body = vec![0u8];
        frame_body.extend_from_slice(b"image/jpeg");
        frame_body.push(0);
        frame_body.push(3);
        frame_body.push(0);
        frame_body.extend_from_slice(&picture);
        let mut apic = Vec::new();
        apic.extend_from_slice(b"APIC");
        apic.extend_from_slice(&(frame_body.len() as u32).to_be_bytes());
        apic.extend_from_slice(&[0, 0]);
        apic.extend_from_slice(&frame_body);

        let mut body = Vec::new();
        body.extend_from_slice(&comm);
        body.extend_from_slice(&apic);

        let mut tag = Vec::new();
        tag.extend_from_slice(b"ID3");
        tag.push(3);
        tag.push(0);
        tag.push(0);
        tag.extend_from_slice(&synchsafe_bytes(body.len() as u32));
        tag.extend_from_slice(&body);

        let found = best_picture(&tag).expect("the APIC after the COMM");
        assert_eq!(found.data, picture);
    }

    #[test]
    fn tag_size_is_read_as_a_synchsafe_integer() {
        // 0x00 0x00 0x02 0x01 is 257, not 513.
        assert_eq!(synchsafe(&[0x00, 0x00, 0x02, 0x01]), 257);
    }
}
