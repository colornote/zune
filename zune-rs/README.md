# zune-client (Rust)

An open Zune client: MTP over USB with the **MTPZ** authentication handshake, so a
Zune 30 / Zune HD can be driven from Linux, macOS and Windows without the Microsoft
desktop software.

This is the Rust reimplementation of the Python `zune-cli`, plus the device-management
surface the official client had (wireless sync configuration).

## Status

| Area | State |
|---|---|
| USB transport (nusb, no libusb dependency) | ✅ |
| MTP container layer (incl. the Zune DATA-phase quirks) | ✅ |
| MTPZ handshake (RSA + AES-CMAC + AES-CBC) | ✅ |
| **Tag reading** (mp3/m4a/flac/ogg/wav → title/artist/album/albumartist/track) | ✅ |
| **Tracks** — push with metadata, de-duplicate by name | ✅ |
| **Albums** — tracks + `.pla` playlist + cover art + abstract album | ✅ |
| **Playlists** — create/replace, reference tracks | ✅ |
| **Listing** — tracks with displayed metadata, albums/playlists, library size | ✅ |
| **Delete** — by handle or by name | ✅ |
| Transcoding (ffmpeg) for wav/flac/m4a/ogg… → mp3, mp4/mkv/avi → wmv | ✅ |
| **Photos** — fitted to 320x240 / 240x320, baseline JPEG, alpha over black | ✅ |
| **Local convert** (`zune convert`) — no device needed, for checking first | ✅ |
| 320-byte WiFi profile record builder | ⏳ (layout under analysis) |

## Build

### Native

```bash
cargo build --release      # -> target/release/zune
```

### Cross-build the macOS (Apple Silicon) binary from Linux

No Xcode or Mac required: Zig's clang links Mach-O against an extracted macOS SDK.

```bash
rustup target add aarch64-apple-darwin
# zig on PATH (e.g. https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz)
# SDK once (e.g. https://github.com/phracker/MacOSX-SDKs/releases -> MacOSX11.3.sdk.tar.xz)

SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh
# -> dist/zune-aarch64-apple-darwin   (Mach-O arm64, minos 11.0, ad-hoc signed)
```

`macos-linker.sh` is the linker wrapper: Zig resolves frameworks and the `.tbd` stubs
through `SDKROOT`, and the deployment target is pinned in the `-target aarch64-macos.<min>`
triple (Zig's default minimum is 13.0, so it is set explicitly).

On the Mac:

```bash
tar xzf zune-aarch64-apple-darwin.tar.gz && chmod +x zune-aarch64-apple-darwin
xattr -d com.apple.quarantine zune-aarch64-apple-darwin 2>/dev/null   # only if it came via a browser
./zune-aarch64-apple-darwin info
```

The binary is ad-hoc signed; if macOS still refuses it, re-sign locally:
`codesign --force --sign - zune-aarch64-apple-darwin`.

## Keys

The Zune refuses storage operations until an MTPZ session is trusted. The key material
is not part of this project; put the 5-line hex file at `$MTPZ_DATA`, `~/.mtpz-data`
or `./.mtpz-data` (same file libmtp-zune uses):

```
<public exponent>
<encryption key>
<modulus>
<private key>
<certificates>
```

## Usage

```bash
zune info                             # identity + counts (3 calls; --size costs a sweep)
zune tracks                           # tracks + the metadata the device displays
zune tracks --grep "kid a"            # filter by title/artist/album/file name
zune albums                           # playlists and abstract albums (with track counts)
zune ops                              # the opcodes the device advertises
zune props 0x0001002a                 # every property + reference of one object

zune tags ~/Music                     # local tag read (no device needed)
zune convert ~/Music -o ready/         # convert locally, inspect before pushing
zune push song.mp3                     # tag it, set properties, de-dup by name
zune push ~/Music/Album                # anything not device-ready is transcoded
zune album "~/Music/01 - Kid A"         # tracks + playlist + cover + abstract album
zune album ~/Music/Boards --name "Geogaddi" --dry-run

zune photo ~/Pictures                  # 320x240 / 240x320 letterboxed baseline JPEG
zune video ~/Movies --subs film.srt    # 320x240 wmv2 + wmav2, subtitles burned in

zune playlist "Road trip" a.mp3 b.mp3  # pushes what is missing, links the rest
zune playlist "Device only" a.mp3 --device-only

zune rm --name song                    # by file stem
zune rm 0x0001002a                     # by handle

zune wifi scan                         # 0x9225 -> ZNetScanAndWaitForNetworks
zune wifi set-profile --hex 8a...      # 0x9227 -> ZNetSetWifiProfile + ZNetAddWifiProfile
zune wifi test                         # 0x9228 -> ZNetTestConnectivity
zune eject
```

### What gets converted, and how

| Input | Output | Notes |
|---|---|---|
| `.mp3` with ID3v2.3 or none | pushed as-is | the device ignores **ID3v2.4** |
| `.mp3` with ID3v2.4 | `-c:a copy -id3v2_version 3` | tags rewritten, audio untouched |
| `.wav` `.flac` `.m4a` `.aac` `.ogg` `.opus` `.wma` … | MP3 320 kbit/s, `-map_metadata 0` | tags survive the transcode |
| `.wmv` | pushed as-is | |
| `.mp4` `.mkv` `.avi` `.mov` `.webm` … | WMV2 320x240 + WMAv2 128k | letterboxed (never stretched), ≤30 fps, `-ignore_editlist 1` on the MP4 family |
| `.jpg` `.png` `.bmp` `.gif` `.tif` `.webp` | baseline JPEG, 320x240 or 240x320 | letterboxed on black, alpha flattened, q90 |
| `.heic` `.heif` | via macOS `sips` first | |

Large or progressive JPEGs list on the device but refuse to open, which is why every
photo is re-encoded rather than passed through. `zune convert` runs exactly the same
pipeline locally so you can check the output before plugging the Zune in.

### Metadata comes from the tags, not from property writes

The Zune indexes title/artist/album/track from the **file's own tags** as it ingests an
object — it even renames it to `NN Title.mp3` from them — and answers
`SetObjectPropValue` with `0x200F AccessDenied` because the media database owns the
object. Property writes are therefore best-effort and reported in one line, never fatal.

What matters is that the file carries correct ID3v2.3 tags: MP3s are passed through with
`-c:a copy` (tags rewritten from v2.4 to v2.3 when needed) and other formats are
transcoded with `-map_metadata 0`.

### What `zune album DIR` actually writes

The Zune shows nothing — no artist, no album, no cover — unless an **Abstract Album**
(0xBA03) object exists at the device root. `zune album DIR` performs the whole ritual:

1. every audio file in `DIR` is pushed to the device **root** as `<stem>.mp3`, with
   Name / Artist / Album / AlbumArtist / Track set from the file's own tags;
2. a `<album>.pla` playlist (0xBA05) in the `Playlists` folder referencing those tracks;
3. the largest JPEG in `DIR` (or `--cover`) pushed as CoverArt (0x3802) into `CoverArt`;
4. an Abstract Album named after the album, referencing **tracks + cover art**.

Re-pushing replaces by name: the firmware strips dots from object names, so
`01. Song.mp3` and `01 Song.mp3` are the same device object.

## Troubleshooting USB

`zune usb` dumps everything that decides whether MTP can work — descriptors, topology,
the claim, the un-stall result, and a non-destructive control-transfer probe — and
`zune usb --reset` resets the device and waits for it to re-enumerate.

| Symptom | Cause | Fix |
|---|---|---|
| `bulk OUT failed … kIOReturnNotResponding (0xe00002ed)` | stale stall from an interrupted session; on macOS the pipe cannot be un-stalled in place | `zune usb --reset`, or replug the Zune |
| `bulk OUT/IN timed out after …` | the device stopped answering — wedged session, or a hub that does not pass the transfers | `zune usb` (drains first), then `zune usb --reset` or replug |
| `clear_halt attempt 1/3 … failed` | the endpoint is genuinely halted | `zune usb --reset` |
| the control probe fails | the problem is below MTP: cable, hub, power, or another program holding the device | close Zune software / VMs / photo apps, check `zune usb` |
| `bulk IN failed … invalid or unsupported argument` | the IN request length was **not a multiple of the endpoint's max packet size**; nusb rejects those before they reach the host | fixed — IN lengths are rounded up (see below) |
| handshake fails with no key file | MTPZ keys are missing | see [Keys](#keys) |

Every transport failure is reported with the endpoint address and what it means; nothing
is swallowed. On startup, if the very first command of the session comes back stuck, the
driver resets the device and retries once before giving up.

### IN request lengths

`nusb` requires an IN transfer's requested length to be a nonzero multiple of the
endpoint's `max_packet_size`, and fails the transfer with `InvalidArgument`
("invalid or unsupported argument") before it reaches the host otherwise. A 512-byte
header read satisfies that on a Full-Speed link (max packet 64) and on a High-Speed one
(512), but not if the descriptor reports anything else — which is how the first read of
a session failed on macOS. `Transport::read` now rounds every request up to a multiple
and logs both numbers at `RUST_LOG=debug`.

Set `RUST_LOG=nusb=debug` (or `RUST_LOG=debug`) to see the USB backend's own decisions,
including rejections that happen client-side.

### Never await a completion that is not there

`Endpoint::wait_next_complete` **panics** when nothing is pending
(`take_completed_from_queue` → `expect("no transfer pending")`). Hand-rolling
submit + wait, and then draining with a `while wait_next_complete(..).is_some()` loop,
crashes the process the moment the queue runs empty — turning an ordinary write
timeout into a panic. Both directions therefore use `Endpoint::transfer_blocking()`,
which consumes exactly one transfer, cancels it on timeout, and never leaves one
pending, and every drain loop is guarded by `pending() > 0`.

### An unread response blocks new commands

A Zune that was interrupted mid-session keeps its response buffered on bulk IN, and
while that data is unread it refuses new commands — the write simply never completes.
`connect()` drains that leftover with one quiet read before OpenSession and reports
`» cleared N bytes left over from an earlier session`.

## Protocol notes
Wire format is little-endian, PTP-derived. Two Zune behaviours are load-bearing and are
implemented in `src/mtp.rs` / `src/device.rs`:

1. **DATA containers go out as two bulk writes** — a 12-byte header, then the payload
   as a separate transfer.
2. **A payload that is a whole multiple of 512 bytes (or empty) must be followed by a
   zero-length packet**, otherwise the device waits forever.
3. **A DATA container's header declares `12 + payload length`.** Sizing it like a
   command container (by parameter count) sends a 12-byte header followed by stray
   bytes, and the device drops the transaction with `0x201C` (TransactionCancelled).
   `Container::data_header` exists for exactly this, and a unit test pins the invariant.

The bulk endpoints also have to be un-stalled on open: a halt left by an interrupted
session makes the Zune silently ignore writes.

### MTPZ handshake order

The Zune serves no storage until this sequence completes, and the order is
load-bearing — it matches libmtp-zune exactly:

| # | operation | notes |
|---|---|---|
| 0 | `OpenSession` (0x1002) | transaction id 0 |
| 1 | `SetDevicePropValue` (0x1016) prop `0xD406` | `libmtp/Sajid Anwar - MTPZClassDriver` |
| 2 | `EndTrustedAppSession` (0x9216) | **exactly once, before the exchange** |
| 3 | `SendWMDRMPDReq` (0x9212) | RSA-signed application certificate + 16-byte nonce |
| 4 | `GetWMDRMPDResp` (0x9213) | device challenge |
| 5 | `SendWMDRMPDReq` (0x9212) | confirmation AES-CMAC |
| 6 | `EnableTrustedOps` (0x9214) | four big-endian CMAC words |

A **second** `EndTrustedAppSession` between steps 4 and 5 tears down the trusted
session the device has just established, and the next request answers `0x201C`
(`TransactionCancelled`). That is not a parameter or timing problem — it is the
device saying the transaction it was in the middle of no longer exists.

The crypto is verified offline against RFC 4493 and against the Python reference
implementation, so a handshake failure points at the protocol, not the arithmetic
(see `cargo test`; the parity test skips itself when no key file is present).

### WiFi control surface

From the Keel 3.30 firmware (`MtpSvc.dll`), the MTP opcode block `0x9220–0x9229` is the
WiFi control path, and the profile blob is passed unmodified to the ZNet client:

```
opcode 0x9225  ZNetScanAndWaitForNetworks
opcode 0x9227  ZNetSetWifiProfile(1000, buf, len) + ZNetAddWifiProfile(1007, buf, len)
opcode 0x9228  ZNetTestConnectivity / ZNetPassthroughConnect
```

Firmware side: `MTP ctx+0x50`, `1 <= len <= 0x1000`. The official Zune 4.8 client
builds this blob as `N × 0x140` (320) byte records — see `../RE_NOTES.md`.

## Layout

```
src/usb.rs      USB transport (device discovery, interface claim, bulk I/O)
src/mtp.rs      opcodes, MTP strings, containers
src/mtpz.rs     RSA/AES-CMAC/AES-CBC handshake + key file
src/device.rs   MTP session: commands, objects, properties, WiFi vendor commands
src/tags.rs     local track tags (lofty) + device-name normalisation
src/library.rs  music library: folders, tracks, playlists, albums, cover art
src/main.rs     CLI
```
