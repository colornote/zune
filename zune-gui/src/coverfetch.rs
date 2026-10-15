//! Cover art from somewhere other than the device.
//!
//! On a Zune 30 the cover JPEGs it exposes are not linked to any album — neither the
//! album's references, its properties, its tracks, nor the images' own properties point
//! at each other (see `diagnose_covers`). So the device is not a cover *source* for its
//! own library, and the UI has to get art elsewhere. The ladder is:
//!
//! 1. **the on-disk cache** — a cover fetched once is instant forever after;
//! 2. **a local music file** — the embedded picture (ID3 APIC / FLAC picture) of a file
//!    whose tags match the album, or a `cover.jpg` in a folder named after it;
//! 3. **iTunes Search** — free, no key, and `artworkUrl100` can be asked for a bigger
//!    rendition. Fetched with the system `curl` rather than an HTTP crate: it is present
//!    on macOS, and it keeps the TLS stack out of the cross-compiled binary.
//!
//! Everything it returns carries its [`Origin`], because the UI has to say where a cover
//! came from — "device" and "guessed from the internet" are not the same claim.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::cache;

/// Where a cover came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Already on disk from an earlier run.
    Cached,
    /// The embedded picture of a local file, or a cover image next to one.
    LocalFile(PathBuf),
    /// Downloaded from the iTunes Search API (the URL is kept for the UI).
    Online(String),
}

impl Origin {
    /// Short label for the tile / detail view.
    pub fn label(&self) -> String {
        match self {
            Origin::Cached => "cached".to_owned(),
            Origin::LocalFile(path) => format!(
                "local file · {}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "?".to_owned())
            ),
            Origin::Online(url) => {
                let is_itunes = url.contains("itunes");
                format!("online · {}", if is_itunes { "iTunes" } else { "web" })
            }
        }
    }

    pub fn fetched(&self) -> bool {
        !matches!(self, Origin::Cached)
    }
}

/// Where to look, and whether the network may be used.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Root of the local music collection (`ZUNE_MUSIC_DIR`, default `~/Music`).
    pub music_dir: Option<PathBuf>,
    /// Set `ZUNE_OFFLINE=1` to keep the app off the network entirely.
    pub online: bool,
    /// How many local files at most to examine.
    pub scan_limit: usize,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            music_dir: default_music_dir(),
            online: std::env::var_os("ZUNE_OFFLINE").is_none(),
            scan_limit: 8_000,
        }
    }
}

/// `$ZUNE_MUSIC_DIR`, else `~/Music` when it exists.
pub fn default_music_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ZUNE_MUSIC_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var_os("HOME")?;
    let music = PathBuf::from(home).join("Music");
    music.is_dir().then_some(music)
}

/// Resolve a cover for one album. Returns the image bytes and where they came from.
pub fn resolve(serial: &str, artist: &str, album: &str, sources: &Sources) -> Result<(Vec<u8>, Origin)> {
    if album.trim().is_empty() {
        bail!("the album has no name to look a cover up by");
    }

    // 1. the cache
    let path = cache_path(serial, artist, album);
    if let Ok(bytes) = std::fs::read(&path) {
        if is_image(&bytes) {
            return Ok((bytes, Origin::Cached));
        }
        log::debug!("ignoring a cached cover that is not an image: {}", path.display());
    }

    // 2. a local file
    if let Some(dir) = &sources.music_dir {
        match from_local_files(dir, artist, album, sources.scan_limit) {
            Ok(Some((bytes, source))) => {
                store(serial, artist, album, &bytes);
                return Ok((bytes, Origin::LocalFile(source)));
            }
            Ok(None) => log::debug!("no local file matched \"{artist} / {album}\""),
            Err(e) => log::warn!("local cover search failed: {e:#}"),
        }
    }

    // 3. the network
    if sources.online {
        // iTunes first: official artwork, and it can be asked for a bigger rendition. It
        // bans by IP when pushed, though, so MusicBrainz follows whenever iTunes has
        // nothing to give — and takes over completely once iTunes has refused this IP.
        match from_itunes(artist, album) {
            Ok(Some((bytes, url))) => {
                store(serial, artist, album, &bytes);
                return Ok((bytes, Origin::Online(url)));
            }
            Ok(None) => log::debug!("iTunes has no artwork for \"{artist} / {album}\""),
            Err(e) => log::warn!("iTunes lookup failed: {e:#}"),
        }
        match from_musicbrainz(artist, album) {
            Ok(Some((bytes, url))) => {
                store(serial, artist, album, &bytes);
                return Ok((bytes, Origin::Online(url)));
            }
            Ok(None) => log::debug!("MusicBrainz has no artwork for \"{artist} / {album}\""),
            Err(e) => log::warn!("MusicBrainz lookup failed: {e:#}"),
        }
    }

    Err(anyhow::Error::new(NotFound).context(format!(
        "no cover found for \"{album}\" — {}",
        if sources.online {
            "not on the device, not in the local files, not online"
        } else {
            "not on the device or in the local files (online lookup is off)"
        }
    )))
}

/// The cache file for one album: inside the cache's own covers directory, keyed by name
/// rather than by handle (the device may have no handle for this cover at all).
pub fn cache_path(serial: &str, artist: &str, album: &str) -> PathBuf {
    cache::covers_dir(serial).join(format!("album-{}.jpg", cache_key(artist, album)))
}

/// How long a "no cover anywhere" verdict is trusted before trying again.
const MISS_TTL_SECS: i64 = 7 * 24 * 60 * 60;

fn miss_path(dir: &Path, artist: &str, album: &str) -> PathBuf {
    dir.join(format!("album-{}.miss", cache_key(artist, album)))
}

/// Has this album failed to find a cover recently? Keeps a device with no art from
/// hammering iTunes on every refresh.
pub fn recently_missed(serial: &str, artist: &str, album: &str) -> bool {
    recently_missed_in(&cache::covers_dir(serial), artist, album)
}

/// Record that this album has no cover anywhere, and when.
pub fn note_miss(serial: &str, artist: &str, album: &str) {
    note_miss_in(&cache::covers_dir(serial), artist, album);
}

/// Forget the recorded misses, so the next pass retries (used by Refresh).
pub fn clear_misses(serial: &str) -> usize {
    clear_misses_in(&cache::covers_dir(serial))
}

fn recently_missed_in(dir: &Path, artist: &str, album: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(miss_path(dir, artist, album)) else {
        return false;
    };
    let Ok(stamp) = text.trim().parse::<i64>() else { return false };
    cache::now_unix() - stamp < MISS_TTL_SECS
}

fn note_miss_in(dir: &Path, artist: &str, album: &str) {
    let path = miss_path(dir, artist, album);
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::debug!("could not create {}: {e}", dir.display());
        return;
    }
    if let Err(e) = std::fs::write(&path, cache::now_unix().to_string()) {
        log::debug!("could not record the cover miss for \"{album}\": {e}");
    }
}

fn clear_misses_in(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "miss").unwrap_or(false) && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn store(serial: &str, artist: &str, album: &str, bytes: &[u8]) {
    let path = cache_path(serial, artist, album);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::debug!("could not create {}: {e}", parent.display());
            return;
        }
    }
    match std::fs::write(&path, bytes) {
        Ok(()) => log::info!("cached cover art for \"{album}\" ({} bytes)", bytes.len()),
        Err(e) => log::debug!("could not cache the cover for \"{album}\": {e}"),
    }
}

// ── local files ─────────────────────────────────────────────────────────────

/// Look for a matching local file and take its embedded picture.
fn from_local_files(dir: &Path, artist: &str, album: &str, limit: usize) -> Result<Option<(Vec<u8>, PathBuf)>> {
    use lofty::file::TaggedFileExt;
    use lofty::prelude::Accessor;
    use lofty::probe::Probe;

    let want_album = normalise(album);
    if want_album.is_empty() {
        return Ok(None);
    }
    let want_artist = normalise(artist);
    let mut examined = 0usize;
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        let mut files: Vec<PathBuf> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }

        // A folder named after the album: `cover.jpg` inside it is the usual convention,
        // and it costs one stat rather than a tag read.
        for folder in &dirs {
            let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if normalise(&name) == want_album {
                if let Some(bytes) = cover_image_in(folder) {
                    return Ok(Some((bytes, folder.clone())));
                }
            }
        }

        for file in files {
            if examined >= limit {
                bail!("stopped after examining {limit} files");
            }
            if !is_audio(&file) {
                continue;
            }
            if std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) > 40 * 1024 * 1024 {
                continue;
            }
            examined += 1;
            let Ok(tagged) = Probe::open(&file).and_then(|p| p.read()) else { continue };
            let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { continue };

            let album_tag = normalise(tag.album().as_deref().unwrap_or_default());
            if album_tag != want_album {
                continue;
            }
            if !want_artist.is_empty() {
                let artist_tag = tag
                    .artist()
                    .or_else(|| tag.get_string(lofty::prelude::ItemKey::AlbumArtist).map(|v| v.to_string().into()))
                    .map(|v| normalise(&v))
                    .unwrap_or_default();
                // A compilation may list a different track artist; only reject when the
                // tags clearly name someone else.
                if !artist_tag.is_empty() && artist_tag != want_artist {
                    continue;
                }
            }
            if let Some(picture) = tag.pictures().first() {
                let bytes = picture.data().to_vec();
                if is_image(&bytes) {
                    return Ok(Some((bytes, file)));
                }
            }
        }
        for d in dirs {
            stack.push(d);
        }
    }
    Ok(None)
}

/// `cover.jpg` / `folder.jpg` / `front.jpg` inside a directory, if any.
fn cover_image_in(dir: &Path) -> Option<Vec<u8>> {
    for name in ["cover.jpg", "folder.jpg", "front.jpg", "cover.jpeg", "album.jpg", "AlbumArtSmall.jpg"] {
        let candidate = dir.join(name);
        if let Ok(bytes) = std::fs::read(&candidate) {
            if is_image(&bytes) {
                return Some(bytes);
            }
        }
    }
    None
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let ext = e.to_string_lossy().to_lowercase();
            crate::core::library::AUDIO_EXTS.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

// ── online (iTunes Search) ──────────────────────────────────────────────────

/// The iTunes Search API allows roughly 20 requests a minute, MusicBrainz one a second.
/// The first version had no floor at all and fired 24 searches in 12 seconds: iTunes
/// answered 429, then 403, and a whole library came back empty for the rest of the run.
/// A few one-off minutes is a fair price for a 90-album library.
const ITUNES_INTERVAL: Duration = Duration::from_millis(3_000);
/// MusicBrainz publishes "one request per second", but a run that keeps to exactly that
/// still collected HTTP 503s: the limit is an average, and four prefetch threads asking
/// in turn hit the burst ceiling. Two seconds is comfortably inside it, and a scan that is
/// a little slower beats one that gets the whole run refused.
const MUSICBRAINZ_INTERVAL: Duration = Duration::from_millis(2_000);

static LAST_QUERY: Mutex<Option<Instant>> = Mutex::new(None);

/// Block until the next search is allowed. Process-wide on purpose: the GUI fetches from a
/// single worker thread today, but nothing should be able to stampede an API even if a
/// second caller appears.
fn throttle_queries(interval: Duration) {
    let mut last = LAST_QUERY.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(previous) = *last {
        let waited = previous.elapsed();
        if waited < interval {
            std::thread::sleep(interval - waited);
        }
    }
    *last = Some(Instant::now());
}

/// A local HTTP proxy, if one is listening.
///
/// Cover Art Archive answers with a redirect to `archive.org`, which is unreachable from
/// this network without one — the API query itself works either way. The port is probed
/// once, on the first cover download, and the answer kept. `ZUNE_PROXY` overrides the
/// guess, and `ZUNE_PROXY=none` forces a direct connection.
fn local_proxy() -> Option<&'static str> {
    static PROXY: OnceLock<Option<String>> = OnceLock::new();
    PROXY
        .get_or_init(|| {
            if let Ok(value) = std::env::var("ZUNE_PROXY") {
                let value = value.trim().to_owned();
                return if value.is_empty() || value.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(value)
                };
            }
            let address: std::net::SocketAddr = "127.0.0.1:7890".parse().ok()?;
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(300))
                .ok()
                .map(|_| "http://127.0.0.1:7890".to_owned())
        })
        .as_deref()
}

/// No cover exists for this album anywhere the ladder looked.
///
/// A definite answer, and the only kind worth remembering as a miss: a timeout or a 503
/// will probably be different on the next run, and caching one of those would leave an
/// album without its cover for a week.
#[derive(Debug)]
pub struct NotFound;

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no cover was found on the device, in the local files, or online")
    }
}

impl std::error::Error for NotFound {}

/// Was this a definite "there is no cover", rather than a network hiccup?
pub fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<NotFound>().is_some())
}

/// The API refused us rather than not having the artwork. The difference matters: a
/// rate-limited album has to be retried on a later run, not remembered as a miss.
#[derive(Debug)]
pub struct RateLimited;

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the iTunes Search API is rate-limiting us — its covers fill in on a later run",
        )
    }
}

impl std::error::Error for RateLimited {}

/// Did this failure come from the API saying "not now" rather than "not found"?
pub fn is_rate_limited(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<RateLimited>().is_some())
}

/// Ask the iTunes Search API for the album artwork.
fn from_itunes(artist: &str, album: &str) -> Result<Option<(Vec<u8>, String)>> {
    let term = if artist.trim().is_empty() {
        album.to_owned()
    } else {
        format!("{artist} {album}")
    };
    // Ask for several results, not one. iTunes ranks by its own relevance, which is often
    // a live album, a tribute band or a same-titled record by somebody else: for
    // "Pink Floyd / Dark Side of the Moon" the first hit is an EP by Moon Byul. The right
    // answer was in the list, just not first.
    let url = format!(
        "https://itunes.apple.com/search?term={}&entity=album&limit=10",
        url_encode(&term)
    );
    let body = curl(&url).context("querying the iTunes Search API")?;
    let candidates = itunes_candidates(&body);
    if candidates.is_empty() {
        return Ok(None);
    }
    // Take the first candidate that actually describes this album; a wrong cover is worse
    // than no cover, so the check stays strict.
    let Some(candidate) = candidates
        .iter()
        .find(|c| is_match(album, artist, &c.collection, &c.artist))
    else {
        let seen: Vec<String> = candidates
            .iter()
            .take(3)
            .map(|c| format!("\"{}\" by \"{}\"", c.collection, c.artist))
            .collect();
        log::info!(
            "iTunes had nothing close for \"{artist} / {album}\" — {} candidate(s), none matched: {}",
            candidates.len(),
            seen.join(", ")
        );
        return Ok(None);
    };
    let large = larger_artwork(&candidate.artwork);
    let bytes = curl_bytes(&large).with_context(|| format!("downloading {large}"))?;
    if !is_image(&bytes) {
        bail!("the artwork URL did not return an image ({} bytes)", bytes.len());
    }
    Ok(Some((bytes, large)))
}

// ── online: MusicBrainz + Cover Art Archive ─────────────────────────────────

/// MusicBrainz asks for a descriptive agent; this is its documented example shape.
const MUSICBRAINZ_AGENT: &str = "zune-gui/0.1.0 (https://github.com/zune-gui)";

/// Ask MusicBrainz for the release, then Cover Art Archive for its front cover.
///
/// A second online source is not redundancy. iTunes bans by IP, and one bad burst makes
/// every remaining cover unreachable for hours — which is exactly what happened here. And
/// a Chinese release is where iTunes is weakest anyway: it holds translated titles or
/// romanised artists, while MusicBrainz keeps the original.
fn from_musicbrainz(artist: &str, album: &str) -> Result<Option<(Vec<u8>, String)>> {
    let cleaned = clean_album_name(album);
    let query = if artist.trim().is_empty() {
        format!("release:\"{cleaned}\"")
    } else {
        // Quoting each field keeps a stray "AND" inside a title from acting as one.
        format!("release:\"{cleaned}\" AND artist:\"{}\"", artist.trim())
    };
    let url = format!(
        "https://musicbrainz.org/ws/2/release?query={}&fmt=json&limit=5",
        url_encode(&query)
    );
    let body = curl_musicbrainz(&url).context("querying MusicBrainz")?;
    let Some(mbid) = musicbrainz_release(&body, artist, album) else {
        log::debug!("MusicBrainz has no close release for \"{artist} / {album}\"");
        return Ok(None);
    };
    let cover_url = format!("https://coverartarchive.org/release/{mbid}/front-500");
    let bytes = curl_image(&cover_url).with_context(|| format!("downloading {cover_url}"))?;
    if !is_image(&bytes) {
        // Cover Art Archive answers a release with no cover art with a 404 and a short
        // text body. That is "nothing here", not a failure: fall through to the other
        // sources rather than recording it as a broken download.
        log::debug!(
            "Cover Art Archive has no image for {mbid} ({} bytes) — trying the next source",
            bytes.len()
        );
        return Ok(None);
    }
    Ok(Some((bytes, cover_url)))
}

/// The release that actually matches, from a MusicBrainz search response.
///
/// Its JSON is nested — an artist arrives through an `artist-credit` array — so this is
/// the one place where a real parser earns its keep over the hand-rolled scanner that
/// serves the flat iTunes response.
fn musicbrainz_release(json: &str, artist: &str, album: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(json).ok()?;
    for release in parsed.get("releases")?.as_array()? {
        let Some(id) = release.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let title = release.get("title").and_then(|v| v.as_str()).unwrap_or_default();
        // The credit's own `name` is the one to compare: it is what a person would call
        // the artist ("Simon & Garfunkel", "周杰倫").
        let credited = release
            .get("artist-credit")
            .and_then(|v| v.as_array())
            .map(|credits| {
                credits
                    .iter()
                    .filter_map(|credit| credit.get("name").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(" & ")
            })
            .unwrap_or_default();
        if is_match(album, artist, title, &credited) {
            return Some(id.to_owned());
        }
        log::debug!(
            "MusicBrainz offered \"{title}\" by \"{credited}\" for \"{artist} / {album}\" — not close enough"
        );
    }
    None
}

/// A MusicBrainz query. Its API policy asks for a real User-Agent and one request a
/// second; both are honoured here. A 503 means the server is refusing rather than
/// answering, so one slow retry is worth making before the album is written off.
fn curl_musicbrainz(url: &str) -> Result<String> {
    match curl_musicbrainz_once(url) {
        Err(error) if is_server_busy(&error) => {
            log::debug!("MusicBrainz answered 503 — retrying once after a pause");
            std::thread::sleep(Duration::from_secs(4));
            curl_musicbrainz_once(url)
        }
        other => other,
    }
}

fn curl_musicbrainz_once(url: &str) -> Result<String> {
    throttle_queries(MUSICBRAINZ_INTERVAL);
    curl_status(url, MUSICBRAINZ_AGENT, false)
}

/// Did the server say "not now" rather than "no"?
fn is_server_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ServerBusy>()
            .is_some()
    })
}

/// HTTP 503 from an online source: worth one retry, unlike a 404.
#[derive(Debug)]
pub struct ServerBusy;

impl std::fmt::Display for ServerBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the server answered 503 (busy or overloaded)")
    }
}

impl std::error::Error for ServerBusy {}

/// Download an image, following redirects and going through the local proxy when there
/// is one.
///
/// Cover Art Archive answers 307 towards `archive.org`, which is unreachable from this
/// network directly — the proxy is what makes cover downloads work at all.
///
/// Deliberately not throttled with the search calls: it is a different host with no
/// published rate, and the transfer is dominated by how slowly a single connection
/// through the proxy gets going. Several albums downloading at once share that latency
/// instead of each paying it in turn (see [`prefetch`]).
fn curl_image(url: &str) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.args(["-sSL", "--max-time", "25", "-A", MUSICBRAINZ_AGENT]);
    if let Some(proxy) = local_proxy() {
        command.args(["-x", proxy]);
    }
    command.arg(url);
    let output = command
        .output()
        .with_context(|| format!("running curl for {url}"))?;
    if !output.status.success() {
        bail!("curl failed for {url} ({})", output.status);
    }
    Ok(output.stdout)
}

/// Warm the cover cache for a whole library, several albums at a time.
///
/// One album takes about seventeen seconds, nearly all of it the download: Cover Art
/// Archive redirects to `archive.org`, reachable only through the proxy, and a single
/// connection there is slow to start. Four at once finish in about the same wall time as
/// one, so the library fills in five times sooner. The one-a-second MusicBrainz limit
/// still holds, because [`throttle_queries`] is process-wide and the searches queue up
/// behind each other no matter how many threads ask.
///
/// Called from a thread of its own; it returns when every album has been attempted.
pub fn prefetch(serial: &str, albums: &[(String, String)], sources: &Sources) {
    /// Enough to hide the per-connection latency without turning into a flood.
    const WORKERS: usize = 4;
    if albums.is_empty() {
        return;
    }
    let queue = Mutex::new(albums.iter().collect::<Vec<_>>());
    let workers = WORKERS.min(albums.len());
    log::debug!("prefetching covers for {} album(s) on {workers} threads", albums.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                // The lock is held only long enough to take the next album.
                let next = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop();
                let Some((artist, album)) = next else { break };
                if recently_missed(serial, artist, album) {
                    continue;
                }
                if let Err(error) = resolve(serial, artist, album, sources) {
                    log::debug!("prefetch \"{artist} / {album}\": {error:#}");
                }
            });
        }
    });
}

/// The first result of an iTunes Search response: the artwork URL plus the titles we need
/// to check it against. JSON-parsed by hand — three fields are not worth a serde
/// dependency.
#[derive(Debug)]
pub struct ItunesCandidate {
    pub artwork: String,
    pub collection: String,
    pub artist: String,
}

pub fn itunes_candidate(json: &str) -> Option<ItunesCandidate> {
    itunes_candidates(json).into_iter().next()
}

/// Every album in an iTunes Search response, in the order iTunes ranked them.
///
/// Each result object opens with `"wrapperType"`, so slicing on that key yields one chunk
/// per entry — and within a chunk the first `"collectionName"` is that entry's own, which
/// is what lets the hand-rolled scanner stay this small.
pub fn itunes_candidates(json: &str) -> Vec<ItunesCandidate> {
    // Nothing found is a normal answer, not an error.
    if json.contains("\"resultCount\":0") {
        return Vec::new();
    }
    json.split("\"wrapperType\"")
        .skip(1)
        .filter_map(|chunk| {
            Some(ItunesCandidate {
                artwork: string_field(chunk, "artworkUrl100")?,
                collection: string_field(chunk, "collectionName").unwrap_or_default(),
                artist: string_field(chunk, "artistName").unwrap_or_default(),
            })
        })
        .collect()
}

/// The first `"key":"value"` in a JSON document, with `\/` unescaped. The first hit is the
/// first result, which is what `limit=1` asks for.
fn string_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => {
                // \" or \/ or \\ — keep the escaped character itself.
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            other => out.push(other),
        }
    }
    None
}

/// iTunes serves the same artwork at several sizes; ask for a big one.
pub fn larger_artwork(url: &str) -> String {
    for (small, big) in [
        ("100x100bb", "600x600bb"),
        ("60x60bb", "600x600bb"),
        ("30x30bb", "600x600bb"),
        ("170x170bb", "600x600bb"),
    ] {
        if url.contains(small) {
            return url.replace(small, big);
        }
    }
    url.to_owned()
}

fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// iTunes refused this IP earlier in the run. The ban outlives a single request, so once
/// it happens every later album goes straight to MusicBrainz rather than waiting out a
/// three-second throttle only to be told "no" again.
static ITUNES_BANNED: AtomicBool = AtomicBool::new(false);

/// Run curl and return the body, turning the HTTP status into an error that names it.
fn curl_status(url: &str, agent: &str, proxy: bool) -> Result<String> {
    let mut command = Command::new("curl");
    command.args(["-sS", "--max-time", "15", "-w", "\n%{http_code}", "-A", agent]);
    if proxy {
        if let Some(proxy) = local_proxy() {
            command.args(["-x", proxy]);
        }
    }
    command.arg(url);
    let output = command
        .output()
        .with_context(|| format!("running curl for {url}"))?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    // `-w` appends the status on its own line; `--fail` is deliberately absent so the code
    // can be read, instead of every failure collapsing into "exit status 22".
    let (body, code) = match text.rsplit_once('\n') {
        Some((body, code)) => (body, code.trim()),
        None => (text.as_str(), "?"),
    };
    match code {
        "200" => Ok(body.to_owned()),
        "403" | "429" => bail!(RateLimited),
        "503" => bail!(ServerBusy),
        other => bail!("curl for {url} answered HTTP {other}"),
    }
}

/// `curl`, because it is already on macOS and keeps a TLS stack out of the binary.
fn curl(url: &str) -> Result<String> {
    if ITUNES_BANNED.load(Ordering::Relaxed) {
        bail!("iTunes refused this IP earlier in the run");
    }
    throttle_queries(ITUNES_INTERVAL);
    // Checked twice on purpose. The prefetch runs four threads at once, and with a plain
    // flag read before the throttle they all saw "false" and all went to the API — the
    // ban was discovered four separate times. The throttle is a mutex, so whichever
    // thread gets through next sees the verdict the previous one recorded.
    if ITUNES_BANNED.load(Ordering::Relaxed) {
        bail!("iTunes refused this IP a moment ago");
    }
    match curl_status(url, "zune-gui/0.1", false) {
        Err(error) => {
            if error
                .chain()
                .any(|cause| cause.downcast_ref::<RateLimited>().is_some())
            {
                ITUNES_BANNED.store(true, Ordering::Relaxed);
                log::warn!(
                    "iTunes is refusing this IP — MusicBrainz will answer for the rest of this run"
                );
            }
            Err(error)
        }
        ok => ok,
    }
}

fn curl_bytes(url: &str) -> Result<Vec<u8>> {
    let output = curl_command(url).output().with_context(|| format!("running curl for {url}"))?;
    if !output.status.success() {
        bail!("curl failed for {url} ({})", output.status);
    }
    Ok(output.stdout)
}

fn curl_command(url: &str) -> Command {
    let mut command = Command::new("curl");
    command.args(["-sSL", "--fail", "--max-time", "12", "-A", "zune-gui/0.1", url]);
    command
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Clean a device album name into something a search API will recognise:
/// `"04.华丽的冒险(2005.09)"` → `"华丽的冒险"`, `"01 - Kid A [Remastered]"` → `"Kid A"`.
pub fn clean_album_name(name: &str) -> String {
    let mut text = name.trim().to_owned();
    // A leading track/disc number: "04.", "04 -", "1.", "01)"
    let stripped = text.trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace() || c == '.' || c == '-' || c == '_' || c == ')');
    if stripped.len() >= 2 {
        text = stripped.to_owned();
    }
    // Bracketed noise: years, "Remastered", "Deluxe Edition", the Zune software's dates.
    for (open, close) in [('(', ')'), ('[', ']'), ('（', '）'), ('【', '】')] {
        while let Some(start) = text.find(open) {
            let Some(end) = text[start..].find(close) else { break };
            text.replace_range(start..start + end + close.len_utf8(), " ");
        }
    }
    // A trailing year on its own, e.g. "Kid A 2000".
    let trimmed = text.trim();
    if let Some((head, tail)) = trimmed.rsplit_once(' ') {
        if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit()) && head.len() >= 2 {
            text = head.to_owned();
        }
    }
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches(|c: char| c == '-' || c == '.' || c == '_' || c.is_whitespace());
    if cleaned.is_empty() {
        name.trim().to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// How well two names agree, 0.0..=1.0, after folding case, punctuation, accents and
/// Traditional → Simplified characters.
pub fn similarity(ours: &str, theirs: &str) -> f32 {
    let a = normalise(ours);
    let b = normalise(theirs);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    // One side carrying a subtitle is still a match — but only when the shorter name is
    // most of the longer one and sits at one end of it. "Kid A" must not match
    // "Kid A Mnesia", while bracket noise is already stripped by `clean_album_name`.
    let (shorter, longer) = if a.chars().count() <= b.chars().count() { (&a, &b) } else { (&b, &a) };
    let short_len = shorter.chars().count();
    let long_len = longer.chars().count();
    if short_len >= 4
        && short_len as f32 / long_len as f32 >= 0.6
        && (longer.starts_with(shorter.as_str()) || longer.ends_with(shorter.as_str()))
    {
        return 0.85;
    }
    // Otherwise: how many characters are shared, counted as a multiset.
    let mut pool: Vec<char> = b.chars().collect();
    let mut hits = 0usize;
    for ch in a.chars() {
        if let Some(index) = pool.iter().position(|c| *c == ch) {
            pool.swap_remove(index);
            hits += 1;
        }
    }
    let ratio = hits as f32 / a.chars().count().max(b.chars().count()) as f32;
    // Long CJK titles share few characters when the folding table misses one, so require
    // a decent overlap rather than an exact match.
    if ratio >= 0.7 {
        ratio
    } else {
        0.0
    }
}

/// Is the returned album actually the one we asked for?
///
/// Getting this wrong means showing the wrong cover, which is worse than showing none, so
/// the bar is deliberately high (0.7) and the artist is only a tie-breaker.
pub fn is_match(album: &str, artist: &str, collection: &str, returned_artist: &str) -> bool {
    // Both sides get the same clean-up. iTunes pads its titles with edition markers
    // ("Hybrid Theory (Deluxe Edition)", "Far from Refuge (Remastered)") that would
    // otherwise sink an exact match: the raw strings share only half their characters.
    let album_score = similarity(&clean_album_name(album), &clean_album_name(collection));
    let artist_score = similarity(artist, returned_artist);

    // A cover by somebody else is worse than no cover, so a *clearly* different artist
    // vetoes an album-name match. This only fires when both sides named an artist and
    // they disagree; a compilation or a romanised name simply scores low and is ignored.
    // Only veto on a same-script disagreement. iTunes romanises CJK artists — it answers
    // "Cheer Chen" for 陈绮贞 — and character similarity across scripts is meaningless, so
    // a Latin-vs-CJK difference is treated as "no opinion" rather than as a conflict.
    if !artist.trim().is_empty()
        && !returned_artist.trim().is_empty()
        && same_script(artist, returned_artist)
        && artist_score < 0.5
        && album_score >= 0.7
    {
        return false;
    }
    if album_score >= 0.7 {
        return true;
    }
    // The album name alone was not enough: accept only if the artist agrees strongly *and*
    // the album names clearly overlap.
    album_score >= 0.4 && artist_score >= 0.8
}

/// Are these two names written in the same script? Latin vs CJK carries no signal.
fn same_script(a: &str, b: &str) -> bool {
    let has_cjk = |text: &str| {
        text.chars()
            .any(|c| ('\u{3400}'..='\u{9FFF}').contains(&c) || ('\u{F900}'..='\u{FAFF}').contains(&c))
    };
    has_cjk(a) == has_cjk(b)
}

/// A stable key for one album, used for the cache file name and the UI's map key.
pub fn cache_key(artist: &str, album: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in artist.trim().to_lowercase().bytes().chain([0u8]).chain(album.trim().to_lowercase().bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("{hash:016x}")
}

/// A synthetic cover key for the UI's texture map: real device handles have a small high
/// byte (0x01 audio, 0x03 image, 0x06 album …), so the top bit is free.
pub fn synthetic_key(artist: &str, album: &str) -> u32 {
    let key = cache_key(artist, album);
    let value = u32::from_str_radix(&key[..8], 16).unwrap_or(0);
    value | 0x8000_0000
}

/// True when the bytes start with an image signature we can decode.
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(&[0x89, b'P', b'N', b'G'])
        || bytes.starts_with(b"GIF8")
        || bytes.starts_with(b"BM")
}

/// Lowercase, drop punctuation: `"Björk"` and `"bjork"` both → `bjork`, and a leading
/// track number or "The " does not matter.
pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        // Fold the accents a Latin-1 style name is likely to carry.
        let folded = match ch {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
            'ç' | 'Ç' => 'c',
            'ñ' | 'Ñ' => 'n',
            // Traditional → simplified, so an iTunes answer in Traditional Chinese can
            // match the device's Simplified album name.
            other => crate::t2s::fold(other),
        };
        if folded.is_alphanumeric() {
            for lower in folded.to_lowercase() {
                out.push(lower);
            }
        }
    }
    // A leading "the " is noise on both sides of a comparison.
    out.strip_prefix("the").map(|rest| rest.to_owned()).unwrap_or(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_urls_are_extracted_and_enlarged() {
        // A trimmed real response: the only field we care about, with its escaping.
        let json = r#"{"resultCount":1,"results":[{"wrapperType":"collection","artistName":"Radiohead","collectionName":"Kid A","artworkUrl60":"https:\/\/is1-ssl.mzstatic.com\/image\/thumb\/Music\/x\/60x60bb.jpg","artworkUrl100":"https:\/\/is1-ssl.mzstatic.com\/image\/thumb\/Music\/x\/100x100bb.jpg"}]}"#;
        let url = itunes_candidate(json).expect("artworkUrl100 must be found").artwork;
        assert!(url.starts_with("https://"), "{url}");
        assert!(!url.contains("\\/"), "escaping must be undone: {url}");
        let large = larger_artwork(&url);
        assert!(large.contains("600x600bb"), "{large}");
        assert!(!large.contains("100x100bb"), "{large}");
        // A response with no artwork at all is not an error, just nothing.
        assert!(itunes_candidate(r#"{"resultCount":0,"results":[]}"#).is_none());
    }

    #[test]
    fn queries_are_url_encoded() {
        assert_eq!(url_encode("Kid A"), "Kid+A");
        assert_eq!(url_encode("Björk"), "Bj%C3%B6rk");
        assert_eq!(url_encode("D'Angelo & The Vanguard"), "D%27Angelo+%26+The+Vanguard");
    }

    #[test]
    fn album_names_compare_across_punctuation_and_accents() {
        assert_eq!(normalise("Björk"), "bjork");
        assert_eq!(normalise("bjork"), "bjork");
        assert_eq!(normalise("04.华丽的冒险(2005.09)"), "04华丽的冒险200509");
        assert_eq!(normalise("The Miseducation of Lauryn Hill"), "miseducationoflaurynhill");
        assert_eq!(normalise("The Beatles"), normalise("Beatles"));
        assert_eq!(normalise("Come On Eileen!"), "comeoneileen");
    }

    #[test]
    fn cover_keys_are_stable_and_name_only_covers_do_not_collide() {
        let a = cache_key("Radiohead", "Kid A");
        let b = cache_key("radiohead ", "kid a");
        assert_eq!(a, b, "case and padding must not matter");
        assert_ne!(a, cache_key("Radiohead", "Amnesiac"));
        // The synthetic UI key must never look like a real device handle.
        let synthetic = synthetic_key("Radiohead", "Kid A");
        assert!(synthetic & 0x8000_0000 != 0);
        assert_ne!(synthetic, synthetic_key("Radiohead", "Amnesiac"));
    }

    #[test]
    fn only_real_images_are_accepted() {
        assert!(is_image(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_image(&[0x89, b'P', b'N', b'G', 0x0D]));
        assert!(!is_image(b"ID3\x04\x00"));
        assert!(!is_image(&[0x30, 0x26, 0xB2, 0x75]));
        assert!(!is_image(&[]));
    }

    #[test]
    fn a_folder_named_after_the_album_provides_its_cover() {
        let dir = std::env::temp_dir().join(format!("zune-coverfetch-{}", std::process::id()));
        let album_dir = dir.join("Kid A");
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("cover.jpg"), [0xFF, 0xD8, 0xFF, 0xE0, 0x00]).unwrap();

        let found = from_local_files(&dir, "Radiohead", "Kid A", 100).unwrap();
        assert!(found.is_some(), "a cover.jpg in a matching folder must be found");
        let (bytes, source) = found.unwrap();
        assert!(is_image(&bytes));
        assert_eq!(source, album_dir);

        // An album with no matching folder finds nothing.
        assert!(from_local_files(&dir, "Radiohead", "Amnesiac", 100).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn device_album_names_are_cleaned_before_searching() {
        // The live case: the Zune stores "04.华丽的冒险(2005.09)"; the search term has to
        // be "华丽的冒险".
        assert_eq!(clean_album_name("04.华丽的冒险(2005.09)"), "华丽的冒险");
        assert_eq!(clean_album_name("01 - Kid A"), "Kid A");
        assert_eq!(clean_album_name("01. Kid A"), "Kid A");
        assert_eq!(clean_album_name("Kid A [Remastered]"), "Kid A");
        assert_eq!(clean_album_name("Kid A (2000)"), "Kid A");
        assert_eq!(clean_album_name("Kid A 2000"), "Kid A");
        assert_eq!(clean_album_name("Kid A"), "Kid A");
        // Nothing to clean: unchanged, not emptied.
        assert_eq!(clean_album_name("A"), "A");
        assert_eq!(clean_album_name("1979"), "1979");
    }

    #[test]
    fn traditional_and_simplified_names_compare_equal() {
        // iTunes answered 華麗的冒險 for 华丽的冒险 — without folding, nothing matches.
        assert_eq!(normalise("華麗的冒險"), normalise("华丽的冒险"));
        assert!(similarity("华丽的冒险", "華麗的冒險") >= 0.99);
        assert_eq!(normalise("陳綺貞"), normalise("陈绮贞"));
        assert!(is_match("04.华丽的冒险(2005.09)", "陈绮贞", "華麗的冒險", "Cheer Chen"));
    }

    #[test]
    fn a_wrong_artist_vetoes_a_matching_album_name() {
        // Same album title, different band: take no cover rather than the wrong one.
        assert!(!is_match("Greatest Hits", "Radiohead", "Greatest Hits", "Queen"));
        // The same artist under a different name form is fine.
        assert!(is_match("Kid A", "Radiohead", "Kid A", "Radiohead"));
        // Album names that merely start alike must not pass.
        assert!(!is_match("Kid A", "Radiohead", "Kid A Mnesia", "Radiohead"));
    }

    #[test]
    fn a_later_candidate_is_used_when_itunes_ranks_a_wrong_one_first() {
        // The live shape: the top hit for "Pink Floyd / Dark Side of the Moon" is an EP by
        // somebody else, and the record we want is second in the list.
        let json = r#"{"resultCount":2,"results":[
            {"wrapperType":"collection","artistName":"Moon Byul","collectionName":"Dark Side of the Moon - EP","artworkUrl100":"https:\/\/x\/100x100bb.jpg"},
            {"wrapperType":"collection","artistName":"Pink Floyd","collectionName":"The Dark Side of the Moon","artworkUrl100":"https:\/\/y\/100x100bb.jpg"}
        ]}"#;
        let candidates = itunes_candidates(json);
        assert_eq!(candidates.len(), 2, "both entries must be parsed: {candidates:?}");
        let chosen = candidates
            .iter()
            .find(|c| is_match("Dark Side of the Moon", "Pink Floyd", &c.collection, &c.artist))
            .expect("the second candidate is the real album");
        assert!(chosen.artwork.contains("/y/"), "wrong entry chosen: {}", chosen.artwork);
        assert!(!is_match("Dark Side of the Moon", "Pink Floyd", &candidates[0].collection, &candidates[0].artist));
    }

    #[test]
    fn edition_markers_on_the_returned_title_do_not_block_a_match() {
        // iTunes pads its titles; comparing them raw shares only half the characters.
        assert!(is_match("Hybrid Theory", "Linkin Park", "Hybrid Theory (Deluxe Edition)", "LINKIN PARK"));
        assert!(is_match("Far from Refuge", "God Is an Astronaut", "Far from Refuge (Remastered)", "God Is an Astronaut"));
        assert!(is_match("Echo", "Leona Lewis", "Echo (Deluxe Version)", "Leona Lewis"));
        // Padding is not a licence to match anything: a different album still fails.
        assert!(!is_match("Blackout", "Britney Spears", "Circus (Deluxe Version)", "Britney Spears"));
    }

    #[test]
    fn a_miss_is_remembered_and_can_be_cleared() {
        // Uses an explicit directory: the process-wide `ZUNE_CACHE_DIR` belongs to the
        // cache module's own tests and mutating it here would race with them.
        let dir = std::env::temp_dir().join(format!("zune-coverfetch-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!recently_missed_in(&dir, "Radiohead", "Kid A"));
        note_miss_in(&dir, "Radiohead", "Kid A");
        assert!(recently_missed_in(&dir, "Radiohead", "Kid A"));
        // A different album is unaffected.
        assert!(!recently_missed_in(&dir, "Radiohead", "Amnesiac"));
        assert_eq!(clear_misses_in(&dir), 1);
        assert!(!recently_missed_in(&dir, "Radiohead", "Kid A"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
