//! Snapshot of `zune-rs/src/device.rs` (read-only copy — do NOT edit zune-rs from here).
//!
//! Provenance: re-synced 2026-10-15T13:16:15Z from ~/zune/zune-rs/src/device.rs
//! sha256: 6095d499edc0cfa3cc34dc181ba42c65b6c72781d418a4020e0644a473daf529
//! Paths rewritten: crate::{mtp,mtpz,usb,device,library,media,tags} -> crate::core::*.
//! Re-synced to pick up the upstream MTPZ CMAC fix (the previous hand-rolled
//! doubling step produced wrong MACs) and the library property-failure reporting.
//! MTP session: command/response plumbing on top of the transport, plus the
//! object operations the Zune exposes once an MTPZ session is trusted.

use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::core::mtp::*;
use crate::core::mtpz::{self, MtpzKeys};
use crate::core::usb::Transport;

pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A byte-range read answers quickly or not at all; see `get_partial_object`.
pub const PARTIAL_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Large writes stall the Zune, so SendObject payloads go out in 16 KB batches.
const PUSH_BATCH: usize = 16 * 1024;

/// Top-level objects report ParentObject == 0.
pub const ROOT: u32 = 0;

pub struct Session {
    pub transport: Transport,
    tx: u32,
    /// storage used as the target for SendObjectInfo
    pub session_storage: u32,
    /// AES session key negotiated by the MTPZ handshake
    pub session_key: Option<[u8; 16]>,
    /// Last operation sent, so a failed response names the step it belongs to.
    last_op: u16,
    /// ZUNE-GUI addition: read timeout for the *next* container. Probes shorten it (a
    /// device that does not implement an operation answers by not answering at all, and
    /// 30 seconds is a long time to find that out).
    read_timeout: Duration,
}

impl Session {
    pub fn new(transport: Transport) -> Self {
        Session {
            transport,
            tx: 0,
            session_storage: 0x0001_0001,
            session_key: None,
            last_op: 0,
            read_timeout: READ_TIMEOUT,
        }
    }

    fn next_tx(&mut self) -> u32 {
        self.tx = self.tx.wrapping_add(1);
        self.tx
    }

    pub fn send_cmd(&mut self, opcode: u16, params: &[u32]) -> Result<()> {
        self.last_op = opcode;
        let tx = self.next_tx();
        let buf = Container::cmd(opcode, tx, params);
        self.transport.write(&buf, WRITE_TIMEOUT)
    }

    /// DATA containers go out as two bulk writes (header, then payload).
    pub fn send_data(&mut self, opcode: u16, payload: &[u8]) -> Result<()> {
        let (head, body) = Container::data(opcode, self.tx, payload);
        self.transport.write(&head, WRITE_TIMEOUT)?;
        if !body.is_empty() {
            self.transport.write(&body, WRITE_TIMEOUT)?;
            if body.len() % 512 == 0 {
                self.transport.write(&[], WRITE_TIMEOUT)?;
            }
        }
        Ok(())
    }

    /// Read one complete container, skipping the zero-length packet that terminates
    /// a payload which filled whole 512-byte packets.
    pub fn recv_container(&mut self) -> Result<Container> {
        let read_timeout = self.read_timeout;
        let mut head = self.transport.read(512, read_timeout)?;
        while head.is_empty() {
            head = self.transport.read(512, read_timeout)?;
        }
        let total = u32_at(&head, 0)? as usize;
        if total < 12 {
            bail!("device sent a container shorter than a header ({total})");
        }
        let mut buf = head;
        while buf.len() < total {
            let more = self.transport.read(total - buf.len(), self.read_timeout)?;
            if more.is_empty() {
                continue;
            }
            buf.extend_from_slice(&more);
        }
        Container::parse(&buf)
    }

    /// A data-returning operation: the device sends DATA then RESPONSE.
    pub fn recv_data(&mut self) -> Result<Container> {
        let c = self.recv_container()?;
        if c.ctype != T_DATA {
            bail!(
                "expected a DATA container, got type {:#06x} (code {:#06x}, params {:?})",
                c.ctype,
                c.code,
                c.params
            );
        }
        Ok(c)
    }

    /// A plain operation response; fails on any non-OK response code.
    pub fn recv_resp(&mut self) -> Result<Container> {
        let c = self.recv_container()?;
        if c.ctype != T_RESP {
            bail!(
                "expected a RESPONSE container for {:#06x} ({}), got type {:#06x}",
                self.last_op,
                op_name(self.last_op),
                c.ctype
            );
        }
        if c.code != C_OK {
            bail!(
                "MTP error {:#06x} ({}) from {:#06x} ({}) — response params {:?}",
                c.code,
                response_name(c.code),
                self.last_op,
                op_name(self.last_op),
                c.params
            );
        }
        Ok(c)
    }

    // ── session ─────────────────────────────────────────────────────────────

    pub fn open_session(&mut self) -> Result<()> {
        // The Zune wants OpenSession with transaction id 0.
        let buf = Container::cmd(OP_OPEN_SESSION, 0, &[1]);
        self.transport.write(&buf, WRITE_TIMEOUT)?;
        self.tx = 0;
        self.recv_resp()?;
        Ok(())
    }

    pub fn close_session(&mut self) -> Result<()> {
        let _ = self.send_cmd(OP_CLOSE_SESSION, &[]);
        let _ = self.recv_resp();
        Ok(())
    }

    pub fn device_info(&mut self) -> Result<DeviceInfo> {
        self.send_cmd(OP_GET_DEVICE_INFO, &[])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        DeviceInfo::parse(&d)
    }

    pub fn set_device_prop(&mut self, code: u16, value: &str) -> Result<()> {
        self.send_cmd(OP_SET_DEVICE_PROP, &[code as u32])?;
        self.send_data(OP_SET_DEVICE_PROP, &enc_str(value))?;
        self.recv_resp()?;
        Ok(())
    }

    // ── MTPZ handshake ──────────────────────────────────────────────────────

    pub fn reset_mtpz(&mut self) -> Result<()> {
        self.send_cmd(OP_END_TRUSTED_APP_SESSION, &[])?;
        self.recv_resp()?;
        Ok(())
    }

    fn send_mtpz(&mut self, data: &[u8]) -> Result<()> {
        self.send_cmd(OP_SEND_WMDRM_PD_REQ, &[])?;
        self.send_data(OP_SEND_WMDRM_PD_REQ, data)?;
        self.recv_resp()?;
        Ok(())
    }

    fn get_mtpz(&mut self) -> Result<Vec<u8>> {
        self.send_cmd(OP_GET_WMDRM_PD_RESP, &[])?;
        let payload = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(payload)
    }

    fn enable_trusted(&mut self, mac_hash: &[u8]) -> Result<()> {
        let h = mtpz::trusted_ops_hash(mac_hash);
        self.send_cmd(OP_ENABLE_TRUSTED_OPS, &h)?;
        self.recv_resp()?;
        Ok(())
    }

    /// Full MTPZ handshake. Must run after `open_session`.
    ///
    /// The order is the one libmtp-zune uses and it is load-bearing: exactly **one**
    /// `EndTrustedAppSession` runs before the exchange. A second one between the
    /// device response and the confirmation tears down the trusted session the device
    /// has just established, and the next request comes back `0x201C`
    /// (TransactionCancelled).
    pub fn authenticate(&mut self, keys: &MtpzKeys) -> Result<()> {
        self.set_device_prop(DEVPROP_SESSION_INITIATOR, mtpz::MTPZ_INITIATOR)
            .context("MTPZ 1/6 set DeviceProp 0xD406 (initiator name)")?;
        self.reset_mtpz()
            .context("MTPZ 2/6 EndTrustedAppSession (0x9216) — clears any previous trusted session")?;

        let (msg, rand) = mtpz::build_app_cert_msg(keys);
        self.send_mtpz(&msg)
            .context("MTPZ 3/6 SendWMDRMPDReq (0x9212) — app certificate")?;
        let resp = self
            .get_mtpz()
            .context("MTPZ 4/6 GetWMDRMPDResp (0x9213) — device challenge")?;
        let (mac, key) = mtpz::parse_device_response(keys, &resp, &rand)
            .context("MTPZ 5/6 decrypting the device response")?;
        self.session_key = Some(key);

        self.send_mtpz(&mtpz::build_confirmation(&mac))
            .context("MTPZ 6/6 SendWMDRMPDReq (0x9212) — confirmation MAC")?;
        self.enable_trusted(&mac)
            .context("MTPZ EnableTrustedOps (0x9214)")?;
        Ok(())
    }

    // ── storage & enumeration ───────────────────────────────────────────────

    pub fn storage_ids(&mut self) -> Result<Vec<u32>> {
        self.send_cmd(OP_GET_STORAGE_IDS, &[])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        parse_u32_array(&d)
    }

    /// Object handles of one format under `parent` (0 = device root).
    pub fn children(&mut self, format: u16, parent: u32) -> Result<Vec<u32>> {
        self.send_cmd(OP_GET_OBJECT_HANDLES, &[self.session_storage, format as u32, parent])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(parse_u32_array(&d)?.into_iter().filter(|h| *h != 0).collect())
    }

    pub fn object_handles(&mut self, storage: u32, format: u16, parent: u32) -> Result<Vec<u32>> {
        self.send_cmd(OP_GET_OBJECT_HANDLES, &[storage, format as u32, parent])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        parse_u32_array(&d)
    }

    pub fn object_info(&mut self, handle: u32) -> Result<ObjectInfo> {
        self.send_cmd(OP_GET_OBJECT_INFO, &[handle])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        ObjectInfo::parse(&d)
    }

    // ── ZUNE-GUI additions ──────────────────────────────────────────────────

    /// Run `f` with a shorter read timeout, restoring the default afterwards.
    pub fn with_read_timeout<R>(&mut self, timeout: Duration, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.read_timeout;
        self.read_timeout = timeout;
        let result = f(self);
        self.read_timeout = previous;
        result
    }

    /// Throw away whatever the device has already sent and nobody read.
    ///
    /// This is the recovery that makes a *timeout* survivable: a Zune with an unread
    /// response buffered refuses every later command with a bulk-OUT timeout, so a
    /// failed bulk read must be drained before anything else is attempted.
    pub fn drain_pending_input(&mut self, quiet: Duration) -> Result<usize> {
        let drained = self.transport.drain_pending_input(quiet)?;
        Ok(drained.len())
    }

    /// GetStorageInfo (0x1005): total/free space of one storage id.
    ///
    /// Firmware that does not implement it answers 0x2005 (OperationNotSupported);
    /// the UI shows "unknown" rather than treating that as a device fault.
    pub fn storage_info(&mut self, storage: u32) -> Result<StorageInfo> {
        self.send_cmd(OP_GET_STORAGE_INFO, &[storage])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        StorageInfo::parse(&d)
    }

    /// GetObject (0x1009): download an object's payload (cover art).
    pub fn get_object_data(&mut self, handle: u32) -> Result<Vec<u8>> {
        self.send_cmd(OP_GET_OBJECT, &[handle])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(d)
    }

    /// GetObjectPropList (0x9805): properties for many objects in one round trip.
    ///
    /// `handle` 0xFFFFFFFF selects every object, `format` 0 selects every object
    /// format, `propcode` 0xFFFF every property, and `depth` 0xFFFFFFFF walks the whole
    /// tree below the handle (libmtp's `ptp_mtp_getobjectproplist` sends exactly these
    /// five parameters).
    pub fn object_prop_list(
        &mut self,
        handle: u32,
        format: u16,
        propcode: u16,
        depth: u32,
    ) -> Result<Vec<PropEntry>> {
        self.send_cmd(
            OP_GET_OBJECT_PROP_LIST,
            &[handle, format as u32, propcode as u32, 0, depth],
        )?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        let entries = parse_prop_list(&d)?;
        if entries.is_empty() {
            bail!("the device answered the property-list request with zero entries");
        }
        Ok(entries)
    }

    /// GetThumb (0x100A): the thumbnail the device already holds for an object.
    ///
    /// The fallback when a firmware refuses GetObject for cover-art objects.
    /// `format` is the wanted thumbnail format code (JPEG = 0x3801, 0 = undefined).
    /// GetPartialObject (0x101B): `count` bytes of an object, starting at `offset`.
    ///
    /// Reads a byte range, so an 8 MB MP3 does not have to cross USB to reach an ID3 tag
    /// in its first 64 KB.
    ///
    /// The timeout is short on purpose. A handle that no longer exists makes this firmware
    /// answer nothing at all, and at the default 30 seconds that is half a minute of a
    /// frozen worker per stale handle — measured on one album in a cached library whose
    /// device had since changed. A partial read is a small, fast request: if it has not
    /// answered in five seconds it is not going to.
    pub fn get_partial_object(&mut self, handle: u32, offset: u32, count: u32) -> Result<Vec<u8>> {
        let saved = self.read_timeout;
        self.read_timeout = PARTIAL_READ_TIMEOUT;
        let result = self.partial_object_inner(handle, offset, count);
        self.read_timeout = saved;
        result
    }

    fn partial_object_inner(&mut self, handle: u32, offset: u32, count: u32) -> Result<Vec<u8>> {
        self.send_cmd(OP_GET_PARTIAL_OBJECT, &[handle, offset, count])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(d)
    }

    pub fn get_thumb_data(&mut self, handle: u32, format: u16) -> Result<Vec<u8>> {
        self.send_cmd(OP_GET_THUMB, &[handle, format as u32])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(d)
    }

    // ── object creation & payload ───────────────────────────────────────────

    /// SendObjectInfo with a full ObjectInfo dataset; returns the new handle.
    #[allow(clippy::too_many_arguments)]
    pub fn create_object(
        &mut self,
        parent: u32,
        format: u16,
        filename: &str,
        size: u32,
        width: u32,
        height: u32,
        depth: u32,
    ) -> Result<u32> {
        let ds = object_dataset(
            self.session_storage,
            format,
            size,
            parent,
            filename,
            width,
            height,
            depth,
        );
        self.send_cmd(OP_SEND_OBJECT_INFO, &[self.session_storage, parent])?;
        self.send_data(OP_SEND_OBJECT_INFO, &ds)?;
        let r = self.recv_resp()?;
        Ok(r.param(2).or_else(|| r.param(0)).unwrap_or(0))
    }

    /// SendObject with a streamed payload (16 KB batches, ZLP when it lands on a
    /// 512-byte boundary). An empty payload commits an empty object and writes no
    /// payload packets at all.
    pub fn push_object_data(&mut self, data: &[u8]) -> Result<()> {
        self.push_object_data_with_progress(data, |_, _| {})
    }

    /// ZUNE-GUI addition: like [`Self::push_object_data`], but reports the bytes on
    /// the wire after every 16 KB batch so the UI can drive a progress bar. The wire
    /// format is identical.
    pub fn push_object_data_with_progress(
        &mut self,
        data: &[u8],
        mut progress: impl FnMut(usize, usize),
    ) -> Result<()> {
        self.send_cmd(OP_SEND_OBJECT, &[])?;
        let head = Container::data_header(OP_SEND_OBJECT, self.tx, data.len());
        self.transport.write(&head, WRITE_TIMEOUT)?;
        if !data.is_empty() {
            let mut sent = 0usize;
            for chunk in data.chunks(PUSH_BATCH) {
                self.transport.write(chunk, WRITE_TIMEOUT)?;
                sent += chunk.len();
                progress(sent, data.len());
            }
            if data.len() % 512 == 0 {
                self.transport.write(&[], WRITE_TIMEOUT)?;
            }
        }
        self.recv_resp()?;
        Ok(())
    }

    pub fn delete_object(&mut self, handle: u32) -> Result<()> {
        self.send_cmd(OP_DELETE_OBJECT, &[handle])?;
        self.recv_resp()?;
        Ok(())
    }

    // ── object properties ───────────────────────────────────────────────────

    fn set_prop(&mut self, handle: u32, prop: u16, data: &[u8]) -> Result<()> {
        self.send_cmd(OP_SET_OBJECT_PROP, &[handle, prop as u32])?;
        self.send_data(OP_SET_OBJECT_PROP, data)?;
        self.recv_resp()?;
        Ok(())
    }

    pub fn set_prop_str(&mut self, handle: u32, prop: u16, value: &str) -> Result<()> {
        self.set_prop(handle, prop, &enc_str(value))
    }

    pub fn set_prop_u16(&mut self, handle: u32, prop: u16, value: u16) -> Result<()> {
        self.set_prop(handle, prop, &value.to_le_bytes())
    }

    pub fn set_prop_u32(&mut self, handle: u32, prop: u16, value: u32) -> Result<()> {
        self.set_prop(handle, prop, &value.to_le_bytes())
    }

    /// SetObjectReferences (0x9811): u32 count followed by u32 handles.
    pub fn set_object_refs(&mut self, handle: u32, refs: &[u32]) -> Result<()> {
        let mut data = Vec::with_capacity(4 + refs.len() * 4);
        data.extend_from_slice(&(refs.len() as u32).to_le_bytes());
        for r in refs {
            data.extend_from_slice(&r.to_le_bytes());
        }
        self.send_cmd(OP_SET_OBJECT_REFS, &[handle])?;
        self.send_data(OP_SET_OBJECT_REFS, &data)?;
        self.recv_resp()?;
        Ok(())
    }

    fn get_prop_raw(&mut self, handle: u32, prop: u16) -> Result<Vec<u8>> {
        self.send_cmd(OP_GET_OBJECT_PROP, &[handle, prop as u32])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        Ok(d)
    }

    /// GetObjectReferences (0x9810): the handles a playlist or album points at.
    pub fn object_refs(&mut self, handle: u32) -> Result<Vec<u32>> {
        self.send_cmd(OP_GET_OBJECT_REFS, &[handle])?;
        let d = self.recv_data()?.data;
        self.recv_resp()?;
        parse_u32_array(&d)
    }

    pub fn get_prop_str(&mut self, handle: u32, prop: u16) -> Result<String> {
        let d = self.get_prop_raw(handle, prop)?;
        Ok(dec_str(&d, 0)?.0)
    }

    pub fn get_prop_u16(&mut self, handle: u32, prop: u16) -> Result<u16> {
        let d = self.get_prop_raw(handle, prop)?;
        u16_at(&d, 0)
    }

    /// ZUNE-GUI addition: Duration (0xDC89) and friends are UINT32 properties.
    pub fn get_prop_u32(&mut self, handle: u32, prop: u16) -> Result<u32> {
        let d = self.get_prop_raw(handle, prop)?;
        u32_at(&d, 0)
    }

    // ── Zune vendor: WiFi control (firmware Keel 3.30, MtpSvc opcode block 0x9220-0x9229) ──

    /// 0x9225 — ask the device to scan and report visible networks.
    pub fn wifi_scan(&mut self) -> Result<Vec<u8>> {
        self.send_cmd(OP_WIFI_SCAN, &[])?;
        let c = self.recv_container()?;
        match c.ctype {
            T_DATA => {
                let data = c.data.clone();
                self.recv_resp()?;
                Ok(data)
            }
            T_RESP => {
                if c.code != C_OK {
                    bail!("wifi scan failed: {:#06x}", c.code);
                }
                Ok(Vec::new())
            }
            other => bail!("wifi scan: unexpected container type {other:#06x}"),
        }
    }

    /// 0x9227 — commit a WiFi profile blob (<= 4096 bytes) to the device.
    ///
    /// The firmware hands this buffer straight to `ZNetSetWifiProfile(1000, buf, len)`
    /// / `ZNetAddWifiProfile(1007, buf, len)`, so the layout is the device-side
    /// WZC/NDIS_802_11 configuration structure, not a PC-side one.
    pub fn wifi_set_profile(&mut self, blob: &[u8]) -> Result<()> {
        if blob.is_empty() || blob.len() > 0x1000 {
            bail!("WiFi profile must be 1..=4096 bytes (got {})", blob.len());
        }
        self.send_cmd(OP_WIFI_SET_PROFILE, &[])?;
        self.send_data(OP_WIFI_SET_PROFILE, blob)?;
        self.recv_resp()?;
        Ok(())
    }

    /// 0x9228 — connectivity test / passthrough connect.
    pub fn wifi_test_connectivity(&mut self) -> Result<()> {
        self.send_cmd(OP_WIFI_TEST_CONNECTIVITY, &[])?;
        self.recv_resp()?;
        Ok(())
    }
}

/// Storage capacity/usage as reported by GetStorageInfo (0x1005).
#[derive(Debug, Default, Clone)]
pub struct StorageInfo {
    pub storage_type: u16,
    pub filesystem_type: u16,
    pub access_capability: u16,
    pub max_capacity: u64,
    pub free_space_bytes: u64,
    pub free_space_objects: u64,
    pub description: String,
    pub volume_label: String,
}

impl StorageInfo {
    /// Standard MTP StorageInfo dataset: three u16, three u64, then two strings.
    /// The trailing strings are optional: some firmware stops after the u64s, and a
    /// missing description is not worth failing a whole refresh over.
    pub fn parse(d: &[u8]) -> Result<Self> {
        let mut out = StorageInfo {
            storage_type: u16_at(d, 0)?,
            filesystem_type: u16_at(d, 2)?,
            access_capability: u16_at(d, 4)?,
            max_capacity: u64_at(d, 6)?,
            free_space_bytes: u64_at(d, 14)?,
            free_space_objects: u64_at(d, 22)?,
            ..Default::default()
        };
        if let Ok((description, off)) = dec_str(d, 30) {
            out.description = description;
            if let Ok((volume, _)) = dec_str(d, off) {
                out.volume_label = volume;
            }
        }
        Ok(out)
    }
}

/// Build the 52-byte ObjectInfo dataset the Zune accepts, plus the name and two
/// empty date strings. Field order is the MTP spec's; the Zune rejects anything
/// shorter.
#[allow(clippy::too_many_arguments)]
pub fn object_dataset(
    storage: u32,
    format: u16,
    size: u32,
    parent: u32,
    filename: &str,
    width: u32,
    height: u32,
    depth: u32,
) -> Vec<u8> {
    let mut d = Vec::with_capacity(64 + filename.len() * 2);
    d.extend_from_slice(&storage.to_le_bytes());
    d.extend_from_slice(&format.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // protection status
    d.extend_from_slice(&size.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // thumbnail format
    d.extend_from_slice(&0u32.to_le_bytes()); // thumbnail size
    d.extend_from_slice(&0u32.to_le_bytes()); // thumbnail width
    d.extend_from_slice(&0u32.to_le_bytes()); // thumbnail height
    d.extend_from_slice(&width.to_le_bytes());
    d.extend_from_slice(&height.to_le_bytes());
    d.extend_from_slice(&depth.to_le_bytes());
    d.extend_from_slice(&parent.to_le_bytes());
    d.extend_from_slice(&0u16.to_le_bytes()); // association type
    d.extend_from_slice(&0u32.to_le_bytes()); // association description
    d.extend_from_slice(&0u32.to_le_bytes()); // sequence number
    d.extend_from_slice(&enc_str(filename));
    d.push(0); // empty capture date
    d.push(0); // empty modification date
    d
}

// ── device info ──────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct DeviceInfo {
    pub standard_version: u16,
    pub vendor_ext_id: u32,
    pub vendor_ext_version: u16,
    pub vendor_ext_desc: String,
    pub functional_mode: u16,
    pub operations: Vec<u16>,
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
}

impl DeviceInfo {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let mut o = 0usize;
        let mut out = DeviceInfo::default();
        out.standard_version = u16_at(d, o)?;
        o += 2;
        out.vendor_ext_id = u32_at(d, o)?;
        o += 4;
        out.vendor_ext_version = u16_at(d, o)?;
        o += 2;
        let (desc, no) = dec_str(d, o)?;
        out.vendor_ext_desc = desc;
        o = no;
        out.functional_mode = u16_at(d, o)?;
        o += 2;

        // arrays: operations, events, device properties, capture formats, image formats
        for i in 0..5 {
            let n = u32_at(d, o)? as usize;
            if i == 0 {
                out.operations = (0..n).filter_map(|k| u16_at(d, o + 4 + k * 2).ok()).collect();
            }
            o += 4 + n * 2;
        }
        let (manufacturer, no) = dec_str(d, o)?;
        out.manufacturer = manufacturer;
        o = no;
        let (model, no) = dec_str(d, o)?;
        out.model = model;
        o = no;
        let (device_version, no) = dec_str(d, o)?;
        out.device_version = device_version;
        o = no;
        let (serial, _) = dec_str(d, o)?;
        out.serial_number = serial;
        Ok(out)
    }
}

// ── object info ──────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct ObjectInfo {
    pub handle: u32,
    pub storage: u32,
    pub format: u16,
    pub size: u32,
    pub parent: u32,
    pub filename: String,
}

impl ObjectInfo {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let mut o = 0usize;
        let mut out = ObjectInfo::default();
        out.storage = u32_at(d, o)?;
        o += 4;
        out.format = u16_at(d, o)?;
        o += 2;
        o += 2; // protection status
        out.size = u32_at(d, o)?;
        o += 4;
        o += 26; // thumbnail + image geometry
        out.parent = u32_at(d, o)?;
        o += 4;
        o += 10; // association type/desc + sequence number
        let (filename, _) = dec_str(d, o)?;
        out.filename = filename;
        Ok(out)
    }

    /// The name the Zune itself displays (it derives it from the object name).
    pub fn label(&self) -> String {
        self.filename.clone()
    }
}

pub mod format_name {
    use super::fmt;
    pub fn of(f: u16) -> &'static str {
        match f {
            fmt::MP3 => "mp3",
            fmt::WMA => "wma",
            fmt::AAC => "aac",
            fmt::WMV => "wmv",
            fmt::MP4 => "mp4",
            fmt::JPEG => "jpeg",
            fmt::COVER_ART => "cover-art",
            fmt::ASSOCIATION => "folder",
            fmt::ABSTRACT_ALBUM => "album",
            fmt::PLAYLIST => "playlist",
            _ => "?",
        }
    }
}

pub fn hexdump(d: &[u8]) -> String {
    d.chunks(16)
        .map(|c| c.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod device_probe_tests {
    use super::*;

    /// Not part of the normal run: a full connect + MTPZ handshake from a background
    /// thread, which is exactly what the GUI worker does. Run with the device plugged
    /// in:
    ///
    ///   cargo test --features linux -- --ignored thread_can_talk_to_the_device
    ///
    /// Read-only: it opens a session and reads the device info, nothing else.
    #[test]
    #[ignore = "talks to a real device over USB"]
    fn thread_can_talk_to_the_device() {
        let handle = std::thread::spawn(|| -> anyhow::Result<String> {
            let info = crate::core::usb::find_device()?;
            let mut transport = crate::core::usb::Transport::open(&info)?;
            let _ = transport.drain_pending_input(std::time::Duration::from_millis(350));
            let mut session = Session::new(transport);
            session.open_session()?;
            // The handshake and a read, the two things the worker does first.
            let keys = crate::core::MtpzKeys::load(None)?;
            session.authenticate(&keys)?;
            let di = session.device_info()?;
            Ok(format!("{} / {}", di.manufacturer, di.device_version))
        });
        match handle.join() {
            Ok(Ok(info)) => println!("background-thread connect OK: {info}"),
            Ok(Err(e)) => panic!("background-thread connect FAILED: {e:#}"),
            Err(_) => panic!("the worker thread panicked"),
        }
    }
}
