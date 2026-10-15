//! WiFi scan results, as the Zune 30 reports them.
//!
//! Opcode `0x9225` (Keel 3.30 firmware, MtpSvc) answers a raw byte blob with no header of
//! its own. Measured against a real scan of seven networks, every record starts on a
//! 56-byte boundary and has this shape:
//!
//! ```text
//!   [0..4]   signal strength, little-endian u32, in the range 0..=100
//!   [4..8]   SSID length in bytes, little-endian u32
//!   [8..8+n] SSID, UTF-8 (the device stores "天空" as three-byte characters)
//!   ...      zero padding out to 56
//! ```
//!
//! Reading it as a fixed record rather than scanning for plausible headers is what keeps
//! an SSID that starts with a zero byte from being mistaken for a header — and the
//! observed offsets (0, 56, 112, 168, 224, 280, 336) agree with it exactly.

/// One network the device can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    /// Signal strength as the device reports it: 0..=100, its own scale, not dBm.
    pub strength: u8,
    pub ssid: String,
}

impl WifiNetwork {
    /// How many solid bars to draw, 1..=4. Anything visible gets one.
    pub fn bars(&self) -> usize {
        match self.strength {
            0..=24 => 1,
            25..=49 => 2,
            50..=74 => 3,
            _ => 4,
        }
    }

    /// A word for the strength, so the number is not the only signal.
    pub fn quality(&self) -> &'static str {
        match self.strength {
            0..=24 => "weak",
            25..=49 => "fair",
            50..=74 => "good",
            _ => "strong",
        }
    }
}

/// The record stride this firmware uses: 8 bytes of header plus a 48-byte SSID area.
const RECORD: usize = 56;
/// The SSID area inside a record, i.e. what is left after the header.
const SSID_AREA: usize = RECORD - 8;
/// The longest SSID 802.11 allows.
const MAX_SSID: usize = 32;

/// Parse a `0x9225` response into the networks it describes.
///
/// Malformed records are skipped rather than rejected: a scan that returns six good
/// networks and one garbled one is still worth showing, and the alternative — a parse
/// error the UI has to explain — helps nobody.
pub fn parse_scan(bytes: &[u8]) -> Vec<WifiNetwork> {
    let mut out = Vec::new();
    for chunk in bytes.chunks_exact(RECORD) {
        let strength = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let length = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;
        if strength > 100 || length == 0 || length > MAX_SSID || length > SSID_AREA {
            continue;
        }
        let raw = &chunk[8..8 + length];
        // An SSID is a byte string, not guaranteed UTF-8. Lossy decoding keeps a
        // half-broken name on screen instead of silently dropping the network.
        let ssid = String::from_utf8_lossy(raw).trim_end_matches('\0').trim().to_owned();
        if ssid.is_empty() {
            continue;
        }
        out.push(WifiNetwork { strength: strength as u8, ssid });
    }
    // Strongest first: the one you are looking for is usually the one you are standing next to.
    out.sort_by(|a, b| b.strength.cmp(&a.strength));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a record the way the device does, padding to the stride.
    fn record(strength: u32, ssid: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECORD);
        out.extend_from_slice(&strength.to_le_bytes());
        out.extend_from_slice(&(ssid.len() as u32).to_le_bytes());
        out.extend_from_slice(ssid.as_bytes());
        out.resize(RECORD, 0);
        out
    }

    #[test]
    fn a_real_scan_parses_into_networks() {
        // The shape of a live scan: seven records, one of them a UTF-8 name.
        let mut blob = Vec::new();
        blob.extend(record(7, "ExampleNet-1"));
        blob.extend(record(50, "44444"));
        blob.extend(record(30, "天空"));
        blob.extend(record(75, "ExampleNet-2"));
        blob.extend(record(95, "ExampleNet-3"));

        let found = parse_scan(&blob);
        assert_eq!(found.len(), 5);
        // Sorted strongest first.
        assert_eq!(found[0].ssid, "ExampleNet-3");
        assert_eq!(found[0].strength, 95);
        assert_eq!(found[0].bars(), 4);
        assert_eq!(found[0].quality(), "strong");
        // A non-ASCII name survives.
        assert!(found.iter().any(|n| n.ssid == "天空"));
        assert_eq!(found.last().unwrap().ssid, "ExampleNet-1");
        assert_eq!(found.last().unwrap().bars(), 1);
    }

    #[test]
    fn a_leading_zero_byte_is_not_mistaken_for_a_header() {
        // The record stride is what disambiguates: an SSID beginning with NUL keeps the
        // next record aligned, where a header-scanning parser would lose its place.
        let mut blob = record(60, "abc");
        // Overwrite the SSID's first byte with a zero the way a padded name would.
        blob[8] = 0;
        blob.extend(record(40, "next-net"));
        let found = parse_scan(&blob);
        assert!(found.iter().any(|n| n.ssid == "next-net"), "{found:?}");
    }

    #[test]
    fn nonsense_records_are_skipped_not_fatal() {
        // A strength above 100 and an impossible length both mean "not a record".
        let mut blob = record(255, "bogus");
        blob.extend(record(0, ""));       // zero-length: no SSID to show
        blob.extend(record(80, "real"));
        let found = parse_scan(&blob);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ssid, "real");
    }

    #[test]
    fn an_empty_or_short_answer_is_simply_no_networks() {
        assert!(parse_scan(&[]).is_empty());
        assert!(parse_scan(&[0u8; 20]).is_empty());
    }
}
