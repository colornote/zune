//! Snapshot of `zune-rs/src/mtp.rs` (read-only copy — do NOT edit zune-rs from here).
//!
//! Provenance: re-synced 2026-10-15T13:16:15Z from ~/zune/zune-rs/src/mtp.rs
//! sha256: 4ecb1a3cfe9125573304f0d77fcf3962ab895e462f10e806a5a54a8d9f07a3fd
//! Paths rewritten: crate::{mtp,mtpz,usb,device,library,media,tags} -> crate::core::*.
//! Re-synced to pick up the upstream MTPZ CMAC fix (the previous hand-rolled
//! doubling step produced wrong MACs) and the library property-failure reporting.
//! MTP container layer as spoken by the Zune (little-endian, PTP-derived).
//!
//! Two Zune-specific quirks are load-bearing:
//!   1. a DATA container is written as **two** bulk transfers (header, then payload);
//!   2. a payload that is a whole multiple of 512 bytes (or empty) must be followed
//!      by a zero-length packet, otherwise the device keeps waiting for more data.

use anyhow::{bail, Context, Result};

pub const T_CMD: u16 = 1;
pub const T_DATA: u16 = 2;
pub const T_RESP: u16 = 3;

pub const C_OK: u16 = 0x2001;

// ── operation codes ──────────────────────────────────────────────────────────
pub const OP_GET_DEVICE_INFO: u16 = 0x1001;
pub const OP_OPEN_SESSION: u16 = 0x1002;
pub const OP_CLOSE_SESSION: u16 = 0x1003;
pub const OP_GET_STORAGE_IDS: u16 = 0x1004;
// ZUNE-GUI addition: capacity/usage of one storage id.
pub const OP_GET_STORAGE_INFO: u16 = 0x1005;
pub const OP_GET_OBJECT_HANDLES: u16 = 0x1007;
// ZUNE-GUI addition: download an object payload (cover art).
pub const OP_GET_OBJECT: u16 = 0x1009;
/// GetPartialObject: read a byte range. Cover art lives in an ID3 tag near the start
/// of an audio file, so this is the difference between 64 KB and 8 MB per album.
pub const OP_GET_PARTIAL_OBJECT: u16 = 0x101B;
// ZUNE-GUI addition: the thumbnail a device already has, when GetObject is refused.
pub const OP_GET_THUMB: u16 = 0x100A;
pub const OP_GET_OBJECT_INFO: u16 = 0x1008;
pub const OP_DELETE_OBJECT: u16 = 0x100B;
pub const OP_SEND_OBJECT_INFO: u16 = 0x100C;
pub const OP_SEND_OBJECT: u16 = 0x100D;
pub const OP_SET_DEVICE_PROP_VALUE: u16 = 0x1015;
pub const OP_SET_DEVICE_PROP: u16 = 0x1016;

// object properties (MTP 1.0 extension block)
pub const OP_GET_OBJECT_PROP: u16 = 0x9803; // GetObjectPropValue
pub const OP_SET_OBJECT_PROP: u16 = 0x9804; // SetObjectPropValue
pub const OP_GET_OBJECT_REFS: u16 = 0x9810; // GetObjectReferences
pub const OP_SET_OBJECT_REFS: u16 = 0x9811; // SetObjectReferences

// ZUNE-GUI addition: GetObjectPropList — "properties for many objects in one round
// trip", the difference between a 3-minute library read and a few seconds.
//
// On the *number*: libmtp (whose `ptp.c` sits in this repo) names 0x9805
// "MTP_GetObjPropList" and sends five parameters; the same number with two parameters
// is the single-property GetObjectPropValue this client already uses successfully
// against a Zune 30. Which reading a given firmware implements is therefore **checked
// at runtime** (the reply either parses as a property list or it does not) and the
// caller falls back — we never probe a Set-style opcode blindly.
pub const OP_GET_OBJECT_PROP_LIST: u16 = 0x9805;

/// Byte width of an MTP property data type, for parsing a property list value.
pub fn value_width(datatype: u16) -> Option<usize> {
    Some(match datatype {
        0x0001 | 0x0002 => 1, // int8 / uint8
        0x0003 | 0x0004 => 2, // int16 / uint16
        0x0005 | 0x0006 => 4, // int32 / uint32
        0x0007 | 0x0008 => 8, // int64 / uint64
        0x0009 | 0x000A => 16,
        _ => return None,
    })
}

/// One `{handle, property, value}` entry of a GetObjectPropList response.
#[derive(Debug, Clone)]
pub struct PropEntry {
    pub handle: u32,
    pub property: u16,
    pub text: Option<String>,
    pub number: Option<u32>,
}

/// Parse a GetObjectPropList dataset: a u32 count, then that many entries.
pub fn parse_prop_list(d: &[u8]) -> Result<Vec<PropEntry>> {
    let n = u32_at(d, 0)? as usize;
    let mut o = 4usize;
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let handle = u32_at(d, o)?;
        o += 4;
        let property = u16_at(d, o)?;
        o += 2;
        let datatype = u16_at(d, o)?;
        o += 2;
        if datatype == 0xFFFF {
            let (text, next) = dec_str(d, o)?;
            o = next;
            out.push(PropEntry { handle, property, text: Some(text), number: None });
        } else {
            let width = value_width(datatype)
                .with_context(|| format!("unknown property data type {datatype:#06x}"))?;
            let bytes = d.get(o..o + width).context("truncated property value")?;
            let mut v = [0u8; 4];
            v[..width.min(4)].copy_from_slice(&bytes[..width.min(4)]);
            o += width;
            out.push(PropEntry {
                handle,
                property,
                text: None,
                number: Some(u32::from_le_bytes(v)),
            });
        }
    }
    Ok(out)
}

// MTPZ / WMDRM-PD
pub const OP_SEND_WMDRM_PD_REQ: u16 = 0x9212;
pub const OP_GET_WMDRM_PD_RESP: u16 = 0x9213;
pub const OP_ENABLE_TRUSTED_OPS: u16 = 0x9214;
pub const OP_END_TRUSTED_APP_SESSION: u16 = 0x9216;

// Zune vendor block: WiFi control surface (from the Keel 3.30 firmware, MtpSvc.dll)
pub const OP_WIFI_SCAN: u16 = 0x9225; // -> ZNetScanAndWaitForNetworks
pub const OP_WIFI_SET_PROFILE: u16 = 0x9227; // -> ZNetSetWifiProfile + ZNetAddWifiProfile
pub const OP_WIFI_TEST_CONNECTIVITY: u16 = 0x9228; // -> ZNetTestConnectivity

// device properties
pub const DEVPROP_SESSION_INITIATOR: u16 = 0xD406;

// object formats
pub mod fmt {
    pub const ASSOCIATION: u16 = 0x3001;
    pub const MP3: u16 = 0x3009;
    pub const JPEG: u16 = 0x3801;
    pub const COVER_ART: u16 = 0x3802;
    pub const WMA: u16 = 0xB901;
    pub const AAC: u16 = 0xB903;
    pub const WMV: u16 = 0xB981;
    pub const MP4: u16 = 0xB982;
    pub const ABSTRACT_ALBUM: u16 = 0xBA03;
    pub const PLAYLIST: u16 = 0xBA05;
}

pub mod prop {
    /// ZUNE-GUI addition: ObjectSize (0xDC04) — the bulk sweep carries the size, so the
    /// fast path needs no GetObjectInfo per object.
    pub const SIZE: u16 = 0xDC04;
    pub const OBJECT_REFS: u16 = 0xDC41;
    pub const NAME: u16 = 0xDC44;
    pub const ARTIST: u16 = 0xDC46;
    pub const TRACK: u16 = 0xDC8B;
    pub const ALBUM: u16 = 0xDC9A;
    /// ZUNE-GUI addition: Duration, **in milliseconds** (MTP spec). A Zune 30 really
    /// does report e.g. 257969 for a 4:18 track — formatting that as seconds produced
    /// "4299:29" and a 791-hour album.
    pub const DURATION: u16 = 0xDC89;
    /// ZUNE-GUI additions: the date properties an album may carry. The base MTP
    /// codes are tried first, then the vendor-ish one; whichever answers is cached.
    pub const DATE_CREATED: u16 = 0xDC09;
    pub const DATE_MODIFIED: u16 = 0xDC0A;
    pub const DATE_ADDED: u16 = 0xDC99;
    /// AlbumArtist — not in the base MTP spec, but the Zune accepts it and uses it
    /// for album grouping in Music → Albums.
    pub const ALBUM_ARTIST: u16 = 0xDC9B;
}

// ── data type helpers ────────────────────────────────────────────────────────

/// Encode an MTP string: u8 char count including the NUL, then UTF-16LE.
pub fn enc_str(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::with_capacity(1 + (units.len() + 1) * 2);
    out.push((units.len() + 1) as u8);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Decode an MTP string; returns the string and the offset just past it.
pub fn dec_str(data: &[u8], off: usize) -> Result<(String, usize)> {
    let n = *data.get(off).context("string: truncated length byte")? as usize;
    if n == 0 {
        return Ok((String::new(), off + 1));
    }
    let end = off + 1 + n * 2;
    let raw = data.get(off + 1..end).context("string: truncated body")?;
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut s = String::from_utf16_lossy(&units);
    if s.ends_with('\0') {
        s.pop();
    }
    Ok((s, end))
}

/// Name of an operation code we may send, so failures name the step.
pub fn op_name(op: u16) -> &'static str {
    match op {
        OP_GET_DEVICE_INFO => "GetDeviceInfo",
        OP_OPEN_SESSION => "OpenSession",
        OP_CLOSE_SESSION => "CloseSession",
        OP_GET_STORAGE_IDS => "GetStorageIDs",
        OP_GET_STORAGE_INFO => "GetStorageInfo",
        OP_GET_OBJECT => "GetObject",
        OP_GET_THUMB => "GetThumb",
        OP_GET_OBJECT_HANDLES => "GetObjectHandles",
        OP_GET_OBJECT_INFO => "GetObjectInfo",
        OP_DELETE_OBJECT => "DeleteObject",
        OP_SEND_OBJECT_INFO => "SendObjectInfo",
        OP_SEND_OBJECT => "SendObject",
        OP_SET_DEVICE_PROP => "SetDevicePropValue",
        OP_SET_OBJECT_PROP => "SetObjectPropValue",
        OP_GET_OBJECT_PROP => "GetObjectPropValue",
        OP_GET_OBJECT_REFS => "GetObjectReferences",
        OP_SET_OBJECT_REFS => "SetObjectReferences",
        OP_SEND_WMDRM_PD_REQ => "SendWMDRMPDReq (MTPZ)",
        OP_GET_WMDRM_PD_RESP => "GetWMDRMPDResp (MTPZ)",
        OP_ENABLE_TRUSTED_OPS => "EnableTrustedOps (MTPZ)",
        OP_END_TRUSTED_APP_SESSION => "EndTrustedAppSession (MTPZ)",
        OP_WIFI_SCAN => "wifi scan (0x9225)",
        OP_WIFI_SET_PROFILE => "wifi set-profile (0x9227)",
        OP_WIFI_TEST_CONNECTIVITY => "wifi test (0x9228)",
        _ => "unknown",
    }
}

/// Name of a response code, so an error reads like a diagnosis.
pub fn response_name(code: u16) -> &'static str {
    match code {
        0x2000 => "Undefined",
        C_OK => "OK",
        0x2002 => "GeneralError",
        0x2003 => "SessionNotOpen",
        0x2004 => "InvalidTransactionID",
        0x2005 => "OperationNotSupported",
        0x2006 => "ParameterNotSupported",
        0x2007 => "IncompleteTransfer",
        0x2008 => "InvalidStorageID",
        0x2009 => "InvalidObjectHandle",
        0x200A => "DevicePropNotSupported",
        0x200B => "InvalidObjectFormatCode",
        0x200C => "StoreFull",
        0x200D => "ObjectWriteProtected",
        0x200E => "StoreReadOnly",
        0x200F => "AccessDenied",
        0x2010 => "NoThumbnail",
        0x2011 => "SelfTestFailed",
        0x2012 => "PartialDeletion",
        0x2013 => "StoreNotAvailable",
        0x2014 => "SpecificationByFormatUnsupported",
        0x2015 => "NoValidObjectInfo",
        0x2016 => "InvalidCodec",
        0x2017 => "InvalidParentObject",
        0x2018 => "InvalidDevicePropFormat",
        0x2019 => "InvalidDevicePropValue",
        0x201A => "InvalidParameter",
        0x201B => "SessionAlreadyOpen",
        0x201C => "TransactionCancelled",
        0x201D => "SpecificationOfDestinationUnsupported",
        _ => "unknown",
    }
}

pub fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(d.get(o..o + 2).context("u16 out of range")?.try_into()?))
}
// ZUNE-GUI addition (StorageInfo carries 64-bit capacities).
pub fn u64_at(d: &[u8], o: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(d.get(o..o + 8).context("u64 out of range")?.try_into()?))
}
pub fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("u32 out of range")?.try_into()?))
}

/// List of u32 as carried in MTP arrays (leading count).
pub fn parse_u32_array(d: &[u8]) -> Result<Vec<u32>> {
    let n = u32_at(d, 0)? as usize;
    (0..n).map(|i| u32_at(d, 4 + i * 4)).collect()
}

// ── containers ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Container {
    pub ctype: u16,
    pub code: u16,
    pub txid: u32,
    pub params: Vec<u32>,
    pub data: Vec<u8>,
}

impl Container {
    pub fn cmd(code: u16, txid: u32, params: &[u32]) -> Vec<u8> {
        build(T_CMD, code, txid, params)
    }
    /// A DATA container header. The declared length covers the **payload**, so this
    /// must not go through `build()`, which sizes containers by their parameter
    /// count — doing that declared 12 bytes for a container carrying 71 and made the
    /// device cancel the transaction with 0x201C (TransactionCancelled).
    pub fn data_header(code: u16, txid: u32, payload_len: usize) -> Vec<u8> {
        let mut head = Vec::with_capacity(12);
        head.extend_from_slice(&((12 + payload_len) as u32).to_le_bytes());
        head.extend_from_slice(&T_DATA.to_le_bytes());
        head.extend_from_slice(&code.to_le_bytes());
        head.extend_from_slice(&txid.to_le_bytes());
        head
    }
    pub fn data(code: u16, txid: u32, payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
        (Self::data_header(code, txid, payload.len()), payload.to_vec())
    }
    pub fn parse(buf: &[u8]) -> Result<Container> {
        let total = u32_at(buf, 0)? as usize;
        if total < 12 || total > buf.len() {
            bail!("bad container length {total} (buffer {})", buf.len());
        }
        let ctype = u16_at(buf, 4)?;
        let code = u16_at(buf, 6)?;
        let txid = u32_at(buf, 8)?;
        let mut params = Vec::new();
        let mut off = 12;
        while off + 4 <= total {
            params.push(u32_at(buf, off)?);
            off += 4;
        }
        Ok(Container { ctype, code, txid, params, data: buf[12..total].to_vec() })
    }
    /// Response parameter n, when the device reports them inline.
    pub fn param(&self, n: usize) -> Option<u32> {
        self.params.get(n).copied()
    }
}

fn build(ctype: u16, code: u16, txid: u32, params: &[u32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + params.len() * 4);
    v.extend_from_slice(&((12 + params.len() * 4) as u32).to_le_bytes());
    v.extend_from_slice(&ctype.to_le_bytes());
    v.extend_from_slice(&code.to_le_bytes());
    v.extend_from_slice(&txid.to_le_bytes());
    for p in params {
        v.extend_from_slice(&p.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DATA container's declared length must cover the payload. Sizing it like a
    /// command (by parameter count) makes the device cancel the transaction.
    #[test]
    fn data_header_declares_the_payload_length() {
        let payload = vec![0u8; 71];
        let (head, body) = Container::data(OP_SET_DEVICE_PROP, 7, &payload);
        assert_eq!(head.len(), 12);
        assert_eq!(u32_at(&head, 0).unwrap() as usize, 12 + payload.len());
        assert_eq!(u16_at(&head, 4).unwrap(), T_DATA);
        assert_eq!(u16_at(&head, 6).unwrap(), OP_SET_DEVICE_PROP);
        assert_eq!(u32_at(&head, 8).unwrap(), 7);
        assert_eq!(body.len(), payload.len());

        let empty = Container::data(OP_SEND_OBJECT, 3, &[]);
        assert_eq!(u32_at(&empty.0, 0).unwrap(), 12);
    }

    /// Command containers are sized by their parameters, and must stay that way.
    #[test]
    fn command_header_declares_the_parameter_count() {
        let cmd = Container::cmd(OP_SET_DEVICE_PROP, 1, &[0xD406]);
        assert_eq!(u32_at(&cmd, 0).unwrap(), 16);
        assert_eq!(u16_at(&cmd, 4).unwrap(), T_CMD);
        assert_eq!(u32_at(&cmd, 12).unwrap(), 0xD406);
    }

    /// An MTP string is a u8 character count that includes the terminating NUL,
    /// followed by UTF-16LE — and it round-trips.
    #[test]
    fn strings_round_trip() {
        for s in ["libmtp/Sajid Anwar - MTPZClassDriver", "", "Kid A"] {
            let enc = enc_str(s);
            assert_eq!(enc[0] as usize * 2 + 1, enc.len());
            let (back, used) = dec_str(&enc, 0).unwrap();
            assert_eq!(back, s);
            assert_eq!(used, enc.len());
        }
    }
}
