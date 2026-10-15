# zune-gui — run & review (macOS arm64)

This build has **design system v1.1**, the **component board**, and the new **Albums
page** (cover wall + detail view + cover diagnostics). It does not need a Zune on the
first pass — `ZUNE_DEMO=1` gives you a synthetic library. Then plug the Zune in to
check the cover-art fix against the real 89 albums.

## Run

```sh
tar xzf zune-gui-aarch64-apple-darwin.tar.gz     # wgpu / Metal build
./zune-gui
```

If the wgpu/Metal build misbehaves, use the OpenGL fallback:

```sh
tar xzf zune-gui-aarch64-apple-darwin-glow.tar.gz
./zune-gui
```

* Ad-hoc signed, no runtime dependencies, one file.
* **The library is cached.** The first read of a real Zune takes 2–3 minutes and is
  written to `~/Library/Application Support/zune-gui/devices/<SERIAL>/`; every later
  launch shows it at once. The top bar says `cached · N min ago` (or `live` after a
  fresh read), with the details on hover. **Refresh** is the only thing that re-reads
  the device — use it after you Push or Delete. Covers are cached too, in the same
  folder, so the album wall is instant and still works with the Zune unplugged. To
  clear it: `rm -rf "$HOME/Library/Application Support/zune-gui"`. Full details in
  `CACHE.md`.
* Track durations are read as the MTP spec defines them — **milliseconds** — so a 4:18
  track shows `4:18`, not `4299:29`, and an album total is the sum of its tracks.
* Downloaded rather than untarred? `xattr -d com.apple.quarantine ./zune-gui`.
* Quit with ⌘Q (the legacy status bar has no quit button yet).

## Pass 1 — no device needed

```sh
ZUNE_DEMO=1 ./zune-gui          # 13 synthetic albums, real JPEG covers, 2 with no art
```

* **Albums** page: cyan signature, adaptive cover wall (160–240px), sort by
  name/artist/year, live filter, and a click-through detail view (cover, artist, year,
  track count, total time, total size, cover provenance, track list).
* Two albums have **no art on the device**: they show the deliberate placeholder —
  pure black, hairline, the album's initial in display type with a cyan text glow.
* `ZUNE_DEMO=1 ZUNE_DEMO_FAIL_COVER=1 ./zune-gui` adds one **simulated failure** so you
  can see the marker: a magenta `!` on the tile, the reason on hover, and a single
  warn line in the **Log** page (`Copy to clipboard` works).

## If the Zune stops answering (read this before you retry)

A Zune 30 whose bulk property list (0x9805) is probed answers by *not answering*. That
unanswered response wedges the firmware, and **neither `zune usb` (drain + probe) nor a
USB device reset recovers it** — measured: the drain reports an empty queue, the reset
re-enumerates the port successfully, and the device still fails `OpenSession`. macOS
resets the port, not the power.

**The only recovery is to unplug the cable, wait ~3 seconds, and plug it back in.**

This build does everything it can to keep you out of that state:

* the bulk property list is probed with a **3-second** timeout (not 30), and on failure
  the session is **drained immediately**;
* the failing path is then **disabled for the rest of the session**, so a refresh never
  pays the timeout twice;
* `ZUNE_NO_BULK_PROPS=1 ./zune-gui` skips it entirely, for A/B comparison;
* if the link does end up wedged, the app stops trying: it shows a magenta banner saying
  to unplug the device, and it will not reconnect on a timer while the device is still
  plugged in.

## Pass 2 — with the Zune: the cover-art question

**Start here (seconds, not minutes): Albums page → `Diagnose covers`.**
It reads *nothing* of the track library — only the image objects and three albums — and
writes a block to the **Log** page (which has *Copy to clipboard*). Send that block back.

Two things this build changed so that button is actually reachable:

* it is **always visible and always enabled** when a device is connected — pressing it
  *interrupts* the library read (the long read stops at the next track) instead of
  waiting behind it;
* it does **not** need the library at all: no track scan, no album grouping, no storage
  info. If the library read failed, the diagnostic still runs.

What it answers, in order:

1. **Where the covers are.** It lists JPEG (0x3801) and CoverArt (0x3802) objects
   twice: once across *every* parent (`GetObjectHandles … parent=0xFFFFFFFF`) and once
   under the root. Your device has 97 cover JPEGs in the `0x03` handle segment, so the
   all-parents count is the one that matters.
2. **What an image object carries.** For the first 3 images: its `object_info` (with the
   **handle segment decoded** — `0x03 image`, `0x01 audio`, `0x06 album`, `0x07 playlist`),
   **every property it stores**, its **own references** read through *both* routes
   (opcode `0x9810` and the `ObjectReferences` property `0xDC41`), and a `GetThumb`
   (0x100A) probe.
3. **What an album actually references.** For the first 3 albums: both reference routes
   side by side, then every reference with its real format — the last line says
   `<-- IMAGE REFERENCE` or `<-- NO image reference`, so the answer is unmissable.
4. **Every folder on the device** and its child counts per format.
5. **The reverse hypothesis**: up to 60 images are scanned for references *back* to an
   album. The dump ends with either the links it found or
   `none references an album — so the link is not stored on the image side`.

Then press **Refresh** on the Albums page and read the Log:

* `library read: N tracks in Xs via …` — which property strategy was available.
* `object references: served by …` — **this is new and important**: if it says
  `nothing — both routes came back empty`, then every album looked cover-less because
  the reference read failed, not because the device lacks covers.
* `cover art: N of M albums have art on the device (…)`, or for each album without art a
  warning naming the strategy that found nothing.

Failures are explainable by construction: a refused cover says *what* the payload was
(e.g. "the object is an ASF/WMA media file (2240665 bytes), not an image").

## The component board

The app opens on **Design**. Everything below is on that one page, top to bottom:

1. **Bleeding typography** — a 72pt ExtraLight title that is allowed to run past the
   content edge and be clipped. This is intentional (Zune HD signature), not a bug.
2. **Type ladder** — display/h1/h2/h3/body/body-sm/label/italic/mono. Check that the
   steps look *extreme* rather than evenly spaced, that the italic is a real italic,
   and that the mono figures are tabular (they should line up in columns).
3. **font proof** — the same string in Inter 200/300/400/600/700 + italic + JetBrains
   Mono, so the weights can be judged side by side. Also a Chinese subtitle line: if
   it renders as boxes instead of 字形, the CJK fallback failed on this machine.
4. **Base/text + neon palette** — the v1.1 tokens. Check: canvas is `#000000`, every
   card carries a visible 1px hairline, the six neons appear only as dots/bars/numbers
   and **every one of them glows**, and primary text is pure white.
   *Spec sheets* (colour, neon palette, page signatures, glow tiers) show all six neons
   by definition — they are documentation. The **component** sections below obey the
   real rules: at most two neons in view, one per component.
5. **Glow tiers** — `glow-sm/md/lg/text` side by side, then idle/hover/press on one dot,
   so the ×1.4 radius / +0.15 alpha / ×0.8 press response can be judged.
6. **Spacing & Radius** — the 8pt grid (4…96) and radius 8/14/20/28/pill.
7. **Motion** — two knobs (spring vs cubic-bezier) that move together when you press
   **Trigger**: the spring overshoots slightly, the eased value does not. Below it,
   the three named easing curves. Softer check: after the knobs settle, the app should
   be completely idle (no fan, ~0 % CPU) — that is a hard requirement, not a nicety.
8. **GlassCard** — four cards: idle, hover, click (toggles selection), raised. Hover
   should lift the card ~2px with an *orange* halo, no black shadow. Cards have no
   borders: the only hierarchy is luminance.
9. **DevicePill** — connected / transferring (pulsing dot) / connecting (spinner arc) /
   no device. These animate continuously; that is the point of the demo.
10. **IconButton** — normal/selected/ghost/disabled/large + a primary button with the
   accent gradient, then the full 24-icon line set (1.5px strokes).
11. **ProgressRing** — 96px ring with the gradient sweep; drag the slider to watch it
    animate. Then **GradientBar** (4px, with a breathing head while active) and
    **TrackRow** (hover bar, selected, animated playing indicator, dense variant).
12. **Composition** — a small bento block to sanity-check layering
    (canvas → surface-1 → surface-2) when components sit next to each other.

The **left nav, top bar and status bar are still the old shell** (P1 replaces them);
judge the board content, not those.

## Please report back

* A screenshot (⌘⇧4 then space, click the window) — or several, if you scroll.
* Which of the two backends you ran, and whether it started.
* Anything from the checklist below that fails, plus console output if it crashed:
  `./zune-gui 2>&1 | tee zune-gui.log`

**Acceptance checklist (design spec §8.1, the v1.1 list)**

- [ ] background is pure `#000000` with no blue cast
- [ ] every card has a 1px hairline (this is what replaces v1.0's "no borders")
- [ ] neon appears only in small areas, and every neon element glows
- [ ] at most two neons on screen in the component sections
- [ ] primary text is pure `#FFFFFF`
- [ ] `display` type reaches 88pt and clips instead of wrapping
- [ ] no coloured background blocks anywhere
- [ ] idle CPU stays near zero once animations settle (press Trigger, then let it be)
- [ ] Chinese text renders as real glyphs, not boxes

## Device-side (not part of P0, for information)

The app still looks for a Zune in the background every few seconds; with one connected
it will read identity, storage, tracks and albums on the board's sibling pages
(*Collection / Albums / Device*). Those pages are functional but still themed by the
old shell, so please ignore their styling for now. MTPZ keys are read from
`$MTPZ_DATA`, `~/.mtpz-data` or `./.mtpz-data`; the Device page names the file it
loaded, or tells you which paths were searched.

## Rebuilding from Linux

```sh
SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh
```
