//! USB transport: claim the Zune's MTP interface and move bulk packets.
//!
//! The Zune 30 (045e:0710) and Zune HD (045e:063e) expose a vendor-specific MTP
//! interface: bulk OUT, bulk IN, and an interrupt IN. Some firmware revisions
//! declare it as class 6/1/1 (still image), others as class 0, so the interface is
//! located structurally.
//!
//! ## Stalls
//!
//! A halt left behind by an interrupted session makes the device silently ignore
//! writes, so both bulk endpoints are un-stalled on open. That un-stall is not
//! cosmetic and it is not always possible:
//!
//! * on Linux, CLEAR_FEATURE(ENDPOINT_HALT) goes through usbfs and normally works;
//! * on macOS, an already-stalled pipe makes `clear_pipe_stall_both_ends` itself
//!   fail, and every later transfer then reports `kIOReturnNotResponding`
//!   (`0xe00002ed`). The only way back is to reset the device and re-open it.
//!
//! So: un-stall results are reported, never swallowed, and a device that will not
//! come back is reset and re-enumerated once before the caller sees an error.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use nusb::transfer::{Bulk, ControlIn, ControlType, In, Out, Recipient, TransferError};
use nusb::{Device, Endpoint, Interface, MaybeFuture};
pub use nusb::DeviceInfo;

pub const ZUNE_VID: u16 = 0x045E;
pub const ZUNE30_PID: u16 = 0x0710;
pub const ZUNEHD_PID: u16 = 0x063E;

/// `kIOReturnNotResponding` — "device not responding" (IOReturn.h).
pub const KIORETURN_NOT_RESPONDING: u32 = 0xe000_02ed;
/// `kIOUSBPipeStalled` — the pipe is halted.
const KIOUSBPIPE_STALLED: u32 = 0xe000_404f;
/// `kIOReturnNotOpen` — the device/interface is not open.
const KIORETURN_NOT_OPEN: u32 = 0xe000_02c0;

const UNSTALL_ATTEMPTS: u32 = 3;
const UNSTALL_RETRY_DELAY: Duration = Duration::from_millis(80);
/// How long the device gets to re-enumerate after a reset.
const REENUMERATE_DELAY: Duration = Duration::from_millis(900);
/// A reset device disappears and comes back as a new object; poll for it.
const REENUMERATE_ATTEMPTS: u32 = 10;
const REENUMERATE_POLL: Duration = Duration::from_millis(250);

pub fn find_device() -> Result<DeviceInfo> {
    let devs = nusb::list_devices().wait().context("enumerating USB devices")?;
    for d in devs {
        if d.vendor_id() == ZUNE_VID && (d.product_id() == ZUNE30_PID || d.product_id() == ZUNEHD_PID) {
            return Ok(d);
        }
    }
    bail!(
        "no Zune found (looking for USB {:04x}:{:04x} / {:04x})",
        ZUNE_VID,
        ZUNE30_PID,
        ZUNEHD_PID
    )
}

/// Re-find the device after a reset, waiting for it to reappear and preferring the
/// same serial number (a stale entry can still be listed while it re-enumerates).
fn find_device_after_reset(serial: Option<&str>) -> Result<DeviceInfo> {
    let mut last = String::from("device not listed");
    for _ in 0..REENUMERATE_ATTEMPTS {
        match find_device() {
            Ok(d) => {
                if serial.is_none() || d.serial_number() == serial {
                    return Ok(d);
                }
                last = format!("only found a device with serial {}", d.serial_number().unwrap_or("-"));
            }
            Err(e) => last = format!("{e}"),
        }
        std::thread::sleep(REENUMERATE_POLL);
    }
    bail!("the Zune did not re-appear after the reset ({last}) — unplug it, plug it back in, and try again")
}

pub fn device_name(info: &DeviceInfo) -> String {
    match info.product_id() {
        ZUNEHD_PID => "Zune HD".to_string(),
        ZUNE30_PID => "Zune 30".to_string(),
        pid => format!("Zune ({pid:#06x})"),
    }
}

/// Where the device sits in the tree — the first thing to check when transfers
/// time out behind a dock or hub.
pub fn topology(info: &DeviceInfo) -> String {
    let chain = info.port_chain();
    let ports = if chain.is_empty() {
        "-".to_string()
    } else {
        chain.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
    };
    let mut s = format!(
        "bus {} port-chain {} addr {}",
        info.bus_id(),
        ports,
        info.device_address()
    );
    #[cfg(target_os = "macos")]
    s.push_str(&format!(" location {:#x}", info.location_id()));
    s
}

/// Is this error the "the device stopped answering" family, i.e. worth a reset?
pub fn is_stuck(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(te) = cause.downcast_ref::<TransferError>() {
            match te {
                TransferError::Stall | TransferError::Cancelled | TransferError::Fault => return true,
                TransferError::Unknown(code) => {
                    if matches!(*code, KIORETURN_NOT_RESPONDING | KIOUSBPIPE_STALLED | KIORETURN_NOT_OPEN) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        let msg = cause.to_string().to_lowercase();
        if msg.contains("0xe00002ed") || msg.contains("stall") || msg.contains("not responding") {
            return true;
        }
    }
    false
}

/// Turn a transfer failure into something the user can act on.
fn transfer_error(e: TransferError, endpoint: u8, direction: &str) -> anyhow::Error {
    let hint = match &e {
        TransferError::Stall => Some(
            "the endpoint is halted (CLEAR_FEATURE(ENDPOINT_HALT)); the session was left mid-transfer \
             — run `zune usb --reset`, or unplug and replug the device",
        ),
        TransferError::Cancelled => Some("the transfer was cancelled or timed out"),
        TransferError::Disconnected => Some("the device was unplugged"),
        TransferError::Unknown(code) if *code == KIORETURN_NOT_RESPONDING => Some(
            "kIOReturnNotResponding: the Zune did not answer. This is the classic stale-stall \
             symptom after an interrupted session, and on macOS the endpoint cannot be un-stalled \
             in place — run `zune usb --reset`, or unplug and replug the device",
        ),
        TransferError::Unknown(code) if *code == KIOUSBPIPE_STALLED => {
            Some("kIOUSBPipeStalled: the pipe is halted; run `zune usb --reset`")
        }
        TransferError::Unknown(code) if *code == KIORETURN_NOT_OPEN => {
            Some("kIOReturnNotOpen: the interface or device is not open")
        }
        TransferError::Fault => Some("hardware fault or protocol violation"),
        TransferError::InvalidArgument => Some("the request is not supported by this platform"),
        TransferError::Unknown(_) => Some("unknown OS error"),
        #[allow(unreachable_patterns)]
        _ => None,
    };
    match hint {
        Some(h) => anyhow!("bulk {direction} failed on endpoint {endpoint:#04x}: {e} — {h}"),
        None => anyhow!("bulk {direction} failed on endpoint {endpoint:#04x}: {e}"),
    }
}

pub struct Transport {
    pub ep_out: Endpoint<Bulk, Out>,
    pub ep_in: Endpoint<Bulk, In>,
    pub out_addr: u8,
    pub in_addr: u8,
    /// Maximum packet size of the bulk IN endpoint, as the descriptor declares it.
    /// IN request lengths must be a nonzero multiple of this value.
    pub in_max_packet: usize,
    /// Whether the endpoints could be un-stalled at open time.
    pub unstalled: bool,
    /// Print every transfer (`ZUNE_TRACE=1`) for byte-level comparison with a
    /// reference implementation.
    pub trace: bool,
    _device: Device,
    _interface: Interface,
}

/// Byte-level transfer log, enabled with `ZUNE_TRACE=1`.
fn trace(on: bool, dir: &str, data: &[u8]) {
    if !on {
        return;
    }
    let n = data.len().min(48);
    let mut line = hex::encode(&data[..n]);
    if data.len() > n {
        line.push_str(" …");
    }
    eprintln!("    [{dir}] {:>5} B  {line}", data.len());
}

impl Transport {
    /// Open and claim the device, un-stalling the bulk endpoints.
    ///
    /// If the endpoints cannot be un-stalled, the device is reset and re-opened
    /// once — that is the only recovery on macOS.
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Self::open_inner(info, true).with_context(|| format!("opening {} ({})", device_name(info), topology(info)))
    }

    fn open_inner(info: &DeviceInfo, allow_reset: bool) -> Result<Self> {
        let device = info.open().wait().context("opening the USB device")?;
        // A single-configuration device is already configured; some hosts report an
        // error for the redundant request, which is not worth failing over.
        if let Err(e) = device.set_configuration(1).wait() {
            eprintln!("  · set_configuration(1) reported: {e} (continuing)");
        }

        // Locate the MTP interface structurally: the one carrying both a bulk IN
        // and a bulk OUT endpoint. Interface 0 is the conventional answer.
        let mut iface_num = None;
        for iface in device.configurations().flat_map(|c| c.interface_alt_settings()) {
            let (mut out, mut inp) = (false, false);
            for ep in iface.endpoints() {
                if ep.transfer_type() != nusb::descriptors::TransferType::Bulk {
                    continue;
                }
                if ep.address() & 0x80 != 0 {
                    inp = true;
                } else {
                    out = true;
                }
            }
            if out && inp {
                iface_num = Some(iface.interface_number());
                break;
            }
        }
        let iface_num = iface_num.unwrap_or(0);

        let interface = device
            .detach_and_claim_interface(iface_num)
            .wait()
            .with_context(|| format!("claiming MTP interface {iface_num}"))?;

        let (mut out_addr, mut in_addr) = (None, None);
        for ep in interface.descriptors().flat_map(|d| d.endpoints()) {
            if ep.transfer_type() != nusb::descriptors::TransferType::Bulk {
                continue;
            }
            let addr = ep.address();
            if addr & 0x80 != 0 {
                in_addr = Some(addr);
            } else {
                out_addr = Some(addr);
            }
        }
        let out_addr = out_addr.ok_or_else(|| anyhow!("no bulk OUT endpoint on interface {iface_num}"))?;
        let in_addr = in_addr.ok_or_else(|| anyhow!("no bulk IN endpoint on interface {iface_num}"))?;

        let mut ep_out = interface.endpoint::<Bulk, Out>(out_addr).context("bulk OUT")?;
        let mut ep_in = interface.endpoint::<Bulk, In>(in_addr).context("bulk IN")?;
        let in_max_packet = ep_in.max_packet_size();
        log::info!(
            "bulk OUT {out_addr:#04x} (max packet {}) · bulk IN {in_addr:#04x} (max packet {in_max_packet})",
            ep_out.max_packet_size()
        );

        let unstalled = unstall(&mut ep_out, &mut ep_in, out_addr, in_addr);

        if !unstalled && allow_reset {
            eprintln!("» bulk endpoints will not un-stall; resetting the device and re-opening");
            let serial = info.serial_number().map(|s| s.to_string());
            drop(ep_out);
            drop(ep_in);
            drop(interface);
            let reset_result = device.reset().wait();
            drop(device);
            std::thread::sleep(REENUMERATE_DELAY);
            if let Err(e) = reset_result {
                eprintln!("  · reset reported: {e}");
            }
            let fresh = find_device_after_reset(serial.as_deref())?;
            return Self::open_inner(&fresh, false);
        }

        if !unstalled {
            eprintln!(
                "» warning: the endpoints could not be confirmed clear. If the next command fails \
                 with kIOReturnNotResponding, run `zune usb --reset` or replug the device."
            );
        }

        Ok(Transport {
            ep_out,
            ep_in,
            out_addr,
            in_addr,
            in_max_packet,
            unstalled,
            trace: std::env::var_os("ZUNE_TRACE").is_some(),
            _device: device,
            _interface: interface,
        })
    }

    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<()> {
        let addr = self.out_addr;
        trace(self.trace, "OUT", data);
        self.drain_out();
        // `transfer_blocking` is the documented single-transfer API: it consumes
        // exactly one transfer, cancels it on timeout, and never leaves the endpoint
        // in a half-submitted state. Hand-rolling submit + wait_next_complete panics
        // ("no transfer pending") the moment the queue is drained, which is what
        // turned a timed-out write into a crash.
        let c = self.ep_out.transfer_blocking(data.to_vec().into(), timeout);
        match c.status {
            Ok(()) => Ok(()),
            Err(TransferError::Cancelled) => Err(anyhow!(
                "bulk OUT timed out after {timeout:?} on endpoint {addr:#04x} ({} bytes): the device \
                 never acknowledged the write. A Zune that still has an unread response buffered \
                 refuses new commands exactly this way — run `zune usb`, which drains \
                 pending data first and then retries the write.",
                data.len()
            )),
            Err(e) => Err(transfer_error(e, addr, "OUT")),
        }
    }

    /// Throw away anything the endpoint still has queued, without ever awaiting a
    /// completion that is not there.
    fn drain_out(&mut self) {
        let mut spins = 0;
        while self.ep_out.pending() > 0 && spins < 20 {
            log::debug!("draining {} stale OUT transfer(s)", self.ep_out.pending());
            if self.ep_out.wait_next_complete(Duration::from_millis(250)).is_none() {
                self.ep_out.cancel_all();
            }
            spins += 1;
        }
    }

    /// One bulk read; may return a short or zero-length packet. Callers loop.
    ///
    /// The request length is rounded up to a multiple of the endpoint's maximum
    /// packet size. nusb rejects any other IN length with `InvalidArgument`
    /// *before* it reaches the host.
    pub fn read(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        let mps = self.in_max_packet.max(1);
        let request = round_in_request(n, mps);
        log::debug!("bulk IN {n} bytes → {request} (max packet {mps})");

        let addr = self.in_addr;
        let buf = self.ep_in.allocate(request);
        self.drain_in();
        let c = self.ep_in.transfer_blocking(buf, timeout);
        match c.status {
            Ok(()) => {
                let len = c.actual_len;
                let mut v = c.buffer.into_vec();
                v.truncate(len);
                trace(self.trace, "IN", &v);
                Ok(v)
            }
            Err(TransferError::Cancelled) => bail!(
                "bulk IN timed out after {timeout:?} on endpoint {addr:#04x} \
                 (requested {request} bytes, max packet {mps})"
            ),
            Err(e) => Err(transfer_error(e, addr, "IN")
                .context(format!("requested {request} bytes (max packet {mps})"))),
        }
    }

    fn drain_in(&mut self) {
        let mut spins = 0;
        while self.ep_in.pending() > 0 && spins < 20 {
            if self.ep_in.wait_next_complete(Duration::from_millis(250)).is_none() {
                self.ep_in.cancel_all();
            }
            spins += 1;
        }
    }

    /// Read whatever the device has already buffered, without waiting: used to clear
    /// a half-finished session's leftover response before starting a new one.
    pub fn drain_pending_input(&mut self, quiet: Duration) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            match self.read(self.in_max_packet.max(1), quiet) {
                Ok(chunk) if !chunk.is_empty() => out.extend_from_slice(&chunk),
                Ok(_) => break,
                Err(_) => break,
            }
            if out.len() > 1 << 20 {
                break;
            }
        }
        Ok(out)
    }
}

/// Round an IN request up to a nonzero multiple of the endpoint's maximum packet
/// size — the only lengths the USB layer accepts for IN transfers.
pub fn round_in_request(n: usize, max_packet: usize) -> usize {
    let mps = max_packet.max(1);
    if n == 0 {
        mps
    } else {
        n.div_ceil(mps) * mps
    }
}

/// Clear a halt on both bulk endpoints, reporting every failure instead of
/// swallowing it. Returns whether both endpoints came back clean.
fn unstall(
    ep_out: &mut Endpoint<Bulk, Out>,
    ep_in: &mut Endpoint<Bulk, In>,
    out_addr: u8,
    in_addr: u8,
) -> bool {
    for attempt in 1..=UNSTALL_ATTEMPTS {
        let out_err = ep_out.clear_halt().wait().err();
        let in_err = ep_in.clear_halt().wait().err();
        match (&out_err, &in_err) {
            (None, None) => {
                if attempt > 1 {
                    eprintln!("  · endpoints cleared on attempt {attempt}");
                }
                return true;
            }
            _ => {
                let describe = |e: &Option<nusb::Error>| match e {
                    Some(e) => format!("{e}"),
                    None => "ok".to_string(),
                };
                eprintln!(
                    "  · clear_halt attempt {attempt}/{UNSTALL_ATTEMPTS}: OUT {:#04x} {} · IN {:#04x} {}",
                    out_addr,
                    describe(&out_err),
                    in_addr,
                    describe(&in_err)
                );
                if attempt < UNSTALL_ATTEMPTS {
                    std::thread::sleep(UNSTALL_RETRY_DELAY);
                }
            }
        }
    }
    false
}

/// Reset the device and wait for it to re-enumerate. This is the macOS recovery
/// for a stalled bulk pipe.
pub fn reset(info: &DeviceInfo) -> Result<()> {
    let serial = info.serial_number().map(|s| s.to_string());
    let device = info.open().wait().context("opening the device to reset it")?;
    // The reset is refused while an interface is claimed by this process, so it has
    // to run on a handle that claims nothing.
    let r = device.reset().wait();
    drop(device);
    std::thread::sleep(REENUMERATE_DELAY);
    r.map_err(|e| {
        anyhow!(
            "device reset failed: {e} — if another program (Zune software, a VM, a photo app) has \
             the device open, close it and retry; otherwise unplug the Zune and plug it back in"
        )
    })?;
    find_device_after_reset(serial.as_deref())?;
    Ok(())
}

/// Claim the interface and probe the bulk pipes both ways. Reading tells us the
/// device is alive; writing tells us it accepts commands. A Zune that still holds an
/// unread response from an interrupted session refuses every new command until that
/// response is drained, which is why the drain runs first.
pub fn bulk_probe(info: &DeviceInfo) -> Result<()> {
    let mut t = Transport::open(info)?;
    let mps = t.in_max_packet.max(1);
    println!(
        "  bulk OUT {:#04x} · bulk IN {:#04x} · max packet {mps} · unstalled={}",
        t.out_addr, t.in_addr, t.unstalled
    );

    print!("  1. drain pending IN (quiet 350 ms) … ");
    match t.drain_pending_input(Duration::from_millis(350)) {
        Ok(d) if d.is_empty() => println!("nothing buffered"),
        Ok(d) => println!(
            "drained {} bytes: {:02x?}",
            d.len(),
            &d[..d.len().min(48)]
        ),
        Err(e) => println!("failed: {e:#}"),
    }

    let cmd = crate::mtp::Container::cmd(crate::mtp::OP_OPEN_SESSION, 0, &[1]);
    print!("  2. bulk OUT {} bytes (OpenSession) … ", cmd.len());
    match t.write(&cmd, Duration::from_secs(3)) {
        Ok(()) => println!("ok"),
        Err(e) => println!("FAILED: {e:#}"),
    }

    print!("  3. bulk IN one packet ({mps} bytes, 3 s) … ");
    match t.read(mps, Duration::from_secs(3)) {
        Ok(d) => println!(
            "{} bytes: {:02x?}",
            d.len(),
            &d[..d.len().min(48)]
        ),
        Err(e) => println!("FAILED: {e:#}"),
    }
    Ok(())
}

/// Human-readable dump of everything that decides whether MTP can work.
pub fn describe(info: &DeviceInfo) -> Result<()> {
    println!("device        {} ({:04x}:{:04x})", device_name(info), info.vendor_id(), info.product_id());
    println!("serial        {}", info.serial_number().unwrap_or("-"));
    println!("manufacturer  {}", info.manufacturer_string().unwrap_or("-"));
    println!("usb version   {:#06x}", info.usb_version());
    println!("device version {:#06x}", info.device_version());
    println!("class         {:#04x}/{:#04x}/{:#04x}", info.class(), info.subclass(), info.protocol());
    println!("speed         {:?}", info.speed());
    println!("topology      {}", topology(info));

    let device = info.open().wait().context("opening the device")?;
    if let Err(e) = device.set_configuration(1).wait() {
        println!("set_config(1) reported: {e}");
    }
    for cfg in device.configurations() {
        println!(
            "configuration {} ({} interfaces)",
            cfg.configuration_value(),
            cfg.interfaces().count()
        );
        for iface in cfg.interface_alt_settings() {
            println!(
                "  interface {} alt {} class {:#04x}/{:#04x}/{:#04x}",
                iface.interface_number(),
                iface.alternate_setting(),
                iface.class(),
                iface.subclass(),
                iface.protocol()
            );
            for ep in iface.endpoints() {
                println!(
                    "    endpoint {:#04x} {:?} {:?} max-packet {}",
                    ep.address(),
                    ep.direction(),
                    ep.transfer_type(),
                    ep.max_packet_size()
                );
            }
        }
    }
    drop(device);
    Ok(())
}

/// Non-destructive link probe: a standard control transfer that answers before any
/// MTP traffic happens. If this fails, the problem is below MTP (cable, hub, power,
/// another program holding the device); if it succeeds but bulk I/O fails, the
/// problem is the bulk pipe or the device's MTP state.
pub fn probe(info: &DeviceInfo) -> Result<()> {
    let device = info.open().wait().context("opening the device")?;

    // GET_DESCRIPTOR(DEVICE)
    let desc = device.control_in(
        ControlIn {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: 6,
            value: 0x0100,
            index: 0,
            length: 18,
        },
        Duration::from_secs(3),
    );
    match desc.wait() {
        Ok(d) if d.len() >= 8 => println!(
            "control IN  GET_DESCRIPTOR(DEVICE) ok — {} bytes, class {:#04x}/{:#04x}/{:#04x}, max-packet-0 {}",
            d.len(),
            d[4],
            d[5],
            d[6],
            d[7]
        ),
        Ok(d) => println!("control IN  GET_DESCRIPTOR(DEVICE) returned {} bytes", d.len()),
        Err(e) => println!(
            "control IN  GET_DESCRIPTOR(DEVICE) FAILED: {e} — the link itself is not answering"
        ),
    }

    // GET_STATUS(DEVICE): 0x0000 = self-powered/remote-wakeup flags, a cheap liveness check.
    let status = device.control_in(
        ControlIn {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: 0,
            value: 0,
            index: 0,
            length: 2,
        },
        Duration::from_secs(3),
    );
    match status.wait() {
        Ok(d) => println!("control IN  GET_STATUS(DEVICE) ok — {d:02x?}"),
        Err(e) => println!("control IN  GET_STATUS(DEVICE) FAILED: {e}"),
    }

    drop(device);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_errors_are_recognised() {
        let e = anyhow::Error::new(TransferError::Unknown(KIORETURN_NOT_RESPONDING));
        assert!(is_stuck(&e));

        let wrapped = e.context("opening MTP session");
        assert!(is_stuck(&wrapped));

        assert!(is_stuck(&anyhow::Error::new(TransferError::Stall)));
        assert!(!is_stuck(&anyhow!("MTP error 0x2006")));
    }

    #[test]
    fn transfer_errors_carry_a_hint() {
        let msg = transfer_error(TransferError::Unknown(KIORETURN_NOT_RESPONDING), 0x01, "OUT").to_string();
        assert!(msg.contains("kIOReturnNotResponding"), "{msg}");
        assert!(msg.contains("usb --reset"), "{msg}");
    }

    #[test]
    fn in_requests_are_multiples_of_the_max_packet_size() {
        // the Zune at High Speed
        assert_eq!(round_in_request(512, 512), 512);
        // at Full Speed
        assert_eq!(round_in_request(512, 64), 512);
        // a max packet larger than the header read: 512 is NOT a multiple of 1024,
        // which is what nusb rejects with `invalid or unsupported argument`
        assert_eq!(round_in_request(512, 1024), 1024);
        assert_eq!(round_in_request(5, 512), 512);
        assert_eq!(round_in_request(513, 512), 1024);
        assert_eq!(round_in_request(0, 512), 512); // never zero
        assert_eq!(round_in_request(100, 0), 100); // broken descriptor: do not divide by zero
    }
}
