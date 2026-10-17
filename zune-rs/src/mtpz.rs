//! MTPZ / WMDRM-PD authentication — the RSA + AES-CMAC handshake the Zune demands
//! before it will serve storage operations.
//!
//! Scheme (identical to libmtp-zune, verified live against a Zune 30):
//!   * an application certificate block is RSA-signed with the *private* key;
//!   * the device answers an RSA-encrypted blob; decrypting it with the private key
//!     yields a 16-byte AES session key plus the echoed random;
//!   * a confirmation AES-CMAC is returned to move the session into "trusted" mode.
//!
//! The key material is never shipped: it lives in a 5-line hex file (`.mtpz-data`).

use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecryptMut, KeyIvInit};
use aes::Aes128;
use anyhow::{anyhow, bail, Context, Result};
use num_bigint::BigUint;
use sha1::{Digest, Sha1};

pub const MTPZ_INITIATOR: &str = "libmtp/Sajid Anwar - MTPZClassDriver";

/// The five hex lines of a libmtp-zune key file.
#[derive(Clone)]
pub struct MtpzKeys {
    pub encryption_key: Vec<u8>,
    pub modulus: BigUint,
    pub private_key: BigUint,
    pub certificates: Vec<u8>,
    pub modulus_bytes: Vec<u8>,
}

impl MtpzKeys {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let candidates: Vec<PathBuf> = match explicit {
            Some(p) => vec![p.to_path_buf()],
            None => {
                let mut v = Vec::new();
                if let Ok(env) = std::env::var("MTPZ_DATA") {
                    v.push(PathBuf::from(env));
                }
                if let Some(home) = std::env::var_os("HOME") {
                    v.push(PathBuf::from(home).join(".mtpz-data"));
                }
                v.push(PathBuf::from(".mtpz-data"));
                v
            }
        };
        let path = candidates
            .iter()
            .find(|p| p.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "MTPZ key file not found. Looked in:\n{}",
                    candidates.iter().map(|p| format!("  - {}", p.display())).collect::<Vec<_>>().join("\n")
                )
            })?;
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let lines: Vec<&str> = text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.first().is_some_and(|l| l.starts_with('<')) {
            bail!("{} is a web page (HTML), not the MTPZ key file", path.display());
        }
        if lines.len() < 5 {
            bail!("{}: expected 5 hex lines (public exponent, encryption key, modulus, private key, certificates), got {}", path.display(), lines.len());
        }
        let d = |i: usize, what: &str| -> Result<Vec<u8>> {
            hex::decode(lines[i]).with_context(|| format!("{}: line {} ({what}) is not hex", path.display(), i + 1))
        };
        let modulus_bytes = d(2, "modulus")?;
        Ok(MtpzKeys {
            encryption_key: d(1, "encryption key")?,
            modulus: BigUint::from_bytes_be(&modulus_bytes),
            private_key: BigUint::from_bytes_be(&d(3, "private key")?),
            certificates: d(4, "certificates")?,
            modulus_bytes,
        })
    }

    /// Raw RSA private-key operation, left-padded to the modulus width.
    fn rsa_private(&self, data: &[u8]) -> Vec<u8> {
        let m = BigUint::from_bytes_be(data);
        let r = m.modpow(&self.private_key, &self.modulus);
        let mut out = r.to_bytes_be();
        let width = self.modulus_bytes.len();
        if out.len() < width {
            let mut pad = vec![0u8; width - out.len()];
            pad.extend_from_slice(&out);
            out = pad;
        }
        out
    }
}

pub fn mgf1_sha1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i: u32 = 0;
    while out.len() < len {
        let mut h = Sha1::new();
        h.update(seed);
        h.update(i.to_be_bytes());
        out.extend_from_slice(&h.finalize());
        i += 1;
    }
    out.truncate(len);
    out
}

/// AES-CMAC (RFC 4493) over an arbitrary-length message.
///
/// Delegates to the `cmac` crate: the previous hand-rolled version carried the
/// doubling step's overflow into the wrong neighbour, which silently produced a
/// wrong MAC and would have failed the handshake even after the ordering fix.
/// The tests below pin it to RFC 4493 and to the reference implementation.
pub fn aes_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    use cmac::{Cmac, Mac};
    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key)
        .expect("AES-128 CMAC accepts a 16-byte key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Step 1: build the application-certificate message and the random we must see echoed.
pub fn build_app_cert_msg(keys: &MtpzKeys) -> (Vec<u8>, Vec<u8>) {
    let mut rand = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut rand);
    let rand = rand.to_vec();
    let msg = build_app_cert_msg_with_rand(keys, &rand);
    (msg, rand)
}

/// Same, with the nonce supplied — used by the parity test against the Python
/// reference implementation.
pub fn build_app_cert_msg_with_rand(keys: &MtpzKeys, rand: &[u8]) -> Vec<u8> {
    let rand = rand.to_vec();
    let mut pre = vec![0x02, 0x01, 0x01, 0x00, 0x00];
    pre.extend_from_slice(&(keys.certificates.len() as u16).to_be_bytes());
    pre.extend_from_slice(&keys.certificates);
    pre.extend_from_slice(&[0x00, 0x10]);
    pre.extend_from_slice(&rand);

    let inner = Sha1::digest(&pre[2..]);
    let mut h = Sha1::new();
    h.update([0u8; 8]);
    h.update(inner);
    let h: [u8; 20] = h.finalize().into();

    let mask = mgf1_sha1(&h, 107);
    let mut o = [0u8; 128];
    o[106] = 0x01;
    o[107..127].copy_from_slice(&h);
    for i in 0..107 {
        o[i] ^= mask[i];
    }
    o[0] &= 0x7F;
    o[127] = 0xBC;

    let mut msg = pre;
    msg.extend_from_slice(&[0x01, 0x00, 0x80]);
    msg.extend_from_slice(&keys.rsa_private(&o));
    msg
}

/// Step 2: decrypt the device response, yielding the response payload and the AES key.
pub fn parse_device_response(
    keys: &MtpzKeys,
    resp: &[u8],
    sent_rand: &[u8],
) -> Result<(Vec<u8>, [u8; 16], Option<Vec<u8>>)> {
    if resp.len() < 136 || resp[0] != 0x02 || resp[1] != 0x02 || resp[3] != 0x80 {
        bail!("bad device response header {:02x?}", &resp[..resp.len().min(4)]);
    }
    let mut dec = keys.rsa_private(&resp[4..132]);

    let sm = mgf1_sha1(&dec[21..128], 20);
    for i in 0..20 {
        dec[1 + i] ^= sm[i];
    }
    let dm = mgf1_sha1(&dec[1..21], 107);
    for i in 0..107 {
        dec[21 + i] ^= dm[i];
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(&dec[112..128]);

    let blen = u16::from_be_bytes([resp[134], resp[135]]) as usize;
    if blen == 0 || blen % 16 != 0 {
        bail!("bad AES block length {blen}");
    }
    let ct = resp.get(136..136 + blen).context("truncated encrypted payload")?;
    let mut pt = ct.to_vec();
    let dec_cipher = cbc::Decryptor::<Aes128>::new((&key).into(), (&[0u8; 16]).into());
    let pt = dec_cipher
        .decrypt_padded_mut::<cipher::block_padding::NoPadding>(&mut pt)
        .map_err(|e| anyhow!("AES-CBC decrypt failed: {e:?}"))?;

    let mut o = 1usize;
    let cl = u32::from_be_bytes(pt.get(o..o + 4).context("payload: class length")?.try_into()?) as usize;
    o += 4 + cl;
    let rl = u16::from_be_bytes(pt.get(o..o + 2).context("payload: random length")?.try_into()?) as usize;
    o += 2;
    if pt.get(o..o + rl) != Some(sent_rand) {
        bail!("device echoed random mismatch");
    }
    o += rl;
    let drl = u16::from_be_bytes(pt.get(o..o + 2).context("payload: data length")?.try_into()?) as usize;
    o += 2 + drl;
    o += 1;
    let sl = u16::from_be_bytes(pt.get(o..o + 2).context("payload: string length")?.try_into()?) as usize;
    o += 2 + sl;
    o += 1;
    let ml = u16::from_be_bytes(pt.get(o..o + 2).context("payload: mac length")?.try_into()?) as usize;
    o += 2;
    let mac = pt.get(o..o + ml).context("payload: mac body")?.to_vec();
    // The same payload carries the device's own RSA-1024 public key (marker
    // `01 00 80`, then the 128-byte little-endian modulus). The WiFi passphrase is
    // encrypted with it later, so hand it to the caller.
    let device_public = find_device_modulus(&pt);
    Ok((mac, key, device_public))
}

/// Scan a decrypted MTPZ payload for the device's RSA public modulus: the marker
/// `01 00 80` (exponent 0x10001) followed by 128 little-endian bytes whose low byte
/// is odd — a modulus is. The marker does not sit at a fixed offset (hence the scan),
/// and this is the same heuristic the OpenZune/AFTL client uses.
fn find_device_modulus(payload: &[u8]) -> Option<Vec<u8>> {
    const MODULUS: usize = 128;
    const MARKER: [u8; 3] = [0x01, 0x00, 0x80];
    if payload.len() < MODULUS + 3 {
        return None;
    }
    (0..=payload.len() - (MODULUS + 3)).find_map(|i| {
        (payload[i..i + 3] == MARKER && payload[i + 3] & 1 == 1)
            .then(|| payload[i + 3..i + 3 + MODULUS].to_vec())
    })
}

/// Encrypt a WiFi passphrase the way the Zune client does: RSA-1024 PKCS#1 v1.5 over
/// the UTF-16LE bytes, with the **device's** public key from the MTPZ handshake.
///
/// The modulus arrives little-endian (it is what a Windows capture of the official
/// client contains); RSA maths want it big-endian, hence the reverse. Legacy crypto
/// for a 2006 device — not a security boundary, just wire format.
pub fn encrypt_wifi_password(modulus_le: &[u8], password: &str) -> Result<Vec<u8>> {
    const MODULUS: usize = 128;
    if modulus_le.len() != MODULUS {
        bail!(
            "device RSA modulus is {} bytes, expected {MODULUS}",
            modulus_le.len()
        );
    }
    if password.is_empty() {
        bail!("password is empty");
    }
    let mut plain = Vec::with_capacity(password.len() * 2);
    for unit in password.encode_utf16() {
        plain.extend_from_slice(&unit.to_le_bytes());
    }
    if plain.len() > MODULUS - 11 {
        bail!(
            "passphrase too long for RSA-1024 ({} UTF-16 bytes)",
            plain.len()
        );
    }

    // PKCS#1 v1.5: 00 02 <non-zero padding, at least 8 bytes> 00 <message>
    let mut em = vec![0u8; MODULUS];
    em[1] = 0x02;
    let pad = MODULUS - 3 - plain.len();
    let mut rng = rand::thread_rng();
    for b in em[2..2 + pad].iter_mut() {
        loop {
            let v = rand::RngCore::next_u32(&mut rng) as u8;
            if v != 0 {
                *b = v;
                break;
            }
        }
    }
    em[2 + pad] = 0x00;
    em[3 + pad..].copy_from_slice(&plain);

    let mut n_be = modulus_le.to_vec();
    n_be.reverse();
    let n = BigUint::from_bytes_be(&n_be);
    let m = BigUint::from_bytes_be(&em);
    let e = BigUint::from(65537u32);
    let c = m.modpow(&e, &n);

    let mut out = vec![0u8; MODULUS];
    let bytes = c.to_bytes_be();
    if bytes.len() > MODULUS {
        bail!("RSA ciphertext is {} bytes — modulus too small", bytes.len());
    }
    out[MODULUS - bytes.len()..].copy_from_slice(&bytes);
    Ok(out)
}

/// Step 3: the confirmation MAC sent back to enable trusted operations.
pub fn build_confirmation(mac_hash: &[u8]) -> Vec<u8> {
    let mut k = [0u8; 16];
    k.copy_from_slice(&mac_hash[..16]);
    let mut msg = [0u8; 16];
    msg[15] = 0x01;
    let cm = aes_cmac(&k, &msg);
    let mut out = vec![0x02, 0x03, 0x00, 0x10];
    out.extend_from_slice(&cm);
    out
}

/// AES-CMAC used for `EnableTrustedOps`: CMAC over the 4-byte tail using the leading 16 bytes.
pub fn trusted_ops_hash(mac_hash: &[u8]) -> [u32; 4] {
    let mut k = [0u8; 16];
    k.copy_from_slice(&mac_hash[..16]);
    let cm = aes_cmac(&k, &mac_hash[16..20]);
    let mut out = [0u32; 4];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u32::from_be_bytes(cm[i * 4..i * 4 + 4].try_into().unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmac_hex(key: &[u8], msg: &[u8]) -> String {
        let mut k = [0u8; 16];
        k.copy_from_slice(key);
        hex::encode(aes_cmac(&k, msg))
    }

    /// RFC 4493, section 4 — the vectors the MTPZ scheme's CMAC must satisfy.
    #[test]
    fn aes_cmac_matches_rfc4493() {
        let k = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let cases: [(&str, &str); 3] = [
            ("", "bb1d6929e95937287fa37d129b756746"),
            (
                "6bc1bee22e409f96e93d7e117393172a",
                "070a16b46b4d4144f79bdd9dd04a287c",
            ),
            (
                "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411",
                "dfa66747de9ae63030ca32611497c827",
            ),
        ];
        for (msg, expected) in cases {
            assert_eq!(cmac_hex(&k, &hex::decode(msg).unwrap()), expected, "message {msg}");
        }
    }

    /// The two CMAC-derived values the handshake sends, checked against the values the
    /// Python reference implementation produces for the same mac_hash.
    #[test]
    fn mtpz_macs_match_the_reference_implementation() {
        let mac: Vec<u8> = (0u8..20).collect();
        assert_eq!(
            hex::encode(build_confirmation(&mac)),
            "0203001040104e8db8d421e74222e6381b4f50fa"
        );
        assert_eq!(
            trusted_ops_hash(&mac),
            [1220712810, 1809949664, 4135743756, 1111256502]
        );
    }

    /// Full app-certificate parity with the reference implementation. Skipped (not
    /// failed) when the key file is absent, so the suite stays runnable anywhere.
    #[test]
    fn app_cert_message_matches_the_reference_implementation() {
        let Ok(keys) = MtpzKeys::load(None) else {
            eprintln!("skipping: no MTPZ key file");
            return;
        };
        let rand: Vec<u8> = (16u8..32).collect();
        let msg = build_app_cert_msg_with_rand(&keys, &rand);
        assert_eq!(msg.len(), 785);
        assert_eq!(&msg[..5], &[0x02, 0x01, 0x01, 0x00, 0x00]);
        assert_eq!(&msg[msg.len() - 131..msg.len() - 128], &[0x01, 0x00, 0x80]);
        assert_eq!(
            hex::encode(Sha1::digest(&msg)),
            "9a5d153025dabe36c9d9454c41bc55bf14c32e1f"
        );
    }
}

#[cfg(test)]
mod primitive_tests {
    use super::*;
    use aes::cipher::{BlockEncrypt, KeyInit};

    /// FIPS-197 with the RFC 4493 key: E_K(0^16). The expected value is the one the
    /// Python reference implementation produces for the same input.
    #[test]
    fn aes128_block_matches_the_reference_implementation() {
        let k = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let mut key = [0u8; 16];
        key.copy_from_slice(&k);
        let cipher = Aes128::new((&key).into());
        let mut b = [0u8; 16].into();
        cipher.encrypt_block(&mut b);
        let out: [u8; 16] = b.into();
        assert_eq!(hex::encode(out), "7df76b0c1ab899b33e42f047b91b546f");
        // Those two facts make RFC 4493's subkeys fall out. CMAC over one full block of
        // zeros is E_K(K1), with K1 = dbl(E_K(0)) = fbeed618… — the value the previous
        // hand-rolled doubling step got wrong.
        assert_eq!(
            hex::encode(aes_cmac(&key, &[0u8; 16])),
            "7ad386c3760fb3498361a1cb5563bd70"
        );
    }

    /// NIST SP 800-38A, F.2.2 — the AES-CBC decryption the device response needs.
    #[test]
    fn aes128_cbc_decrypt_matches_nist() {
        use cipher::{BlockDecryptMut, KeyIvInit};
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let ct = hex::decode("7649abac8119b246cee98e9b12e9197d").unwrap();
        let mut buf = ct.clone();
        let dec = cbc::Decryptor::<Aes128>::new(key.as_slice().into(), iv.as_slice().into());
        let pt = dec
            .decrypt_padded_mut::<cipher::block_padding::NoPadding>(&mut buf)
            .unwrap();
        assert_eq!(hex::encode(pt), "6bc1bee22e409f96e93d7e117393172a");
    }

    /// The WiFi passphrase must come back out of the RSA layer byte-for-byte:
    /// encrypt with the public modulus, decrypt with the matching private exponent,
    /// then check the PKCS#1 v1.5 structure and the UTF-16LE message inside.
    ///
    /// Throwaway keypair from `openssl genrsa 1024` (never used for anything else);
    /// both halves live here so the test is self-contained.
    #[test]
    fn wifi_password_survives_the_rsa_roundtrip() {
        let n_hex = "bb80dee21525a3c87c67af37b7903511f8de5f6e15691cd6af22152c88f894eb40f3e14f99496040c3dbbc156cc096625572a25a3cade4007db6fdf37e6edfeec626b574aea1fcfa832dafb55d547cfe657ee9a4486f11f9b0c334975225b70d4c6c2fb9143345bf0394fb3de15b35741e8968a081c95a59235f44ffb8e8e511";
        let d_hex = "6895c23c62c542ecd20f575981a70b230228af1c0d0f3b49b8180fad6e9d111b837844e58c3fb4538baba5e66169aeb81ca73b4d8fe902730fac214c4eea73369c9dc9e3455bfb51a3b668da2ee7356dbb04e78ea74287b3a8bbd6399269305f1a54d664aa121b7e5e0bb21a139e02dae57528fce9a855f6122ff7bd54894dd1";

        // The device hands its modulus over little-endian; flip it as the wire code does.
        let mut n_le = hex::decode(n_hex).unwrap();
        n_le.reverse();
        let ct = encrypt_wifi_password(&n_le, "13800000000").unwrap();
        assert_eq!(ct.len(), 128);

        let n = BigUint::from_bytes_be(&hex::decode(n_hex).unwrap());
        let d = BigUint::from_bytes_be(&hex::decode(d_hex).unwrap());
        let m = BigUint::from_bytes_be(&ct).modpow(&d, &n);
        let mut em = vec![0u8; 128];
        let raw = m.to_bytes_be();
        em[128 - raw.len()..].copy_from_slice(&raw);

        assert_eq!(em[0], 0x00);
        assert_eq!(em[1], 0x02);
        let sep = em[2..].iter().position(|b| *b == 0).unwrap() + 2;
        assert!(sep >= 10, "PKCS#1 padding must be at least 8 bytes");
        let mut expect = Vec::new();
        for u in "13800000000".encode_utf16() {
            expect.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(&em[sep + 1..], expect.as_slice());
    }
}
