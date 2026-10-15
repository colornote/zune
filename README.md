# zune

Tools for a **Zune 30** (Microsoft, 2006) on modern macOS, written in Rust.

The device speaks MTP over USB, guarded by the **MTPZ** handshake Microsoft added in the
Zune firmware. Nothing on macOS talks to one out of the box, so this is both a protocol
implementation and a desktop client for it.

## What is here

| Path | What it is |
|---|---|
| `zune-gui/` | Desktop app (egui + eframe, Metal via wgpu). Pure black Metro-inspired UI. |
| `zune-rs/` | *(on the build host)* command-line client: library read, push, album, WiFi control. |

## Status

Working against a real Zune 30 (device version `03.30.00039.00-00435`, Keel 3.30):

- **MTPZ authentication** — the six-step handshake, `ptp_mtpz_handshake` from libmtp-zune.
- **Library read** — 2425 tracks / 14626 MB / 87 albums / 7 playlists from the device.
- **UTF-16LE strings** (`0xD406`) — Chinese filenames and tags render correctly.
- **Cover art** — see the note below; the device carries none for its own library, so the
  GUI fetches it (cached on disk afterwards).
- **Photo and video push**, playlists, album push (tracks + playlist + cover + abstract album).

## Cover art, and why it is fetched

A Zune 30's cover JPEGs are **not linked to any album**. Verified against this device:

- an album's references (`0x9810` / the `ObjectReferences` property `0xDC41`) contain
  **only audio objects** — never an image;
- the 95 JPEGs on the device sit directly under the root with camera/download filenames
  (`ht_080917_094308.jpg`, `Img219747653.jpg`);
- `CoverArt (0x3802)` is not a format this firmware answers.

So the device is not a cover *source* for its own library. `zune-gui` resolves covers in
this order, and caches every hit on disk:

1. the on-disk cache — instant, offline;
2. a local music file (embedded ID3/FLAC picture, or `cover.jpg` beside matching tags);
3. **iTunes Search** — free, keyless, official art;
4. **MusicBrainz + Cover Art Archive** — free, keyless, and better at Chinese releases.

Both online sources are rate-limited (iTunes ~20 req/min, MusicBrainz 1 req/s) and both are
matched strictly: a wrong cover is worse than no cover, so a candidate must agree on album
*and* artist before it is accepted. Traditional↔Simplified folding (`zune-gui/src/t2s.rs`)
is what lets `"04.华丽的冒险(2005.09)"` match MusicBrainz' `"華麗的冒險"`.

## Building

macOS arm64, cross-compiled from Linux or built natively:

```sh
cd zune-gui
cargo build --release --no-default-features --features wgpu   # or: glow
./build-macos.sh                                              # both, with the linker setup
```

The MTPZ key is read from `$MTPZ_DATA`, then `~/.mtpz-data`, then `./.mtpz-data`. It is the
device's authentication secret and **must not be committed** — `.gitignore` covers it.
Public copies of the key are available in
[libmtp-zune](https://github.com/kbhomes/libmtp-zune) (`src/.mtpz-data`).

## Notes

- macOS bulk USB transfers go through `nusb`; two upstream bugs are worked around in
  `zune-gui/src/core/usb.rs` (a bulk-OUT panic on completion, and residual data left in the
  IN endpoint).
- The GUI's cache lives in `~/Library/Application Support/zune-gui/`: `library.json` for the
  library snapshot and `devices/<serial>/covers/` for artwork.
