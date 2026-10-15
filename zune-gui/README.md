# zune-gui

A desktop client for the **Zune 30 / Zune HD**, in Rust: `egui` + `eframe`, native
window, no runtime dependencies, cross-compiled from Linux to macOS arm64.

> Status: **design system v1.1 + the Albums page**. The component board and the cover
> wall are both reviewable without a device (`ZUNE_DEMO=1`).

---

## What is in here

| Area | State |
|---|---|
| Cross-compilation Linux → macOS arm64 (glow + wgpu) | ✅ shipped in `dist/` |
| Background device thread (MTP/MTPZ over USB, mpsc event channel) | ✅ works, not yet re-skinned |
| Design system **v1.1** (pure black, neon + glow, tokens, fonts, motion) | ✅ `src/design/` |
| Component library + **component board** for visual review | ✅ `src/ui/` |
| **Albums page**: cover wall, sort/filter, detail view, cover diagnosis | ✅ P3 (first page done) |
| Other pages (Collection / Tracks / Push / Device / Wi-Fi) | 🚧 functional, still on the old theme — P4+ |
| Album cover art resolution | ✅ format-verified, with an on-device diagnostic |

## Layout

```
zune-gui/
├── assets/fonts/            Inter 4.1 variable + JetBrains Mono (both OFL) + licences
├── src/
│   ├── design/              the design system — UI_DESIGN.md is the contract
│   │   ├── tokens.rs        colour / type ladder / 8pt spacing / radius / motion + tests
│   │   ├── fonts.rs         Inter per-weight instances, JetBrains Mono, CJK fallback
│   │   ├── anim.rs          Spring, Animated<T>, Bezier easings, idle-repaint discipline
│   │   └── paint.rs         coloured glows, gradients, glass edges, dot matrix
│   ├── ui/                  components (plain functions over Ui + Motion)
│   │   ├── icons.rs         24 hand-drawn 1.5px line icons
│   │   ├── glass_card.rs  device_pill.rs  icon_button.rs  section.rs
│   │   ├── track_row.rs   progress_ring.rs  gradient_bar.rs
│   │   ├── album_tile.rs    cover tile: art, deliberate placeholder, failure badge
│   │   └── board.rs         the component board: every token and component on one page
│   ├── core/                read-only snapshot of zune-rs (see below)
│   ├── cache.rs             on-disk library cache (CACHE.md) — why a start is instant
│   ├── worker.rs            the only thread that touches USB
│   ├── model.rs             worker ↔ UI message types
│   ├── app.rs               shell: nav, top bar, status bar, page routing
│   └── theme.rs             legacy shim — delete once P3/P4 are rebuilt
├── build-macos.sh           cross-build + package dist/
└── macos-linker.sh          zig-as-linker wrapper (identical to zune-rs)
```

## Build

### Linux (development, fast iteration)

```sh
cargo run --features linux              # adds winit's X11/Wayland backends
cargo test --features linux
```

### macOS arm64 (the shipped artifact)

```sh
SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh        # both backends
BACKEND=wgpu ./build-macos.sh                            # just one
```

Produces `dist/zune-gui-aarch64-apple-darwin.tar.gz` (wgpu/Metal) and
`…-glow.tar.gz` (OpenGL fallback). Zig drives the link, the deployment target is
pinned to 11.0, and the result is an ad-hoc signed Mach-O arm64 with no external
dylibs.

### Rendering the UI without a display (how the screenshots were made)

```sh
cargo build --features linux,screenshot
EFRAME_SCREENSHOT_TO=/tmp/board.png ZUNE_WINDOW=1400x2600 \
  xvfb-run -s "-screen 0 1450x2700x24" ./target/debug/zune-gui
```

Dev-only environment variables:

| Variable | Effect |
|---|---|
| `ZUNE_WINDOW=WxH` | initial window size (one tall window fits the whole board) |
| `ZUNE_PAGE=board\|tracks\|albums\|push\|device\|wifi\|log` | start on a page |
| `ZUNE_DEMO=1` | synthetic library (13 albums, real JPEG covers, 2 without art) so the UI can be reviewed without a device |
| `ZUNE_DEMO_FAIL_COVER=1` | also simulate one cover failure, to review the failure marker |
| `ZUNE_ALBUM="Kid A"` | open an album's detail view directly |
| `ZUNE_TRACE=1` | upstream: log every USB transfer, byte level |
| `MTPZ_DATA=/path/to/.mtpz-data` | MTPZ key file (also `~/.mtpz-data`, `./.mtpz-data`) |

## Library cache (why a start is instant)

The device library is read once and written to disk, so the next launch shows it
immediately instead of spending 2–3 minutes on USB. **Refresh** is the only thing that
re-reads the device; the top bar says `cached · N min ago` or `live` so a cached view
is never mistaken for a fresh one. Covers are cached the same way. Full details —
location, invalidation policy, how to clear it, and the diff foundation for a future
Mac ↔ Zune sync — are in [CACHE.md](CACHE.md).

## Album cover art: why it is resolved the way it is

A live run on a real Zune produced this:

```
cover 0x010013c6 unavailable: decoding the cover art: The image format could not be determined
```

`0x010013c6` turned out to be a **2.2 MB WMA track**, not cover art. The old rule —
"the first reference that is not a track handle is the cover" — is unsafe twice over:
the device's own library is largely **WMA**, and `Library::tracks()` originally
enumerated **only MP3**, so every WMA track was absent from the handle set and looked
exactly like a cover-art object.

Two further facts, from the follow-up on-device check:

* the device holds **97 cover JPEGs in the `0x03` handle segment** (the high byte of a
  Zune handle encodes the object kind: `0x01` audio, `0x03` image, `0x06` abstract album,
  `0x07` playlist) and exposes **no `CoverArt` folder at all**;
* therefore the cover index no longer walks a folder: it asks for every JPEG /
  CoverArt object **across all parents** (`GetObjectHandles(storage, fmt, 0xFFFFFFFF)`),
  one call for the whole device.

`ObjectReferences` is also read **two ways** now — the opcode `0x9810` and the
`ObjectReferences` property `0xDC41` — because firmware may implement only one, and a
refused read makes every album look cover-less while the UI still lists its tracks
(those come from the album *tag*, not from references). The refresh log states which
route served the references.

The rules are:

1. **Format decides, never handle bookkeeping.** Only an object whose *format* is
   CoverArt (`0x3802`) or JPEG (`0x3801`) may be cover art
   (`cover_source_for_format`, unit-tested against MP3/WMA/AAC/WMV/MP4/playlist/etc).
2. **Enumerate every audio format the device stores** (MP3, WMA, AAC), so the library —
   and every album's track list, duration and size — is complete.
3. **Name the payload when something is wrong.** A refused download says
   *"the object is an ASF/WMA (or WMV) media file (2240665 bytes), not an image"*
   instead of a decoder error, and lands in the **Log at warn level** with a magenta
   `!` badge on the tile.
4. **Try the reverse link too.** If an album references no image, up to 200 image
   objects are scanned for references *back* to an album (`CoverSource::ReverseRef`) —
   the shape a Zune 30 actually uses — and only then is the name matched against the
   image index.
5. **Never guess when nothing carries art.** A tile with no art gets a deliberate
   placeholder (pure black, hairline, the initial in display type with a cyan text glow)
   so "no cover" never looks like "loading failed".
6. **Diagnose in seconds, not minutes.** *Diagnose covers* reads only the image objects
   and three albums, and prints the handle segment, every property, both reference
   routes and a `GetThumb` probe for each — see `README-RUN.md`. It is always clickable
   while connected: pressing it *interrupts* the library read.

### Where a cover can come from

The device is not a cover source for its own library on this firmware, so `coverfetch`
provides the fallback ladder (**disk cache → local music file → iTunes Search**), each
result labelled with its origin in the UI. Matching is deliberately conservative —
normalised names, a generated Traditional→Simplified table (`src/t2s.rs`, from OpenCC),
and a same-script artist veto — because a wrong cover is worse than none. Misses are
cached for a week; `Refresh` retries.

### The wedged-device trap

Probing the bulk property list (0x9805) on a device that does not implement it costs more
than time: the device answers by not answering, and the unanswered response wedges the
firmware hard enough that **neither a drain nor a USB device reset brings it back** — only
unplugging the cable does. So the probe now uses a **3-second** timeout, **drains**
immediately on failure, **disables the path for the session**, and the GUI tells the user
to unplug the device instead of suggesting a command that does not work
(`ZUNE_NO_BULK_PROPS=1` skips the probe entirely for A/B tests).

### What the on-device diagnosis established (Zune 30, Keel 3.30)

* **Album references carry only audio.** `GetObjectReferences` (0x9810) answers, and for
  an 11-track album it returns 11 MP3 handles — no image, ever. "The cover is in the
  album's references" is therefore false on this firmware (it is true for albums *this
  client* writes, which is why the forward strategy stays).
* **The wildcard parent query is not supported.** `GetObjectHandles(storage, JPEG,
  0xFFFFFFFF)` fails with GeneralError (0x2002); the 95 JPEG objects are answered as
  **direct children of the root**. The image index therefore asks for root children
  *first*, folders second, and treats the wildcard query as a bonus.
* **Both sides are now checked and reported**: each image's own properties (any numeric
  value that is an album handle is called out) and references, each album's properties,
  and whether any of the album's *tracks* references an image. The dump ends with a
  `SUMMARY` and a `CONCLUSION`.

`ZUNE_ALBUM`/the **Diagnose albums** button dumps the same kind of structure for albums,
and both write to the Log page, which has a *Copy to clipboard* button — facts first,
parsing second.

## Design system notes

* **v1.1: pure black + neon.** `canvas #000000` everywhere large, `surface-1 #0A0A0A`
  for cards, and **every card carries a 1px `hairline` `rgba(255,255,255,0.08)`** —
  on pure black the luminance step alone is not enough, and this deliberately reverses
  v1.0's "no borders".
* **Neon is small and always glowing.** Six neons (orange/amber/lime/cyan/magenta/
  violet), never a large fill, never two in one component, each one carrying a glow
  (`glow-sm/md/lg/text`, hover ×1.4 radius +0.15 alpha, press ×0.8). Pages own exactly
  one signature colour (`PageAccent`): Collection orange, Albums cyan, Artists magenta,
  Tracks lime, Playlist/Sync violet, transfer amber. Measured on the board screenshot:
  **69 % of pixels are exactly `#000000`, 0.6 % are saturated neon.**
* **Primary text is pure `#FFFFFF`**; secondary/tertiary are white at 62 %/38 % alpha,
  and `display` type is **88pt** ExtraLight (it bleeds past the content edge on purpose).
* **Type.** Inter is embedded **once** as a variable font and instantiated per weight
  through its `wght` axis (`epaint::FontTweak::coords`) — 200/300/400/600/700 plus
  real italics from `InterVariable-Italic.ttf`. CJK is *not* embedded: the system font
  (PingFang SC / Microsoft YaHei / Noto Sans CJK / WenQuanYi) is appended to every
  family at runtime, so Chinese metadata renders through egui's own fallback.
* **Tracking.** egui has no letter-spacing, so `label` styling is produced by
  `tokens::tracked()`, which inserts thin spaces (+0.08em) and uppercases.
* **Idle means idle.** `Motion` is the only thing that may call `request_repaint()`;
  every spring reports when it has settled. Measured: **0 CPU ticks over 6 idle
  seconds** on a static page (the component board animates on purpose — it shows live
  states such as a pulsing transfer pill).
* **egui gotchas worth remembering** (both cost real debugging time):
  * `Mesh::default()` carries the **font atlas** texture id; a custom texture must set
    `mesh.texture_id` explicitly or the quads render magnified glyph fragments.
  * `TextureWrapMode::Repeat` is not honoured for user textures on every backend
    (measured under Mesa/GL), so the dot matrix is drawn as one quad per pre-baked
    252px tile instead of a tiled shader wrap.
  * Style must be installed **after** fonts, and `eframe::App` in 0.36 takes
    `fn ui(&mut self, ui: &mut egui::Ui, frame)` (not `update(ctx, frame)`).

## `src/core/` — the zune-rs snapshot

`core/` is a **copy** of the modules the GUI needs from `zune-rs`, taken so that GUI
work could proceed while `zune-rs/src/usb.rs` was being changed by another agent.
Each file carries its source path and sha256 in the header.

* Re-synced to pick up upstream's **MTPZ CMAC fix** — the earlier hand-rolled version
  had a broken doubling step that produced wrong MACs and would have failed the
  handshake.
* Local additions are marked `ZUNE-GUI` in the source; the full list is in
  `core/mod.rs`. Nothing in `zune-rs/` is ever modified.
* To re-sync: re-copy the file, rewrite `crate::<mod>::` → `crate::core::<mod>::`, and
  re-apply the `ZUNE-GUI` hunks (see the commit-able recipe in `core/mod.rs`).

## Roadmap

* **P0 ✅** design system + component board
* **P1** app skeleton: window chrome, icon nav with animated selection, page
  transitions, background (dot matrix + ambient album colour)
* **P2** device layer surface: pill bound to real state, empty/error states —
  the worker already emits everything needed
* **P3** 🚧 **Albums done** (cover wall, sort by name/artist/year, filter, detail view,
  cover provenance + diagnostics). Collection (bento) and the Tracks list are next.
* **P4** transfer: drag & drop, queue, progress, completion toasts
* **P5** polish: micro-interactions everywhere, empty/loading/error states, keyboard
  navigation, ≥4.5:1 contrast, layout degradation at 900px
* **P6** Photos / Videos, Wi-Fi configuration, Settings + diagnostics

Each stage ends with both backends cross-compiled, `dist/` refreshed, and the Mac
tester running the artifact.

## Unverified / known limitations

* `GetStorageInfo` (0x1005) is implemented but unverified against a device; the Device
  page shows "unknown" when it is refused.
* `GetObject` (0x1009) **is** verified: the device serves cover-art-sized and track-sized
  payloads happily. `GetThumb` (0x100A) is the fallback and is still unverified.
* The album wall has not yet been exercised against a device *after* the format fix —
  that is the next Mac run.
* Usb/`nusb` behaviour is only validated on macOS so far; the Linux build has never
  been run against a Zune.
* Fonts: Inter 4.1 and JetBrains Mono are OFL (licences in `assets/fonts/`); both were
  converted from the upstream `.woff2` releases, unmodified otherwise.
