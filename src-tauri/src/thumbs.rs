//! Thumbnails for the Icons view, on both sides of the window.
//!
//! Images are decoded and re-encoded to a small JPEG; videos get a poster frame
//! from the system `ffmpeg`. Everything lands in one disk cache with a size cap.
//!
//! The contract with the frontend is three-valued: `Ok(Some(uri))` is a
//! preview, `Ok(None)` means there is legitimately none (no ffmpeg, a file too
//! large, an unsupported format) and the tile keeps its type icon, and `Err` is
//! a real failure, which the tile also survives.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder, ImageReader};
use md5::{Digest, Md5};
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::local_path::resolve_local;
use crate::transfers::s3;

/// Long edge of a thumbnail in pixels: the largest tile at roughly two device
/// pixels. Smaller tile sizes scale this down in CSS.
const THUMB_EDGE: u32 = 256;

/// JPEG quality: visually lossless at this size, and a data URI of 10–20 KB.
const JPEG_Q: u8 = 80;

/// Images larger than this get the type icon instead of a preview.
///
/// Not a technical limit: the source is downloaded and decoded whole either
/// way, and this is the ceiling on what that is worth.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// How much of a remote video is fetched for ffmpeg on the first try.
///
/// A faststart file keeps its `moov` atom and first keyframe at the beginning,
/// so this is enough. A smaller file is fetched whole.
const VIDEO_HEAD_BYTES: u64 = 24 * 1024 * 1024;

/// When the head is not enough, a video up to this size is fetched in full and
/// tried once more; only the missing rest is requested.
///
/// Parsing the container to locate `moov` would be more precise and far more
/// fragile.
const FULL_VIDEO_FALLBACK_BYTES: u64 = 200 * 1024 * 1024;

/// Cache ceiling; going over it evicts down to about 85% of this.
const CACHE_CAP_BYTES: u64 = 256 * 1024 * 1024;

/// How long a single frame extraction may take.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(15);

/// Serializes eviction so two sweeps cannot delete each other's files.
static EVICT_LOCK: Mutex<()> = Mutex::new(());

/// Makes temporary file names unique within this process.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);


// The same extension set the listing uses for its video glyph.
fn is_video(name: &str) -> bool {
    matches!(
        name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref(),
        Some("mp4" | "webm" | "mov" | "mkv" | "avi" | "m4v")
    )
}

/// Produces the thumbnail for one tile.
///
/// `side` is `"local"` or `"remote"`; `path` is a local path or an S3 key
/// relative to the volume root.
#[tauri::command]
pub async fn get_thumbnail(
    app: AppHandle,
    side: String,
    path: String,
) -> Result<Option<String>, String> {
    let cache_dir = thumbs_dir(&app)?;
    let video = is_video(&path);
    match (side.as_str(), video) {
        ("local", false) => {
            let file = resolve_local(&path)?;
            // Reading, decoding, resizing and encoding is synchronous CPU work.
            tauri::async_runtime::spawn_blocking(move || local_thumbnail(&file, &cache_dir))
                .await
                .map_err(|e| format!("thumbnail task failed: {e}"))?
        }
        ("local", true) => {
            let file = resolve_local(&path)?;
            local_video_thumbnail(&file, &cache_dir).await
        }
        ("remote", false) => remote_thumbnail(&app, &path, &cache_dir).await,
        ("remote", true) => remote_video_thumbnail(&app, &path, &cache_dir).await,
        _ => Ok(None),
    }
}

fn thumbs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir unavailable: {e}"))?
        .join("thumbs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

// ── Images ─────────────────────────────────────────────────────────────

/// Local image, from the cache when possible.
///
/// The cache key carries the modification time, so an edited file produces a
/// new thumbnail without anything having to invalidate the old one.
fn local_thumbnail(file: &Path, cache_dir: &Path) -> Result<Option<String>, String> {
    let Ok(meta) = std::fs::metadata(file) else {
        return Ok(None);
    };
    if !meta.is_file() || meta.len() > MAX_SOURCE_BYTES {
        return Ok(None);
    }
    let key = format!("local\0{}\0{}\0{THUMB_EDGE}", file.display(), mtime_ms(&meta));
    let cache_path = cache_dir.join(cache_name(&key));

    if let Ok(bytes) = std::fs::read(&cache_path) {
        touch(&cache_path);
        return Ok(Some(to_data_uri(&bytes)));
    }

    let src = std::fs::read(file).map_err(|e| e.to_string())?;
    let Some(jpeg) = encode_thumb(&src) else {
        return Ok(None);
    };
    write_atomic(&cache_path, &jpeg).map_err(|e| e.to_string())?;
    enforce_cap(cache_dir);
    Ok(Some(to_data_uri(&jpeg)))
}

/// Remote image: `HeadObject` first, so an object over the limit costs one cheap
/// request instead of a download. Its etag is what keys the cache.
async fn remote_thumbnail(
    app: &AppHandle,
    key_path: &str,
    cache_dir: &Path,
) -> Result<Option<String>, String> {
    let key = remote_key(key_path);
    if key.is_empty() {
        return Ok(None);
    }
    let (client, bucket) = s3(app)?;

    let head = client
        .head_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let size = head.content_length().unwrap_or(0).max(0) as u64;
    if size == 0 || size > MAX_SOURCE_BYTES {
        return Ok(None);
    }
    let etag = head.e_tag().unwrap_or_default().trim_matches('"').to_string();

    let cache_key = format!("remote\0{bucket}\0{key}\0{etag}\0{THUMB_EDGE}");
    let cache_path = cache_dir.join(cache_name(&cache_key));
    if let Ok(bytes) = std::fs::read(&cache_path) {
        touch(&cache_path);
        return Ok(Some(to_data_uri(&bytes)));
    }

    let obj = client
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let src = obj
        .body
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .into_bytes();

    finalize_thumb(src.to_vec(), cache_path, cache_dir).await
}

// ── Video, through the system ffmpeg ───────────────────────────────────────────

/// Local video: a frame from around the first second.
async fn local_video_thumbnail(
    file: &Path,
    cache_dir: &Path,
) -> Result<Option<String>, String> {
    let Ok(meta) = std::fs::metadata(file) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let key = format!(
        "localvid\0{}\0{}\0{THUMB_EDGE}",
        file.display(),
        mtime_ms(&meta)
    );
    let cache_path = cache_dir.join(cache_name(&key));

    // The cache is consulted before ffmpeg is looked for, so an already made
    // poster still shows on a machine where ffmpeg has since been removed.
    if let Ok(bytes) = std::fs::read(&cache_path) {
        touch(&cache_path);
        return Ok(Some(to_data_uri(&bytes)));
    }
    if !ffmpeg_available().await {
        return Ok(None);
    }

    let Some(png) = ffmpeg_frame(&file.to_string_lossy()).await else {
        return Ok(None);
    };
    finalize_thumb(png, cache_path, cache_dir).await
}

/// Remote video: the head of the file is fetched with a ranged GET and handed to
/// ffmpeg; if that yields no frame, the whole file is tried once.
async fn remote_video_thumbnail(
    app: &AppHandle,
    key_path: &str,
    cache_dir: &Path,
) -> Result<Option<String>, String> {
    let key = remote_key(key_path);
    if key.is_empty() {
        return Ok(None);
    }
    let (client, bucket) = s3(app)?;

    let head = client
        .head_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let size = head.content_length().unwrap_or(0).max(0) as u64;
    let etag = head.e_tag().unwrap_or_default().trim_matches('"').to_string();

    let cache_key = format!("remotevid\0{bucket}\0{key}\0{etag}\0{THUMB_EDGE}");
    let cache_path = cache_dir.join(cache_name(&cache_key));
    if let Ok(bytes) = std::fs::read(&cache_path) {
        touch(&cache_path);
        return Ok(Some(to_data_uri(&bytes)));
    }
    if size == 0 || !ffmpeg_available().await {
        return Ok(None);
    }

    let want = VIDEO_HEAD_BYTES.min(size);
    let obj = client
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .range(format!("bytes=0-{}", want - 1))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let head_bytes = obj
        .body
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .into_bytes();

    let ext = Path::new(&key)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp4");
    if let Some(png) = frame_from_head(&head_bytes, ext, cache_dir).await {
        return finalize_thumb(png, cache_path, cache_dir).await;
    }

    // No frame from the head, so the index is probably at the end of the file.
    // Only the missing part is requested; ffmpeg reads a complete file wherever
    // the index sits.
    if size > want && size <= FULL_VIDEO_FALLBACK_BYTES {
        let rest = client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .range(format!("bytes={want}-{}", size - 1))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let rest_bytes = rest.body.collect().await.map_err(|e| e.to_string())?.into_bytes();
        let mut full = Vec::with_capacity(size as usize);
        full.extend_from_slice(&head_bytes);
        full.extend_from_slice(&rest_bytes);
        if let Some(png) = frame_from_head(&full, ext, cache_dir).await {
            return finalize_thumb(png, cache_path, cache_dir).await;
        }
    }
    Ok(None)
}

/// Writes the fetched bytes to a temporary file, since ffmpeg wants a path, and
/// removes it whatever happens.
async fn frame_from_head(bytes: &[u8], ext: &str, cache_dir: &Path) -> Option<Vec<u8>> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = cache_dir.join(format!(".vh-{}-{seq}.{ext}", std::process::id()));
    std::fs::write(&tmp, bytes).ok()?;
    let png = ffmpeg_frame(&tmp.to_string_lossy()).await;
    let _ = std::fs::remove_file(&tmp);
    png
}

/// Whether the system has ffmpeg; asked once and remembered.
async fn ffmpeg_available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    if let Some(v) = OK.get() {
        return *v;
    }
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = OK.set(ok);
    ok
}

/// Extracts one frame as PNG, trying one second in and then the very start.
///
/// A clip shorter than a second has nothing at the first seek point, and no
/// frame at all is an expected outcome, not an error.
async fn ffmpeg_frame(input: &str) -> Option<Vec<u8>> {
    let scale = format!("scale={THUMB_EDGE}:-1:flags=bilinear");
    for seek in ["1", "0"] {
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-v", "error", "-nostdin", "-ss", seek, "-i", input, "-frames:v", "1",
            "-vf", &scale, "-f", "image2pipe", "-vcodec", "png", "-",
        ]);
        cmd.stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let out = match tokio::time::timeout(FFMPEG_TIMEOUT, cmd.output()).await {
            Ok(Ok(o)) => o,
            _ => return None,
        };
        if out.status.success() && !out.stdout.is_empty() {
            return Some(out.stdout);
        }
    }
    None
}

// ── Shared ─────────────────────────────────────────────────────────────

fn remote_key(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

fn mtime_ms(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Shared tail for the paths that already hold the source bytes: encode, cache,
/// enforce the cap, return the data URI.
async fn finalize_thumb(
    src: Vec<u8>,
    cache_path: PathBuf,
    cache_dir: &Path,
) -> Result<Option<String>, String> {
    let cache_dir = cache_dir.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(jpeg) = encode_thumb(&src) else {
            return Ok::<Option<String>, String>(None);
        };
        write_atomic(&cache_path, &jpeg).map_err(|e| e.to_string())?;
        enforce_cap(&cache_dir);
        Ok(Some(to_data_uri(&jpeg)))
    })
    .await
    .map_err(|e| format!("thumbnail task failed: {e}"))?
}

fn encode_thumb(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    // `thumbnail` keeps the aspect ratio; JPEG has no alpha, so transparency is
    // flattened away against the tile's own dark background.
    let rgb = img.thumbnail(THUMB_EDGE, THUMB_EDGE).to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());

    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, JPEG_Q)
        .write_image(rgb.as_raw(), w, h, ExtendedColorType::Rgb8)
        .ok()?;
    Some(out.into_inner())
}

/// Turns a cache key into a short, filesystem-safe file name.
fn cache_name(key: &str) -> String {
    let mut h = Md5::new();
    h.update(key.as_bytes());
    format!("{}.jpg", URL_SAFE_NO_PAD.encode(h.finalize()))
}

fn to_data_uri(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg))
}

/// Marks a cache hit as recently used by moving its modification time.
///
/// Access time would be the natural field and is unreliable: most mounts are
/// `relatime` or `noatime`.
fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Writes through a temporary name in the same directory and renames it into
/// place, so a half-written file is never read as a thumbnail.
fn write_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dest.with_extension(format!("tmp{}-{seq}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)
}

/// Deletes least recently used thumbnails until the cache is back under the cap.
fn enforce_cap(dir: &Path) {
    let _guard = EVICT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in rd.flatten() {
        let Ok(m) = entry.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        let p = entry.path();
        // Only finished thumbnails are swept; temporary files belong to whoever is writing them.
        if p.extension().and_then(|x| x.to_str()) != Some("jpg") {
            continue;
        }
        total += m.len();
        files.push((p, m.len(), m.modified().unwrap_or(UNIX_EPOCH)));
    }
    if total <= CACHE_CAP_BYTES {
        return;
    }

    files.sort_by_key(|(_, _, mtime)| *mtime);
    let target = CACHE_CAP_BYTES / 100 * 85;
    for (p, sz, _) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total = total.saturating_sub(sz);
        }
    }
}
