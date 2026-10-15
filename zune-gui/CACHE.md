# Library cache (why a start is instant)

Reading the real Zune library is one USB round trip per object: a 2425-track device
takes **2–3 minutes**. That is fine once, and intolerable on every launch while the UI
is being built — so the first read is written to disk, and every later launch reads
the file instead of the device.

## Where it lives

| Platform | Directory |
|---|---|
| macOS | `~/Library/Application Support/zune-gui/` |
| Linux | `$XDG_DATA_HOME/zune-gui/` (or `~/.local/share/zune-gui/`) |
| Windows | `%APPDATA%\zune-gui\` |

```text
<data-dir>/last-device                            # serial of the most recent read
<data-dir>/devices/<SERIAL>/library.json          # the whole library, pretty JSON
<data-dir>/devices/<SERIAL>/covers/<HANDLE>.jpg   # raw cover bytes, by object handle
```

Every object in `library.json` records its **handle**, the attributes the UI shows and
`read_at` (unix seconds). The file also carries the device identity/storage, its own
`read_at`, and a reserved `last_full_sync` field for the future Mac ↔ Zune sync.

`ZUNE_CACHE_DIR=/some/dir` overrides the location (used by the tests and handy for a
throwaway run).

## Invalidation & refresh policy

The cache is **never validated at start-up**. Asking the device whether the cache is
still good costs exactly the 2–3 minutes the cache exists to save.

* **Start with a cache** → it is shown immediately; the top bar says
  `cached · N min ago` (muted, text-2). No device round trips.
* **Start with no cache** → the device is read once, the cache is written, the badge
  says `live`.
* **Refresh** (top bar or Albums page) → re-read the device and overwrite the cache.
  Its tooltip says so: *"Re-read the device now and overwrite the cached library"*.
* **Keyed by device serial** → a second Zune switches to its own cache automatically,
  and two devices never share a library.
* **Older than 24 h** → the note says the data may be stale. It is a hint only;
  nothing is ever re-read automatically.
* **Older cache format** (`version` mismatch) → ignored and replaced by the next read.
* **Device unplugged** → the last library is still shown, with a `no device` dot and
  the cached badge.

Known limitation: a **write** to the device (Push / Delete) does not reconcile the
cache by itself. Press **Refresh** to re-read; otherwise the next start shows the
pre-write library. This is deliberate — the alternative is the start-up validation
above.

## How to clear it

```sh
rm -rf "$HOME/Library/Application Support/zune-gui"        # macOS
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/zune-gui"     # Linux
```

In code, `cache::clear(serial)` removes one device and `cache::clear_all()` removes
every cached device.

## Covers

A cover downloaded once is stored as raw bytes under `devices/<SERIAL>/covers/`. The
next time the album page asks for that handle the bytes are decoded from disk — no USB
round trip, and it works with the device unplugged. Only payloads that decoded as an
image are cached, so a cached lookup always decodes.

## Integration points (which existing files changed)

Almost everything lives in the new `src/cache.rs`. The edits to existing files are
small and additive:

* `Cargo.toml` — added `serde` + `serde_json`.
* `main.rs` — `mod cache;`.
* `model.rs` — added `Source`, `DataOrigin` and `Event::Origin`. `Event::Library` is
  untouched, so the library payload could not be disturbed.
* `worker.rs` — `use crate::cache;`; `run()` calls `serve_newest_cache`; the tail of
  `connect()` calls `serve_cache_for_serial` before `refresh`; new
  `serve_newest_cache` / `serve_cache_for_serial` / `apply_cache`; `refresh()` now
  builds a playlist list, writes the cache and sends an `Origin`; `fetch_cover()`
  reads/writes the cover cache; `demo_library()` sends an `Origin`. The cache write is
  skipped when the read was `interrupted` (a diagnostic), so a truncated read can
  never overwrite a good cache.
* `app.rs` — an `origin` field, the `Event::Origin` arm, `data_origin_label()` in the
  top bar, and the `Refresh` tooltip wording.

## Foundation for Mac ↔ Zune sync

The cache is built to be **diffed**, not just displayed. `LibraryCache::diff(&other)`
returns the added / removed / changed tracks, albums and playlists (identity = handle,
payload = attributes; `read_at` is provenance and is never counted as a difference).
`last_full_sync` is written through and reserved for the sync layer, and the on-disk
record shape is covered by tests so a cache round trip cannot silently drop a field.
