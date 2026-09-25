//! Transfer engine: one queue, one job at a time, in both directions and on both sides.
//!
//! An interrupted download or upload leaves its `.part` and sidecar files behind and continues
//! from there the next time the same job is queued.
//!
//! Progress reaches the window as events: `transfer-progress`, `transfer-file`, `transfer-retry`,
//! `transfer-done`, `transfer-error`, `transfer-canceled`.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_sdk_s3::Client;
use aws_sdk_s3::config::interceptors::{
    BeforeTransmitInterceptorContextRef, FinalizerInterceptorContextRef,
};
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::{ConfigBag, Intercept, RuntimeComponents};
use aws_sdk_s3::error::{BoxError, DisplayErrorContext};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinSet;

use crate::config::{active_client, active_connection_id, client_by_id};
use crate::core::{put_object_verified, s3_err, verify_object_size};
use crate::fs_ops::ensure_not_root;
use crate::local_path::resolve_local;

const CHUNK: u64 = 8 * 1024 * 1024;
const EMIT_EVERY: Duration = Duration::from_millis(250);
/// How many times one ranged GET is tried before the transfer fails.
const GET_ATTEMPTS: u32 = 4;
/// Wait before retrying a failed range; grows with each attempt.
const GET_BACKOFF: Duration = Duration::from_millis(500);
/// How many times one upload part is tried. The SDK does the retrying; the
/// count matches a download's ranges, so the queue row reads the same.
const PART_ATTEMPTS: u32 = 4;
/// How many ranges of one file are fetched at the same time.
const DOWNLOAD_STREAMS: usize = 4;
/// Range size of one download chunk.
const DOWNLOAD_CHUNK: u64 = 8 * 1024 * 1024;
/// How often a running download checks for cancellation and refreshes progress.
const DOWNLOAD_TICK: Duration = Duration::from_millis(200);

/// What a download reports when its object was replaced while it ran.
const ERR_CHANGED: &str =
    "changed on the volume while it was downloading; start the download again";

/// Reads bytes `off..=end` of an object, retrying a failed attempt after a
/// growing pause.
///
/// A body of any other length than the range counts as a failed attempt: a
/// short one would leave a hole of zeros in the file while its size still
/// looks right.
///
/// With `etag`, the object must still be that version. If it was replaced the
/// read stops at once with `ERR_CHANGED`: another attempt would only fetch the
/// new version into a file already holding parts of the old one.
#[allow(clippy::too_many_arguments)]
async fn get_range_retry(
    app: &AppHandle,
    id: &str,
    client: &Client,
    bucket: &str,
    key: &str,
    off: u64,
    end: u64,
    etag: Option<&str>,
    cancel: &AtomicBool,
) -> Result<impl std::ops::Deref<Target = [u8]> + Send, String> {
    let range = format!("bytes={off}-{end}");
    let want = end - off + 1;
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let outcome = match client
            .get_object()
            .bucket(bucket)
            .key(key)
            .range(&range)
            .set_if_match(etag.map(str::to_string))
            .send()
            .await
        {
            Ok(obj) => match obj.body.collect().await {
                Ok(data) => {
                    let data = data.into_bytes();
                    if data.len() as u64 == want {
                        Ok(data)
                    } else {
                        Err(format!("{range} came back with {} bytes instead of {want}", data.len()))
                    }
                }
                Err(e) => Err(e.to_string()),
            },
            Err(e) if e.raw_response().is_some_and(|r| r.status().as_u16() == 412) => {
                return Err(ERR_CHANGED.into());
            }
            Err(e) => Err(s3_err(&e)),
        };
        match outcome {
            Ok(data) => {
                if attempt > 1 {
                    emit_retry(app, id, 0, GET_ATTEMPTS);
                }
                return Ok(data);
            }
            Err(detail) => {
                if attempt >= GET_ATTEMPTS || cancel.load(Ordering::Relaxed) {
                    return Err(detail);
                }
                crate::devlog::verbose(
                    "transfer",
                    format!("range {range} of {key:?} failed ({detail}), retry {attempt}"),
                );
                emit_retry(app, id, attempt, GET_ATTEMPTS);
                tokio::time::sleep(GET_BACKOFF * attempt).await;
            }
        }
    }
}

/// One queued transfer with everything the worker needs to run it alone.
struct Job {
    id: String,
    kind: String,
    /// Source path, relative to the root of its own side.
    src: String,
    /// Destination directory, relative to the root of its own side.
    dest_dir: String,
    /// Name at the destination; for a zip job the archive name without `.zip`.
    name: String,
    /// Folder job: the whole tree is transferred.
    is_dir: bool,
    /// Move: the source is deleted once the transfer succeeds.
    move_src: bool,
    /// What to do when the name is taken: `skip`, `overwrite` or `rename`.
    conflict: String,
    /// Zip jobs only: the remote sources that go into the archive.
    zip_srcs: Vec<ZipSrc>,
    /// Connection this job was queued with; `None` for local-only jobs.
    conn_id: Option<String>,
    cancel: Arc<AtomicBool>,
    /// Local source files that really reached the destination; a Move deletes only these.
    sent_local: Mutex<Vec<PathBuf>>,
    /// Remote source keys that really reached the destination, `.keep` markers included.
    sent_remote: Mutex<Vec<String>>,
}

/// One remote source inside a zip job: a file, or a folder taken as a prefix.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ZipSrc {
    /// Remote path, relative to the pane root.
    path: String,
    is_dir: bool,
}

#[derive(Clone)]
pub struct TransferManager {
    queue: Arc<Mutex<VecDeque<Job>>>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    running: Arc<AtomicBool>,
    counter: Arc<AtomicU64>,
}

impl TransferManager {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_id(&self) -> String {
        format!("t{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvt {
    id: String,
    bytes_done: u64,
    bytes_total: u64,
    speed: u64,
}

#[derive(Serialize, Clone)]
struct IdEvt {
    id: String,
}

/// Which file of a folder job is being sent right now; single-file jobs never emit it.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileEvt {
    id: String,
    name: String,
}

/// A chunk is being tried again; `attempt` 0 means the chunk finally arrived.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RetryEvt {
    id: String,
    attempt: u32,
    of: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ErrEvt {
    id: String,
    detail: String,
}

/// Sidecar next to a `.part` download; tells a later run what is already on disk.
#[derive(Serialize, Deserialize)]
struct Resume {
    key: String,
    total: u64,
    etag: String,
    chunk: u64,
    /// Chunks finished from the start of the file; a restart begins here.
    #[serde(default)]
    done_chunks: u64,
}

enum Outcome {
    Done,
    Canceled,
    /// Nothing was written; the reason is shown to the user. A Move keeps its source.
    Skipped(String),
}

/// A single-item job met a name at the destination that the interface did not see.
const SKIP_TAKEN: &str = "not transferred: the name is already taken at the destination";
const SKIP_SAME: &str = "not transferred: source and destination are the same";

/// Queues one transfer and returns its id, which every later event carries.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // parameter names are the IPC field names
pub fn transfer_start(
    app: AppHandle,
    manager: tauri::State<'_, TransferManager>,
    kind: String,
    src: String,
    dest_dir: String,
    name: String,
    is_dir: bool,
    move_src: bool,
    conflict: String,
) -> Result<String, String> {
    // A move deletes its source afterwards.
    if move_src {
        let src_remote = matches!(kind.as_str(), "download" | "remoteCopy" | "remoteMove");
        ensure_not_root(src_remote, &src)?;
    }
    let mgr = (*manager).clone();
    let id = mgr.next_id();
    let cancel = Arc::new(AtomicBool::new(false));
    mgr.cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    // The connection is taken here, not when the worker picks the job up.
    let conn_id = active_connection_id(app.clone());
    mgr.queue.lock().unwrap().push_back(Job {
        id: id.clone(),
        kind,
        src,
        dest_dir,
        name,
        is_dir,
        move_src,
        conflict,
        zip_srcs: Vec::new(),
        conn_id,
        cancel,
        sent_local: Mutex::new(Vec::new()),
        sent_remote: Mutex::new(Vec::new()),
    });
    ensure_worker(app, mgr);
    Ok(id)
}

/// Queues a zip download: the chosen remote sources are streamed into one archive.
#[tauri::command]
pub fn transfer_zip_start(
    app: AppHandle,
    manager: tauri::State<'_, TransferManager>,
    srcs: Vec<ZipSrc>,
    dest_dir: String,
    name: String,
) -> Result<String, String> {
    if srcs.is_empty() {
        return Err("no sources for zip".into());
    }
    let mgr = (*manager).clone();
    let id = mgr.next_id();
    let cancel = Arc::new(AtomicBool::new(false));
    mgr.cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    let conn_id = active_connection_id(app.clone());
    mgr.queue.lock().unwrap().push_back(Job {
        id: id.clone(),
        kind: "downloadZip".into(),
        src: String::new(),
        dest_dir,
        name,
        is_dir: false,
        move_src: false,
        conflict: "skip".into(),
        zip_srcs: srcs,
        conn_id,
        cancel,
        sent_local: Mutex::new(Vec::new()),
        sent_remote: Mutex::new(Vec::new()),
    });
    ensure_worker(app, mgr);
    Ok(id)
}

/// Queues the download that opens a remote file in the OS default application.
#[tauri::command]
pub fn open_remote_start(
    app: AppHandle,
    manager: tauri::State<'_, TransferManager>,
    path: String,
    name: String,
) -> Result<String, String> {
    let mgr = (*manager).clone();
    let id = mgr.next_id();
    let cancel = Arc::new(AtomicBool::new(false));
    mgr.cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    let conn_id = active_connection_id(app.clone());
    mgr.queue.lock().unwrap().push_back(Job {
        id: id.clone(),
        kind: "openDownload".into(),
        src: path,
        dest_dir: String::new(), // the worker resolves the cache directory itself
        name,
        is_dir: false,
        move_src: false,
        conflict: "overwrite".into(),
        zip_srcs: Vec::new(),
        conn_id,
        cancel,
        sent_local: Mutex::new(Vec::new()),
        sent_remote: Mutex::new(Vec::new()),
    });
    ensure_worker(app, mgr);
    Ok(id)
}

/// Cancels one transfer, whether it is running or still waiting in the queue.
#[tauri::command]
pub fn transfer_cancel(
    app: AppHandle,
    manager: tauri::State<'_, TransferManager>,
    id: String,
) {
    if let Some(c) = manager.cancels.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
    let removed = {
        let mut q = manager.queue.lock().unwrap();
        q.iter()
            .position(|j| j.id == id)
            .map(|pos| q.remove(pos))
            .is_some()
    };
    if removed {
        manager.cancels.lock().unwrap().remove(&id);
        let _ = app.emit("transfer-canceled", IdEvt { id });
    }
}

/// Cancels everything: the running job and every job still waiting.
#[tauri::command]
pub fn transfer_cancel_all(app: AppHandle, manager: tauri::State<'_, TransferManager>) {
    for c in manager.cancels.lock().unwrap().values() {
        c.store(true, Ordering::Relaxed);
    }
    let drained: Vec<String> = {
        let mut q = manager.queue.lock().unwrap();
        q.drain(..).map(|j| j.id.clone()).collect()
    };
    let mut cancels = manager.cancels.lock().unwrap();
    for id in drained {
        cancels.remove(&id);
        let _ = app.emit("transfer-canceled", IdEvt { id });
    }
}

/// Starts the single worker if it is not already running.
fn ensure_worker(app: AppHandle, mgr: TransferManager) {
    if mgr.running.swap(true, Ordering::SeqCst) {
        return; // a worker is already draining the queue
    }
    tauri::async_runtime::spawn(async move {
        // However this task ends, the flag is cleared so the next job can start a new worker.
        struct Guard(std::sync::Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = Guard(mgr.running.clone());
        loop {
            let job = mgr.queue.lock().unwrap().pop_front();
            let Some(job) = job else {
                mgr.running.store(false, Ordering::SeqCst);
                // A job may have arrived between the pop and the flag; take the queue back.
                if mgr.queue.lock().unwrap().is_empty() {
                    break;
                }
                if mgr.running.swap(true, Ordering::SeqCst) {
                    break;
                }
                continue;
            };

            let id = job.id.clone();
            if job.cancel.load(Ordering::Relaxed) {
                let _ = app.emit("transfer-canceled", IdEvt { id: id.clone() });
                mgr.cancels.lock().unwrap().remove(&id);
                continue;
            }

            crate::devlog::verbose(
                "transfer",
                format!(
                    "start {id} {} {:?} src={:?} dest={:?} dir={} move={} conflict={}",
                    job.kind, job.name, job.src, job.dest_dir, job.is_dir, job.move_src, job.conflict
                ),
            );
            let started = std::time::Instant::now();
            let result = run_job(&app, &job).await;
            mgr.cancels.lock().unwrap().remove(&id);
            let ms = started.elapsed().as_millis();
            let outcome = match &result {
                Ok(Outcome::Done) => format!("done {id} {ms}ms"),
                Ok(Outcome::Canceled) => format!("canceled {id} {ms}ms"),
                Ok(Outcome::Skipped(why)) => format!("skipped {id} {ms}ms {why}"),
                Err(detail) => format!("error {id} {ms}ms {detail}"),
            };
            crate::devlog::verbose("transfer", outcome);
            match result {
                Ok(Outcome::Done) => {
                    // A move deletes its source only now, after the copy succeeded.
                    let is_move = job.move_src
                        || matches!(job.kind.as_str(), "localMove" | "remoteMove");
                    if is_move && let Err(e) = delete_source(&app, &job).await {
                        crate::devlog::verbose("transfer", format!("move source not removed {id}: {e}"));
                        let _ = app.emit(
                            "transfer-error",
                            ErrEvt {
                                id,
                                detail: format!("copied, but source not removed: {e}"),
                            },
                        );
                        continue;
                    }
                    let _ = app.emit("transfer-done", IdEvt { id });
                }
                Ok(Outcome::Canceled) => {
                    let _ = app.emit("transfer-canceled", IdEvt { id });
                }
                // Reported like an error, so the user learns the item did not arrive.
                Ok(Outcome::Skipped(detail)) | Err(detail) => {
                    let _ = app.emit("transfer-error", ErrEvt { id, detail });
                }
            }
        }
    });
}

async fn run_job(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    match job.kind.as_str() {
        "download" => run_download(app, job).await,
        "upload" => run_upload(app, job).await,
        "downloadZip" => run_download_zip(app, job).await,
        "openDownload" => run_open_download(app, job).await,
        "localCopy" | "localMove" => run_local_copy(app, job).await,
        "remoteCopy" | "remoteMove" => run_remote_copy(app, job).await,
        other => Err(format!("unsupported transfer kind: {other}")),
    }
}

/// Cache directory holding the copies downloaded for "open in the OS app".
fn opened_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("cache dir unavailable: {e}"))?
        .join("opened");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Empties that cache at startup; called from `lib.rs`.
pub fn clear_opened_cache(app: &AppHandle) {
    if let Ok(dir) = app.path().app_cache_dir().map(|d| d.join("opened")) {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Cache subdirectory for one object version; the etag is part of it.
fn opened_hash(bucket: &str, key: &str, etag: &str) -> String {
    let mut h = Md5::new();
    h.update(format!("{bucket}\0{key}\0{etag}").as_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize())
}

/// Marks the downloaded copy read-only so an editor shows it cannot be saved back.
fn make_read_only(path: &std::path::Path) {
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = fs::set_permissions(path, perms);
    }
}

async fn run_open_download(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;
    let key = remote_key(&job.src);

    // HEAD gives size and etag, and unlike listing it is immediately consistent.
    let head = client
        .head_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| s3_err(&e))?;
    let size = head.content_length().unwrap_or(0).max(0) as u64;
    let etag = head.e_tag().unwrap_or_default().trim_matches('"').to_string();

    let cache_path = opened_dir(app)?
        .join(opened_hash(&bucket, &key, &etag))
        .join(&job.name);

    // Already in the cache with the right size: open it without downloading.
    if let Ok(meta) = fs::metadata(&cache_path)
        && meta.is_file()
        && meta.len() == size
    {
        emit_progress(app, &job.id, size, size, 0);
        make_read_only(&cache_path);
        opener::open(&cache_path).map_err(|e| e.to_string())?;
        return Ok(Outcome::Done);
    }

    // Otherwise download it like any other file, with progress and cancel.
    match download_one(
        app, &stream_clients(&client), &bucket, &key, &cache_path, &job.id, &job.cancel,
        None, "overwrite",
    )
    .await?
    {
        Outcome::Canceled => Ok(Outcome::Canceled),
        Outcome::Skipped(why) => Ok(Outcome::Skipped(why)),
        Outcome::Done => {
            make_read_only(&cache_path);
            opener::open(&cache_path).map_err(|e| e.to_string())?;
            Ok(Outcome::Done)
        }
    }
}

/// Deletes the source of a finished Move.
async fn delete_source(app: &AppHandle, job: &Job) -> Result<(), String> {
    match job.kind.as_str() {
        "download" | "remoteCopy" | "remoteMove" => {
            let (client, bucket) = s3_job(app, &job.conn_id)?;
            if job.is_dir {
                let prefix = as_prefix(&job.src);
                // Only what was really transferred is deleted; the source is never re-listed.
                let keys = job.sent_remote.lock().unwrap().clone();
                for k in &keys {
                    let _ = client.delete_object().bucket(&bucket).key(k).send().await;
                }
                // Subfolders go from the deepest outwards, because the gateway only removes an empty one.
                for dir in nested_dir_prefixes(&prefix, &keys) {
                    let _ = client.delete_object().bucket(&bucket).key(&dir).send().await;
                }
                let _ = client
                    .delete_object()
                    .bucket(&bucket)
                    .key(&prefix)
                    .send()
                    .await;
            } else {
                client
                    .delete_object()
                    .bucket(&bucket)
                    .key(remote_key(&job.src))
                    .send()
                    .await
                    .map_err(|e| s3_err(&e))?;
            }
        }
        "upload" | "localCopy" | "localMove" => {
            let p = resolve_local(&job.src)?;
            if job.is_dir {
                // Same rule locally: delete the sent files, not the whole tree.
                let sent = job.sent_local.lock().unwrap().clone();
                return remove_sent_files(&p, &sent);
            }
            match fs::remove_file(&p) {
                Ok(()) => {
                    let _ = fs::remove_file(sidecar_of(&p));
                }
                // A local move may already have renamed the file away; nothing left to delete.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        _ => {}
    }
    Ok(())
}

const ERR_INTO_ITSELF: &str = "can't copy or move a folder into itself";

/// Is `dest` the same folder as `src`, or inside it? Symlinks are resolved before comparing.
fn local_is_within(src: &std::path::Path, dest: &std::path::Path) -> bool {
    let src_c = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = dest.to_path_buf();
    let dest_c = loop {
        if let Ok(c) = fs::canonicalize(&cur) {
            break tail.iter().rev().fold(c, |acc, seg| acc.join(seg));
        }
        match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                cur = parent.to_path_buf();
            }
            _ => break dest.to_path_buf(),
        }
    };
    dest_c.starts_with(&src_c)
}

/// Delete step of a local folder Move: removes the files that were really sent, then the
/// directories that became empty. Anything left behind keeps its folder.
fn remove_sent_files(root: &std::path::Path, sent: &[PathBuf]) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let mut failed = 0usize;
    for f in sent {
        match fs::remove_file(f) {
            Ok(()) => {
                let _ = fs::remove_file(sidecar_of(f));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => failed += 1,
        }
    }
    // Then the directories, deepest first, without following symlinks.
    let mut dirs = vec![root.to_path_buf()];
    let mut i = 0;
    while i < dirs.len() {
        if let Ok(rd) = fs::read_dir(&dirs[i]) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    dirs.push(e.path());
                }
            }
        }
        i += 1;
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = fs::remove_dir(&d);
    }
    if failed > 0 {
        return Err(format!("{failed} file(s) could not be removed"));
    }
    Ok(())
}

/// Intermediate folder prefixes of the given keys, deepest first, excluding `root` itself.
fn nested_dir_prefixes(root: &str, keys: &[String]) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for k in keys {
        let Some(rel) = k.strip_prefix(root) else { continue };
        let mut end = 0;
        while let Some(i) = rel[end..].find('/') {
            end += i + 1;
            dirs.insert(format!("{root}{}", &rel[..end]));
        }
    }
    let mut out: Vec<String> = dirs.into_iter().collect();
    out.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    out
}

/// Remote path as a prefix: `""` stays empty, `"a/b"` becomes `"a/b/"`.
fn as_prefix(path: &str) -> String {
    match path.trim_matches('/') {
        "" => String::new(),
        p => format!("{p}/"),
    }
}

/// Splits a name into the part before any existing " copy N" suffix, and its extension.
fn split_for_copy(name: &str) -> (String, String) {
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let trimmed = base.trim_end();
    let stem = if let Some(pre) = trimmed.strip_suffix(" copy") {
        pre
    } else if let Some(idx) = trimmed.rfind(" copy ") {
        let tail = &trimmed[idx + 6..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            &trimmed[..idx]
        } else {
            base
        }
    } else {
        base
    };
    (stem.to_string(), ext.to_string())
}

fn nth_copy(stem: &str, ext: &str, n: usize) -> String {
    if n == 1 {
        format!("{stem} copy{ext}")
    } else {
        format!("{stem} copy {n}{ext}")
    }
}

/// First free " copy N" name for a local directory.
fn free_local_name(dir: &std::path::Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = split_for_copy(name);
    (1usize..)
        .map(|n| nth_copy(&stem, &ext, n))
        .find(|c| !dir.join(c).exists())
        .unwrap_or_else(|| name.to_string())
}

async fn head_ok(client: &Client, bucket: &str, key: &str) -> bool {
    client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .is_ok()
}

/// First free " copy N" name under a remote prefix, checked with HEAD.
async fn free_remote_name(
    client: &Client,
    bucket: &str,
    dest_prefix: &str,
    name: &str,
) -> String {
    if !head_ok(client, bucket, &format!("{dest_prefix}{name}")).await {
        return name.to_string();
    }
    let (stem, ext) = split_for_copy(name);
    for n in 1usize..10_000 {
        let cand = nth_copy(&stem, &ext, n);
        if !head_ok(client, bucket, &format!("{dest_prefix}{cand}")).await {
            return cand;
        }
    }
    name.to_string()
}

/// S3 client and bucket name of the active connection; `thumbs.rs` uses it too.
pub(crate) fn s3(app: &AppHandle) -> Result<(Client, String), String> {
    active_client(app).map_err(|e| e.to_string())
}

/// Client of the connection this job was queued with, falling back to the active one.
fn s3_job(app: &AppHandle, conn_id: &Option<String>) -> Result<(Client, String), String> {
    match conn_id {
        Some(id) => client_by_id(app, id).map_err(|e| e.to_string()),
        None => s3(app),
    }
}

fn remote_key(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

/// True when a path taken from a remote key stays inside the folder it is
/// joined onto: every part is an ordinary name, so no `..`, no leading `/`, no
/// drive letter.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Joins a remote-relative path onto a local root, refusing one that would land
/// outside it.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if !safe_rel(rel) {
        return Err(format!("unsafe path in object name: {rel}"));
    }
    Ok(root.join(rel))
}

/// Progress context of a folder job: bytes already done, the folder total, and its clock.
type Agg = Option<(u64, u64, Instant)>;

async fn run_download(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;
    let dest_root = resolve_local(&job.dest_dir)?;
    // One set of stream clients for the whole job: a folder reuses their
    // connections from file to file.
    let clients = stream_clients(&client);

    if !job.is_dir {
        let out = safe_join(&dest_root, &job.name)?;
        return download_one(
            app, &clients, &bucket, &remote_key(&job.src), &out, &job.id,
            &job.cancel, None, &job.conflict,
        )
        .await;
    }

    // A folder job downloads every object under the prefix and rebuilds the tree locally.
    let prefix = as_prefix(&job.src);
    let folder_root = dest_root.join(&job.name);
    fs::create_dir_all(&folder_root).map_err(|e| e.to_string())?; // an empty folder still arrives
    let objects = list_objects_all(&client, &bucket, &prefix).await?;
    let total: u64 = objects
        .iter()
        .filter(|(k, _, _)| !k.ends_with("/.keep"))
        .map(|(_, sz, _)| *sz)
        .sum();
    let clock = Instant::now();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total, 0);

    for (key, sz, _) in &objects {
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        let rel = key.strip_prefix(&prefix).unwrap_or(key);
        if rel.is_empty() {
            continue;
        }
        // A `.keep` marker stands for an empty remote folder and becomes a real directory here.
        if key.ends_with("/.keep") {
            if let Some((dir, _)) = rel.rsplit_once('/') {
                fs::create_dir_all(safe_join(&folder_root, dir)?).map_err(|e| e.to_string())?;
            }
            job.sent_remote.lock().unwrap().push(key.clone());
            continue;
        }
        let out = safe_join(&folder_root, rel)?;
        // A skipped file was not transferred, so a Move must leave it in place.
        if job.conflict == "skip" && out.exists() {
            done += *sz;
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match download_one(
            app, &clients, &bucket, key, &out, &job.id, &job.cancel,
            Some((done, total, clock)), &job.conflict,
        )
        .await?
        {
            Outcome::Canceled => return Ok(Outcome::Canceled),
            Outcome::Done => job.sent_remote.lock().unwrap().push(key.clone()),
            Outcome::Skipped(_) => {}
        }
        done += *sz;
    }
    emit_progress(app, &job.id, total, total, 0);
    Ok(Outcome::Done)
}

/// One client per download stream: the given one, plus copies of it that each
/// open their own connection.
///
/// Streams that share a connection share its flow-control window, and one
/// connection alone stays well below the line's speed.
fn stream_clients(client: &Client) -> Vec<Client> {
    let mut clients = vec![client.clone()];
    clients.extend((1..DOWNLOAD_STREAMS).map(|_| Client::from_conf(client.config().clone())));
    clients
}

/// Downloads one object into `<out>.part` in parallel chunks and renames it when complete.
///
/// `clients` are the stream clients from `stream_clients`; the first one also
/// asks for the object's size.
#[allow(clippy::too_many_arguments)]
async fn download_one(
    app: &AppHandle,
    clients: &[Client],
    bucket: &str,
    key: &str,
    out_path: &std::path::Path,
    id: &str,
    cancel: &Arc<AtomicBool>,
    agg: Agg,
    conflict: &str,
) -> Result<Outcome, String> {
    // Name already taken: skip returns, rename picks a free name, overwrite keeps going.
    let target: PathBuf = if out_path.exists() {
        match conflict {
            "skip" => return Ok(Outcome::Skipped(SKIP_TAKEN.into())),
            "rename" => {
                let dir = out_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                let nm = out_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                dir.join(free_local_name(&dir, nm))
            }
            _ => out_path.to_path_buf(),
        }
    } else {
        out_path.to_path_buf()
    };
    let out_path: &std::path::Path = &target;

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part_path = dot_sibling(out_path, ".part");
    let meta_path = dot_sibling(out_path, ".bgdl");

    let head = clients[0]
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| s3_err(&e))?;
    let total = head.content_length().unwrap_or(0).max(0) as u64;
    let etag = head.e_tag().unwrap_or_default().to_string();

    let saved: Option<Resume> = fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Resume>(&s).ok());
    let resume_ok = part_path.exists()
        && saved
            .as_ref()
            .map(|r| r.key == key && r.total == total && r.etag == etag && r.chunk == DOWNLOAD_CHUNK)
            .unwrap_or(false);

    let chunks = total.div_ceil(DOWNLOAD_CHUNK);
    let mut watermark: u64 = match (&saved, resume_ok) {
        (Some(r), true) => r.done_chunks.min(chunks),
        _ => 0,
    };
    let write_sidecar = |done_chunks: u64| {
        let rec = Resume {
            key: key.to_string(),
            total,
            etag: etag.clone(),
            chunk: DOWNLOAD_CHUNK,
            done_chunks,
        };
        let _ = fs::write(&meta_path, serde_json::to_string(&rec).unwrap_or_default());
    };
    if !resume_ok {
        let _ = fs::remove_file(&part_path);
    }
    write_sidecar(watermark);

    // The file is created at its final size and every chunk is written where it belongs,
    // so chunks may arrive in any order.
    let part = OpenOptions::new()
        .create(true)
        .truncate(false) // keeps the chunks a previous run already wrote
        .read(true)
        .write(true)
        .open(&part_path)
        .map_err(|e| e.to_string())?;
    part.set_len(total).map_err(|e| e.to_string())?;
    let part = Arc::new(Mutex::new(part));

    let file_started = Instant::now();
    let start_bytes = watermark * DOWNLOAD_CHUNK;
    let fetched = Arc::new(AtomicU64::new(start_bytes));
    let (base, grand_total, clock) = agg.unwrap_or((0, total, file_started));
    let emit = |done: u64| {
        let cur = base + done;
        let (spd_bytes, spd_clock) = match agg {
            Some(_) => (cur, clock),
            None => (done - start_bytes, file_started),
        };
        let secs = spd_clock.elapsed().as_secs_f64().max(0.001);
        emit_progress(app, id, cur, grand_total, (spd_bytes as f64 / secs) as u64);
    };
    emit(start_bytes);

    // Chunks in flight, plus the ones that finished ahead of the watermark.
    let mut running: JoinSet<Result<(u64, usize), String>> = JoinSet::new();
    let mut ahead: BTreeSet<u64> = BTreeSet::new();
    let mut next = watermark;
    let mut tick = tokio::time::interval(DOWNLOAD_TICK);
    let mut last_saved = watermark;
    // A stream slot is held by one chunk at a time, so each client carries one
    // chunk at a time; with a single client every slot shares it.
    let mut free_slots: Vec<usize> = (0..DOWNLOAD_STREAMS).rev().collect();

    loop {
        while next < chunks
            && let Some(slot) = free_slots.pop()
        {
            let idx = next;
            next += 1;
            let off = idx * DOWNLOAD_CHUNK;
            let end = (off + DOWNLOAD_CHUNK).min(total) - 1;
            let (app, id) = (app.clone(), id.to_string());
            let client = clients[slot % clients.len()].clone();
            let (bucket, key, etag) = (bucket.to_string(), key.to_string(), etag.clone());
            let (cancel, part, fetched) = (cancel.clone(), part.clone(), fetched.clone());
            running.spawn(async move {
                // Every chunk must come from the version the HEAD above saw.
                let etag = (!etag.is_empty()).then_some(etag.as_str());
                let data =
                    get_range_retry(&app, &id, &client, &bucket, &key, off, end, etag, &cancel)
                        .await?;
                let mut f = part.lock().map_err(|e| e.to_string())?;
                f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                f.write_all(&data).map_err(|e| e.to_string())?;
                fetched.fetch_add(data.len() as u64, Ordering::Relaxed);
                Ok((idx, slot))
            });
        }
        if running.is_empty() {
            break;
        }

        let idx = tokio::select! {
            joined = running.join_next() => match joined {
                Some(res) => match res.map_err(|e| e.to_string())? {
                    Ok((idx, slot)) => {
                        free_slots.push(slot);
                        idx
                    }
                    Err(detail) => {
                        running.abort_all();
                        if detail == ERR_CHANGED {
                            // What was fetched belongs to a version that is gone,
                            // so there is nothing to resume from.
                            running.shutdown().await; // the tasks let go of the file
                            drop(part);
                            let _ = fs::remove_file(&part_path);
                            let _ = fs::remove_file(&meta_path);
                        } else {
                            write_sidecar(watermark);
                        }
                        return Err(detail);
                    }
                },
                None => continue,
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    running.abort_all();
                    write_sidecar(watermark);
                    return Ok(Outcome::Canceled); // `.part` and its sidecar stay for a resume
                }
                emit(fetched.load(Ordering::Relaxed));
                continue;
            }
        };

        ahead.insert(idx);
        while ahead.remove(&watermark) {
            watermark += 1;
        }
        if watermark != last_saved {
            write_sidecar(watermark);
            last_saved = watermark;
        }
    }

    if let Ok(f) = part.lock() {
        f.sync_data().ok();
    }
    drop(part);
    emit(total);

    // Every chunk arrived at its full length, so the file is complete. Its size
    // proves nothing on its own: `.part` was created at full size.
    fs::rename(&part_path, out_path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&meta_path);
    Ok(Outcome::Done)
}

/// Every object under a prefix, paged, as (key, size, etag).
async fn list_objects_all(
    client: &Client,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<(String, u64, String)>, String> {
    let mut out = Vec::new();
    // The SDK's paginator follows the continuation token and stops when it
    // repeats; it never looks at `is_truncated`.
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let resp = page.map_err(|e| s3_err(&e))?;
        for o in resp.contents() {
            if let Some(k) = o.key() {
                out.push((
                    k.to_string(),
                    o.size().unwrap_or(0).max(0) as u64,
                    o.e_tag().unwrap_or_default().to_string(),
                ));
            }
        }
    }
    Ok(out)
}

/// Streams the chosen remote sources into one archive, member by member.
async fn run_download_zip(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;
    let dest_root = resolve_local(&job.dest_dir)?;
    fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;
    let base = job.name.strip_suffix(".zip").unwrap_or(&job.name);
    let zip_name = format!("{base}.zip");
    // The archive is built under a hidden name and takes its real one only when
    // complete. A `.zip` already sitting there is the user's own and is never
    // opened, resumed into or deleted.
    let final_path = dest_root.join(&zip_name);
    let out_path = dot_sibling(&final_path, ".part");

    // Members as (remote key, path inside the archive, size, etag).
    let mut members: Vec<(String, String, u64, String)> = Vec::new();
    for zs in &job.zip_srcs {
        if zs.is_dir {
            let prefix = as_prefix(&zs.path);
            let top = prefix
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            for (key, sz, etag) in list_objects_all(&client, &bucket, &prefix).await? {
                if key.ends_with("/.keep") {
                    continue;
                }
                let rel = key.strip_prefix(&prefix).unwrap_or(&key);
                if rel.is_empty() {
                    continue;
                }
                // The archive is extracted by some other tool later, so a member
                // name that climbs out of the archive is refused here too.
                if !safe_rel(rel) {
                    return Err(format!("unsafe path in object name: {rel}"));
                }
                let arc = if top.is_empty() {
                    rel.to_string()
                } else {
                    format!("{top}/{rel}")
                };
                members.push((key, arc, sz, etag));
            }
        } else {
            let key = remote_key(&zs.path);
            let name = key.rsplit('/').next().unwrap_or(&key).to_string();
            let head = client
                .head_object()
                .bucket(&bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| s3_err(&e))?;
            let sz = head.content_length().unwrap_or(0).max(0) as u64;
            let etag = head.e_tag().unwrap_or_default().to_string();
            members.push((key, name, sz, etag));
        }
    }
    if members.is_empty() {
        return Err("zip: nothing to archive".into());
    }

    let grand_total: u64 = members.iter().map(|(_, _, s, _)| *s).sum();

    // A zip is resumable: members already inside the archive with the right size are skipped,
    // and before each new member the archive is copied to a `.ckpt` file it can be restored from.
    let ckpt = dot_sibling(&final_path, ".ckpt");

    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    if out_path.exists() {
        if let Ok(existing) = fs::File::open(&out_path)
            && let Ok(mut ar) = zip::ZipArchive::new(existing)
        {
            for (_, arc_name, sz, _) in &members {
                if ar.by_name(arc_name).is_ok_and(|e| e.size() == *sz) {
                    done.insert(arc_name.clone());
                }
            }
        }
        if done.is_empty() {
            let _ = fs::remove_file(&out_path); // leftover that matches nothing
        }
    }
    if !out_path.exists() {
        // A fresh run starts from an empty but valid archive, so the loop below has only one path.
        zip::ZipWriter::new(fs::File::create(&out_path).map_err(|e| e.to_string())?)
            .finish()
            .map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_file(&ckpt); // may be left over from an earlier run

    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);

    let clock = Instant::now();
    // On a resume the already archived members count as progress from the start.
    let mut written: u64 = members
        .iter()
        .filter(|(_, arc, _, _)| done.contains(arc))
        .map(|(_, _, sz, _)| *sz)
        .sum();
    let mut last_emit = Instant::now() - EMIT_EVERY;
    emit_progress(app, &job.id, written, grand_total, 0);

    for (key, arc_name, sz, etag) in &members {
        if done.contains(arc_name) {
            continue; // this member is already in the archive
        }
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }

        // Save the still-valid archive before touching it.
        fs::copy(&out_path, &ckpt).map_err(|e| e.to_string())?;
        let restore = |out: &std::path::Path, ckpt: &std::path::Path| {
            let _ = fs::copy(ckpt, out);
        };

        let f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&out_path)
            .map_err(|e| e.to_string())?;
        let mut zip = match zip::ZipWriter::new_append(f) {
            Ok(z) => z,
            Err(e) => {
                restore(&out_path, &ckpt);
                return Err(e.to_string());
            }
        };
        if let Err(e) = zip.start_file(arc_name.as_str(), opts) {
            drop(zip);
            restore(&out_path, &ckpt);
            return Err(e.to_string());
        }

        let total = *sz;
        let mut off: u64 = 0;
        let mut canceled = false;
        while off < total {
            if job.cancel.load(Ordering::Relaxed) {
                canceled = true;
                break;
            }
            let end = (off + CHUNK).min(total) - 1;
            let etag = (!etag.is_empty()).then_some(etag.as_str());
            let data = match get_range_retry(app, &job.id, &client, &bucket, key, off, end, etag, &job.cancel).await {
                Ok(d) => d,
                Err(e) => {
                    drop(zip);
                    restore(&out_path, &ckpt);
                    return Err(e);
                }
            };
            if let Err(e) = zip.write_all(&data) {
                drop(zip);
                restore(&out_path, &ckpt);
                return Err(e.to_string());
            }
            off += data.len() as u64;
            written += data.len() as u64;
            if last_emit.elapsed() >= EMIT_EVERY {
                let secs = clock.elapsed().as_secs_f64().max(0.001);
                emit_progress(
                    app,
                    &job.id,
                    written,
                    grand_total,
                    (written as f64 / secs) as u64,
                );
                last_emit = Instant::now();
            }
        }

        if canceled {
            drop(zip); // no finish: this member is half written
            restore(&out_path, &ckpt);
            let _ = fs::remove_file(&ckpt);
            return Ok(Outcome::Canceled);
        }
        if let Err(e) = zip.finish() {
            restore(&out_path, &ckpt);
            return Err(e.to_string());
        }
    }

    let _ = fs::remove_file(&ckpt);
    // A name taken in the meantime, or from the start, gets the next " copy" name.
    let name = free_local_name(&dest_root, &zip_name);
    fs::rename(&out_path, dest_root.join(&name)).map_err(|e| e.to_string())?;
    emit_progress(app, &job.id, grand_total, grand_total, 0);
    Ok(Outcome::Done)
}

// Same-side jobs: local→local and remote→remote copy and move, on the same queue and events.

async fn run_local_copy(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let is_move = job.kind.as_str() == "localMove";
    let src = resolve_local(&job.src)?;
    let dest = resolve_local(&job.dest_dir)?.join(&job.name);

    // A folder cannot go into itself; symlinks are resolved before deciding.
    if job.is_dir && local_is_within(&src, &dest) {
        return Err(ERR_INTO_ITSELF.into());
    }
    if src == dest {
        return Ok(Outcome::Skipped(SKIP_SAME.into()));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // A move inside one filesystem is a rename: instant, and it never needs twice the space.
    if is_move && !dest.exists() && fs::rename(&src, &dest).is_ok() {
        emit_progress(app, &job.id, 1, 1, 0);
        return Ok(Outcome::Done);
    }

    if job.is_dir {
        fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let (files, empty_dirs) = walk_files(&src)?;
        // Empty subfolders are created explicitly; walking only files would lose them.
        for d in &empty_dirs {
            fs::create_dir_all(dest.join(d)).map_err(|e| e.to_string())?;
        }
        let total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
        let clock = Instant::now();
        let mut done: u64 = 0;
        emit_progress(app, &job.id, 0, total, 0);
        for (path, sz, rel) in &files {
            if job.cancel.load(Ordering::Relaxed) {
                return Ok(Outcome::Canceled);
            }
            let out = dest.join(rel);
            // A skipped file was not sent, so a Move must leave it in place.
            if job.conflict == "skip" && out.exists() {
                done += *sz;
                continue;
            }
            emit_current_file(app, &job.id, rel);
            match copy_file_chunked(
                app,
                &job.id,
                &job.cancel,
                path,
                &out,
                Some((done, total, clock)),
                &job.conflict,
            )? {
                Outcome::Canceled => return Ok(Outcome::Canceled),
                Outcome::Done => job.sent_local.lock().unwrap().push(path.clone()),
                Outcome::Skipped(_) => {}
            }
            done += *sz;
        }
        emit_progress(app, &job.id, total, total, 0);
        Ok(Outcome::Done)
    } else {
        let total = fs::metadata(&src).map_err(|e| e.to_string())?.len();
        emit_progress(app, &job.id, 0, total, 0);
        copy_file_chunked(app, &job.id, &job.cancel, &src, &dest, None, &job.conflict)
    }
}

/// Copies one local file through a `.part` file and renames it into place when done.
#[allow(clippy::too_many_arguments)]
fn copy_file_chunked(
    app: &AppHandle,
    id: &str,
    cancel: &AtomicBool,
    src: &std::path::Path,
    dst: &std::path::Path,
    agg: Agg,
    conflict: &str,
) -> Result<Outcome, String> {
    use std::io::Read;

    let target: PathBuf = if dst.exists() {
        match conflict {
            "skip" => return Ok(Outcome::Skipped(SKIP_TAKEN.into())),
            "rename" => {
                let dir = dst.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let nm = dst.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                dir.join(free_local_name(&dir, nm))
            }
            _ => dst.to_path_buf(),
        }
    } else {
        dst.to_path_buf()
    };
    let dst: &std::path::Path = &target;

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dot_sibling(dst, ".part");
    let mut reader = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut writer = fs::File::create(&tmp).map_err(|e| e.to_string())?;

    let total = fs::metadata(src).map_err(|e| e.to_string())?.len();
    let (base, grand_total, clock) = agg.unwrap_or((0, total, Instant::now()));
    let started = Instant::now();
    let mut done: u64 = 0;
    let mut last_emit = Instant::now() - EMIT_EVERY;
    let mut buf = vec![0u8; CHUNK as usize];

    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = fs::remove_file(&tmp);
            return Ok(Outcome::Canceled);
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last_emit.elapsed() >= EMIT_EVERY {
            let cur = base + done;
            let (spd_bytes, spd_clock) = match agg {
                Some(_) => (cur, clock),
                None => (done, started),
            };
            let secs = spd_clock.elapsed().as_secs_f64().max(0.001);
            emit_progress(app, id, cur, grand_total, (spd_bytes as f64 / secs) as u64);
            last_emit = Instant::now();
        }
    }

    writer.sync_data().ok();
    drop(writer);
    fs::rename(&tmp, dst).map_err(|e| e.to_string())?;
    Ok(Outcome::Done)
}

async fn run_remote_copy(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;

    let name = &job.name;

    if !job.is_dir {
        let src_key = remote_key(&job.src);
        let dest_prefix = as_prefix(&job.dest_dir);
        // Conflict handling, with HEAD as the existence check.
        let dest_key = match job.conflict.as_str() {
            "rename" => format!(
                "{dest_prefix}{}",
                free_remote_name(&client, &bucket, &dest_prefix, name).await
            ),
            "skip" => {
                let k = format!("{dest_prefix}{name}");
                if head_ok(&client, &bucket, &k).await {
                    return Ok(Outcome::Skipped(SKIP_TAKEN.into()));
                }
                k
            }
            _ => format!("{dest_prefix}{name}"), // overwrite: the PUT replaces it
        };
        if src_key == dest_key {
            return Ok(Outcome::Skipped(SKIP_SAME.into()));
        }
        // The result is passed through unchanged.
        return remote_copy_stream(
            app, &client, &bucket, &src_key, &dest_key, &job.id, None, &job.cancel,
        )
        .await;
    }

    // A folder copies every object under the prefix, `.keep` markers included.
    let src_prefix = as_prefix(&job.src);
    let dest_prefix = format!("{}{}/", as_prefix(&job.dest_dir), name);
    // Copying into itself would write into the source tree, and a Move would then delete both.
    if dest_prefix.starts_with(&src_prefix) {
        return Err(ERR_INTO_ITSELF.into());
    }
    let objects = list_objects_all(&client, &bucket, &src_prefix).await?;
    let total: u64 = objects.iter().map(|(_, sz, _)| *sz).sum();
    let clock = Instant::now();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total, 0);

    // With `skip`, an object already at the destination is left as it is and its
    // source is not counted as sent, the same as a folder download does.
    let taken: std::collections::HashSet<String> = if job.conflict == "skip" {
        list_objects_all(&client, &bucket, &dest_prefix)
            .await?
            .into_iter()
            .map(|(k, _, _)| k)
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    for (key, sz, _) in &objects {
        let rel = key.strip_prefix(&src_prefix).unwrap_or(key);
        let dest_key = format!("{dest_prefix}{rel}");
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        if rel.is_empty() {
            continue;
        }
        if taken.contains(&dest_key) {
            done += *sz;
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match remote_copy_stream(
            app, &client, &bucket, key, &dest_key, &job.id,
            Some((done, total, clock)), &job.cancel,
        )
        .await?
        {
            Outcome::Canceled => return Ok(Outcome::Canceled),
            Outcome::Done => job.sent_remote.lock().unwrap().push(key.clone()),
            Outcome::Skipped(_) => {}
        }
        done += *sz;
    }
    emit_progress(app, &job.id, total, total, 0);
    Ok(Outcome::Done)
}

/// Runs an operation up to three times, for gateway errors that pass on their own.
async fn retry3<T, E, F, Fut>(mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut last: Option<E> = None;
    for attempt in 0u32..3 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(
                        100 + 200 * u64::from(attempt),
                    ))
                    .await;
                }
            }
        }
    }
    Err(last.expect("the loop above always leaves an error behind"))
}

/// Largest object `CopyObject` copies in one request, per the S3 limit.
const COPY_OBJECT_MAX: u64 = 5 * 1024 * 1024 * 1024;

/// Server-side copy of one object, checked by size afterwards.
async fn copy_object_checked(
    client: &Client,
    bucket: &str,
    src_key: &str,
    dest_key: &str,
    total: u64,
) -> Result<(), String> {
    // The source goes unencoded: the gateway looks the header up as written.
    client
        .copy_object()
        .bucket(bucket)
        .key(dest_key)
        .copy_source(format!("{bucket}/{src_key}"))
        .send()
        .await
        .map_err(|e| s3_err(&e))?;
    verify_object_size(client, bucket, dest_key, total).await
}

/// Copies one object inside the bucket: on the server with `CopyObject` when it
/// can, otherwise by reading it and writing it back.
///
/// The server-side copy does not pass through this machine, so a large file
/// takes seconds instead of a download and an upload. Any failure of it falls
/// back to the read-and-write path, which has always worked.
#[allow(clippy::too_many_arguments)]
async fn remote_copy_stream(
    app: &AppHandle,
    client: &Client,
    bucket: &str,
    src_key: &str,
    dest_key: &str,
    id: &str,
    agg: Agg,
    cancel: &AtomicBool,
) -> Result<Outcome, String> {
    let head = retry3(|| client.head_object().bucket(bucket).key(src_key).send())
        .await
        .map_err(|e| format!("HEAD {src_key}: {}", s3_err(&e)))?;
    let total = head.content_length().unwrap_or(0).max(0) as u64;
    let (base, grand_total, clock) = agg.unwrap_or((0, total, Instant::now()));

    if total == 0 {
        retry3(|| {
            client
                .put_object()
                .bucket(bucket)
                .key(dest_key)
                .body(ByteStream::from_static(b""))
                .send()
        })
        .await
        .map_err(|e| format!("PUT {dest_key}: {}", s3_err(&e)))?;
        emit_progress(app, id, base, grand_total, 0);
        return Ok(Outcome::Done);
    }

    if total <= COPY_OBJECT_MAX {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        match copy_object_checked(client, bucket, src_key, dest_key, total).await {
            Ok(()) => {
                emit_progress(app, id, base + total, grand_total, 0);
                return Ok(Outcome::Done);
            }
            Err(e) => crate::devlog::verbose(
                "transfer",
                format!(
                    "CopyObject {src_key:?} -> {dest_key:?} failed ({e}), copying through the app"
                ),
            ),
        }
    }

    if total <= CHUNK {
        let obj = retry3(|| client.get_object().bucket(bucket).key(src_key).send())
            .await
            .map_err(|e| format!("GET {src_key}: {}", s3_err(&e)))?;
        let data = obj.body.collect().await.map_err(|e| e.to_string())?.into_bytes();
        put_object_verified(client, bucket, dest_key, &data).await?;
        emit_progress(app, id, base + total, grand_total, 0);
        return Ok(Outcome::Done);
    }

    // Larger objects go part by part; the part size keeps S3's 10.000 part limit.
    let part_size = std::cmp::max(CHUNK, total.div_ceil(10_000));
    let num_parts = total.div_ceil(part_size) as i32;
    let create = retry3(|| {
        client
            .create_multipart_upload()
            .bucket(bucket)
            .key(dest_key)
            .send()
    })
    .await
    .map_err(|e| format!("CreateMultipartUpload {dest_key}: {}", s3_err(&e)))?;
    let upload_id = create.upload_id().unwrap_or_default().to_string();
    if upload_id.is_empty() {
        return Err("gateway returned no UploadId".into());
    }

    let mut parts: Vec<CompletedPart> = Vec::new();
    let mut sent: u64 = 0;
    let started = Instant::now();
    let mut last_emit = Instant::now() - EMIT_EVERY;

    for pn in 1..=num_parts {
        if cancel.load(Ordering::Relaxed) {
            let _ = client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(dest_key)
                .upload_id(&upload_id)
                .send()
                .await;
            return Ok(Outcome::Canceled);
        }
        let off = (pn as u64 - 1) * part_size;
        let end = (off + part_size).min(total) - 1;
        let range = format!("bytes={off}-{end}");
        let obj = retry3(|| {
            client
                .get_object()
                .bucket(bucket)
                .key(src_key)
                .range(&range)
                .send()
        })
        .await
        .map_err(|e| format!("GET {src_key} {range}: {}", s3_err(&e)))?;
        let data = obj.body.collect().await.map_err(|e| e.to_string())?.into_bytes();
        let n = data.len() as u64;
        let out = retry3(|| {
            client
                .upload_part()
                .bucket(bucket)
                .key(dest_key)
                .upload_id(&upload_id)
                .part_number(pn)
                .body(ByteStream::from(data.to_vec()))
                .send()
        })
        .await
        .map_err(|e| format!("UploadPart {dest_key} #{pn}: {}", s3_err(&e)))?;
        parts.push(
            CompletedPart::builder()
                .part_number(pn)
                .set_e_tag(out.e_tag().map(str::to_string))
                .build(),
        );
        sent += n;
        if last_emit.elapsed() >= EMIT_EVERY {
            let cur = base + sent;
            let (sb, sc) = match agg {
                Some(_) => (cur, clock),
                None => (sent, started),
            };
            let secs = sc.elapsed().as_secs_f64().max(0.001);
            emit_progress(app, id, cur, grand_total, (sb as f64 / secs) as u64);
            last_emit = Instant::now();
        }
    }

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    retry3(|| {
        client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(dest_key)
            .upload_id(&upload_id)
            .multipart_upload(completed.clone())
            .send()
    })
    .await
    .map_err(|e| format!("CompleteMultipartUpload {dest_key}: {}", s3_err(&e)))?;
    verify_object_size(client, bucket, dest_key, total).await?;
    Ok(Outcome::Done)
}

/// Sidecar next to an uploaded file; lets a later run continue the same multipart upload.
#[derive(Serialize, Deserialize)]
struct UploadResume {
    key: String,
    upload_id: String,
    part_size: u64,
    size: u64,
    mtime: u64,
}

fn mtime_secs(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sidecar_of(src: &std::path::Path) -> PathBuf {
    dot_sibling(src, ".bgul")
}

/// Path of a transfer's side file: `parent/.<name><suffix>`, hidden from the listing by the dot.
fn dot_sibling(path: &std::path::Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let mut fname = std::ffi::OsString::from(".");
    fname.push(&name);
    fname.push(suffix);
    match path.parent() {
        Some(p) => p.join(fname),
        None => PathBuf::from(fname),
    }
}

async fn run_upload(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;
    let src_root = resolve_local(&job.src)?;

    let name = &job.name;

    if !job.is_dir {
        let dest_prefix = as_prefix(&job.dest_dir);
        // Conflict handling; an overwrite needs nothing special because the upload replaces the key.
        let key = match job.conflict.as_str() {
            "rename" => format!(
                "{dest_prefix}{}",
                free_remote_name(&client, &bucket, &dest_prefix, name).await
            ),
            "skip" => {
                let k = format!("{dest_prefix}{name}");
                if head_ok(&client, &bucket, &k).await {
                    return Ok(Outcome::Skipped(SKIP_TAKEN.into()));
                }
                k
            }
            _ => format!("{dest_prefix}{name}"),
        };
        return upload_one(
            app, &client, &bucket, &key, &src_root, &job.id, &job.cancel, None,
        )
        .await;
    }

    // A folder job walks the tree and uploads every file under the same relative path.
    let dest_prefix = format!("{}{}/", as_prefix(&job.dest_dir), name);
    let (files, empty_dirs) = walk_files(&src_root)?;
    let total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
    let clock = Instant::now();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total, 0);

    // With `skip`, an object already at the destination is left as it is and its
    // file is not counted as sent. That is also what lets a half-done folder
    // upload continue: an unfinished multipart upload is not an object yet.
    let existing: std::collections::HashSet<String> = if job.conflict == "skip" {
        list_objects_all(&client, &bucket, &dest_prefix)
            .await?
            .into_iter()
            .map(|(key, _, _)| key)
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    for (path, sz, rel) in &files {
        let key = format!("{dest_prefix}{rel}");
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        if existing.contains(&key) {
            done += *sz;
            emit_progress(app, &job.id, done, total, 0);
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match upload_one(
            app, &client, &bucket, &key, path, &job.id, &job.cancel,
            Some((done, total, clock)),
        )
        .await?
        {
            Outcome::Canceled => return Ok(Outcome::Canceled),
            Outcome::Done => job.sent_local.lock().unwrap().push(path.clone()),
            Outcome::Skipped(_) => {}
        }
        done += *sz;
    }
    // Empty folders are represented by a `.keep` object, the same way New Folder does it.
    for d in &empty_dirs {
        let key = if d.is_empty() {
            format!("{dest_prefix}.keep")
        } else {
            format!("{dest_prefix}{d}/.keep")
        };
        let _ = client
            .put_object()
            .bucket(&bucket)
            .key(&key)
            .body(ByteStream::from_static(b""))
            .send()
            .await;
    }
    emit_progress(app, &job.id, total, total, 0);
    Ok(Outcome::Done)
}

/// Walks a directory tree: every file as (path, size, relative path), plus the empty folders.
type WalkResult = (Vec<(PathBuf, u64, String)>, Vec<String>);
fn walk_files(root: &std::path::Path) -> Result<WalkResult, String> {
    let mut out = Vec::new();
    let mut empty_dirs = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel_dir)) = stack.pop() {
        let rd = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut any_entry = false;
        for item in rd.flatten() {
            any_entry = true;
            let p = item.path();
            let Ok(meta) = item.metadata() else { continue };
            let name = item.file_name().to_string_lossy().into_owned();
            let child_rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            if meta.is_dir() {
                stack.push((p, child_rel));
                continue;
            }
            // Leftovers of an interrupted transfer are not uploaded.
            if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("bgul" | "bgdl" | "part")
            ) {
                continue;
            }
            out.push((p, meta.len(), child_rel));
        }
        if !any_entry {
            empty_dirs.push(rel_dir);
        }
    }
    Ok((out, empty_dirs))
}

/// Uploads one file, in parts, continuing an earlier attempt when the sidecar matches.
#[allow(clippy::too_many_arguments)]
async fn upload_one(
    app: &AppHandle,
    client: &Client,
    bucket: &str,
    key: &str,
    src: &std::path::Path,
    id: &str,
    cancel: &AtomicBool,
    agg: Agg,
) -> Result<Outcome, String> {
    use std::io::{Read, Seek, SeekFrom};

    let meta = fs::metadata(src).map_err(|e| e.to_string())?;
    let total = meta.len();
    let mtime = mtime_secs(&meta);
    let sidecar = sidecar_of(src);

    let (base, grand_total, clock) = agg.unwrap_or((0, total, Instant::now()));

    // An empty file cannot be a multipart upload with zero parts.
    if total == 0 {
        client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| s3_err(&e))?;
        emit_progress(app, id, base, grand_total, 0);
        return Ok(Outcome::Done);
    }

    // A small file goes in one verified request; splitting it would cost three and gain nothing.
    if total <= CHUNK {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        let data = fs::read(src).map_err(|e| e.to_string())?;
        if data.len() as u64 != total {
            return Err(format!("{}: size changed during upload", src.display()));
        }
        put_object_verified(client, bucket, key, &data).await?;
        // A sidecar from an earlier multipart attempt is now meaningless.
        let _ = fs::remove_file(&sidecar);
        emit_progress(app, id, base + total, grand_total, 0);
        return Ok(Outcome::Done);
    }

    let default_part = std::cmp::max(CHUNK, total.div_ceil(10_000));

    let resumed: Option<UploadResume> = fs::read_to_string(&sidecar)
        .ok()
        .and_then(|s| serde_json::from_str::<UploadResume>(&s).ok())
        .filter(|r| r.key == key && r.size == total && r.mtime == mtime);

    let (upload_id, part_size, mut done) = match resumed {
        Some(r) => match list_parts_all(client, bucket, key, &r.upload_id).await {
            Ok(done) => (r.upload_id, r.part_size, done),
            Err(_) => {
                let _ = fs::remove_file(&sidecar);
                fresh_upload(client, bucket, key, default_part, total, mtime, &sidecar).await?
            }
        },
        None => {
            fresh_upload(client, bucket, key, default_part, total, mtime, &sidecar).await?
        }
    };

    let num_parts: i32 = total.div_ceil(part_size) as i32;
    let mut file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; part_size as usize];
    let mut parts: Vec<CompletedPart> = Vec::new();
    let mut sent: u64 = 0;
    let started = Instant::now();
    let mut resume_bytes: u64 = 0;
    let mut last_emit = Instant::now() - EMIT_EVERY;

    for pn in 1..=num_parts {
        let offset = (pn as u64 - 1) * part_size;
        let part_len = (total - offset).min(part_size);

        if let Some(etag) = done.remove(&pn) {
            parts.push(CompletedPart::builder().part_number(pn).e_tag(etag).build());
            sent += part_len;
            resume_bytes += part_len;
            continue;
        }

        if cancel.load(Ordering::Relaxed) {
            // No abort: the sidecar and the server-side upload stay, so this can continue
            return Ok(Outcome::Canceled);
        }

        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let slice = &mut buf[..part_len as usize];
        file.read_exact(slice).map_err(|e| e.to_string())?;

        // The SDK retries a failed part itself; the notice puts each retry in
        // the queue row and the log, the way a download's retries show.
        let attempts = Arc::new(AtomicU32::new(0));
        let out = client
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .part_number(pn)
            .body(ByteStream::from(slice.to_vec()))
            .customize()
            .config_override(
                aws_sdk_s3::config::Builder::new()
                    .retry_config(RetryConfig::standard().with_max_attempts(PART_ATTEMPTS)),
            )
            .interceptor(PartRetryNotice {
                app: app.clone(),
                id: id.to_string(),
                what: format!("part {pn} of {key:?}"),
                attempts: attempts.clone(),
                last_error: Mutex::new(String::new()),
            })
            .send()
            .await
            .map_err(|e| s3_err(&e))?; // the sidecar survives, so a retry resumes
        if attempts.load(Ordering::Relaxed) > 1 {
            emit_retry(app, id, 0, PART_ATTEMPTS); // went through in the end
        }
        parts.push(
            CompletedPart::builder()
                .part_number(pn)
                .set_e_tag(out.e_tag().map(str::to_string))
                .build(),
        );
        sent += part_len;

        if last_emit.elapsed() >= EMIT_EVERY || sent >= total {
            let cur = base + sent;
            let (spd_bytes, spd_clock) = match agg {
                Some(_) => (cur, clock),
                None => (sent - resume_bytes, started),
            };
            let secs = spd_clock.elapsed().as_secs_f64().max(0.001);
            emit_progress(app, id, cur, grand_total, (spd_bytes as f64 / secs) as u64);
            last_emit = Instant::now();
        }
    }

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    client
        .complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(&upload_id)
        .multipart_upload(completed)
        .send()
        .await
        .map_err(|e| s3_err(&e))?; // the sidecar survives, so a retry resumes

    let _ = fs::remove_file(&sidecar);
    verify_object_size(client, bucket, key, total).await?;
    Ok(Outcome::Done)
}

/// Makes the SDK's own retries of one upload part visible: each one shows in
/// the queue row, as a download's do, and gets a line in `verbose.log`.
#[derive(Debug)]
struct PartRetryNotice {
    app: AppHandle,
    id: String,
    /// Names the part in the log line.
    what: String,
    attempts: Arc<AtomicU32>,
    last_error: Mutex<String>,
}

impl Intercept for PartRetryNotice {
    fn name(&self) -> &'static str {
        "PartRetryNotice"
    }

    fn read_before_attempt(
        &self,
        _ctx: &BeforeTransmitInterceptorContextRef<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        if attempt > 1 {
            let detail = self.last_error.lock().map(|e| e.clone()).unwrap_or_default();
            crate::devlog::verbose(
                "transfer",
                format!("{} failed ({detail}), retry {}", self.what, attempt - 1),
            );
            emit_retry(&self.app, &self.id, attempt - 1, PART_ATTEMPTS);
        }
        Ok(())
    }

    fn read_after_attempt(
        &self,
        ctx: &FinalizerInterceptorContextRef<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        if let Some(Err(e)) = ctx.output_or_error()
            && let Ok(mut last) = self.last_error.lock()
        {
            *last = DisplayErrorContext(e).to_string();
        }
        Ok(())
    }
}

/// Starts a new multipart upload and writes its sidecar.
async fn fresh_upload(
    client: &Client,
    bucket: &str,
    key: &str,
    part_size: u64,
    size: u64,
    mtime: u64,
    sidecar: &std::path::Path,
) -> Result<(String, u64, HashMap<i32, String>), String> {
    let create = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| s3_err(&e))?;
    let upload_id = create.upload_id().unwrap_or_default().to_string();
    if upload_id.is_empty() {
        return Err("gateway returned no UploadId".into());
    }
    let rec = UploadResume {
        key: key.to_string(),
        upload_id: upload_id.clone(),
        part_size,
        size,
        mtime,
    };
    let _ = fs::write(sidecar, serde_json::to_string(&rec).unwrap_or_default());
    Ok((upload_id, part_size, HashMap::new()))
}

/// The marker that asks for the next page of `ListParts`, or `None` when the
/// page just read was the last.
///
/// Servers disagree about what a last page carries — RunPod sends no marker,
/// MinIO sends `"0"` — but both set `is_truncated` right, so that decides. A
/// marker that did not move ends the listing too, whatever the server claims.
fn next_page_marker(
    truncated: Option<bool>,
    next: Option<&str>,
    sent: Option<&str>,
) -> Option<String> {
    let next = next.filter(|m| !m.is_empty())?;
    (truncated == Some(true) && Some(next) != sent).then(|| next.to_string())
}

/// `next_page_marker` for `ListMultipartUploads`, whose marker has two parts:
/// the key can stay the same while the upload id moves on.
fn next_uploads_marker(
    truncated: Option<bool>,
    next: (Option<&str>, Option<&str>),
    sent: (Option<&str>, Option<&str>),
) -> Option<(String, Option<String>)> {
    let key = next.0.filter(|m| !m.is_empty())?;
    let id = next.1.filter(|m| !m.is_empty());
    (truncated == Some(true) && (Some(key), id) != sent)
        .then(|| (key.to_string(), id.map(str::to_string)))
}

/// Parts the gateway already holds for an upload, as part number to ETag.
async fn list_parts_all(
    client: &Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
) -> Result<HashMap<i32, String>, String> {
    let mut done = HashMap::new();
    let mut marker: Option<String> = None;
    loop {
        let mut req = client
            .list_parts()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id);
        if let Some(m) = &marker {
            req = req.part_number_marker(m);
        }
        let resp = req.send().await.map_err(|e| s3_err(&e))?;
        for p in resp.parts() {
            if let (Some(n), Some(et)) = (p.part_number(), p.e_tag()) {
                done.insert(n, et.to_string());
            }
        }
        marker = next_page_marker(
            resp.is_truncated(),
            resp.next_part_number_marker(),
            marker.as_deref(),
        );
        if marker.is_none() {
            break;
        }
    }
    Ok(done)
}

// Multipart uploads that were never completed keep taking space on the volume, invisible in
// the listing. They are swept at startup when old, or listed and aborted from Settings.

const ORPHAN_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrphanUpload {
    key: String,
    upload_id: String,
    /// When the upload started, in epoch milliseconds; `None` when the gateway does not say.
    initiated_ms: Option<i64>,
    /// Total size of the parts uploaded so far.
    bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanRef {
    key: String,
    upload_id: String,
}

/// Every unfinished multipart upload in the bucket, as (key, upload id, start time).
async fn list_multipart_uploads_all(
    client: &Client,
    bucket: &str,
) -> Result<Vec<(String, String, Option<i64>)>, String> {
    let mut out = Vec::new();
    let mut key_marker: Option<String> = None;
    let mut id_marker: Option<String> = None;
    loop {
        let mut req = client.list_multipart_uploads().bucket(bucket);
        if let Some(k) = &key_marker {
            req = req.key_marker(k);
        }
        if let Some(i) = &id_marker {
            req = req.upload_id_marker(i);
        }
        let resp = req.send().await.map_err(|e| s3_err(&e))?;
        for u in resp.uploads() {
            if let (Some(k), Some(id)) = (u.key(), u.upload_id()) {
                let ms = u
                    .initiated()
                    .map(|d| d.secs() * 1000 + i64::from(d.subsec_nanos()) / 1_000_000);
                out.push((k.to_string(), id.to_string(), ms));
            }
        }
        match next_uploads_marker(
            resp.is_truncated(),
            (resp.next_key_marker(), resp.next_upload_id_marker()),
            (key_marker.as_deref(), id_marker.as_deref()),
        ) {
            Some((k, i)) => {
                key_marker = Some(k);
                id_marker = i;
            }
            None => return Ok(out),
        }
    }
}

/// Total size of the parts already uploaded for one unfinished upload.
async fn sum_parts_bytes(
    client: &Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
) -> u64 {
    let mut total: u64 = 0;
    let mut marker: Option<String> = None;
    loop {
        let mut req = client
            .list_parts()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id);
        if let Some(m) = &marker {
            req = req.part_number_marker(m);
        }
        let Ok(resp) = req.send().await else { break };
        for p in resp.parts() {
            total += p.size().unwrap_or(0).max(0) as u64;
        }
        marker = next_page_marker(
            resp.is_truncated(),
            resp.next_part_number_marker(),
            marker.as_deref(),
        );
        if marker.is_none() {
            break;
        }
    }
    total
}

/// Unfinished uploads with the space each one holds; Settings lists them with this.
#[tauri::command]
pub async fn list_orphan_uploads(app: AppHandle) -> Result<Vec<OrphanUpload>, String> {
    let (client, bucket) = s3(&app)?;
    let ups = list_multipart_uploads_all(&client, &bucket).await?;
    let mut out = Vec::with_capacity(ups.len());
    for (key, upload_id, initiated_ms) in ups {
        let bytes = sum_parts_bytes(&client, &bucket, &key, &upload_id).await;
        out.push(OrphanUpload {
            key,
            upload_id,
            initiated_ms,
            bytes,
        });
    }
    Ok(out)
}

/// Aborts the given uploads and returns how many were really removed.
#[tauri::command]
pub async fn abort_orphan_uploads(
    app: AppHandle,
    uploads: Vec<OrphanRef>,
) -> Result<u32, String> {
    let (client, bucket) = s3(&app)?;
    let mut n = 0;
    for u in uploads {
        if client
            .abort_multipart_upload()
            .bucket(&bucket)
            .key(&u.key)
            .upload_id(&u.upload_id)
            .send()
            .await
            .is_ok()
        {
            n += 1;
        }
    }
    Ok(n)
}

/// Silently aborts uploads older than a week; runs in the background at startup.
pub async fn sweep_old_orphans(app: AppHandle) {
    let Ok((client, bucket)) = s3(&app) else {
        return;
    };
    let Ok(ups) = list_multipart_uploads_all(&client, &bucket).await else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let cutoff = now_ms - ORPHAN_MAX_AGE_SECS * 1000;
    let mut n = 0;
    for (key, upload_id, initiated_ms) in ups {
        let Some(ms) = initiated_ms else { continue };
        if ms >= cutoff {
            continue;
        }
        if client
            .abort_multipart_upload()
            .bucket(&bucket)
            .key(&key)
            .upload_id(&upload_id)
            .send()
            .await
            .is_ok()
        {
            n += 1;
        }
    }
    if n > 0 {
        crate::devlog::verbose(
            "transfer",
            format!("orphan multipart sweep: aborted {n} upload(s) older than 7 days"),
        );
    }
}

fn emit_progress(app: &AppHandle, id: &str, done: u64, total: u64, speed: u64) {
    let _ = app.emit(
        "transfer-progress",
        ProgressEvt {
            id: id.to_string(),
            bytes_done: done,
            bytes_total: total,
            speed,
        },
    );
}

/// Tells the queue row that a chunk is being retried; `attempt` 0 clears the notice.
fn emit_retry(app: &AppHandle, id: &str, attempt: u32, of: u32) {
    let _ = app.emit(
        "transfer-retry",
        RetryEvt { id: id.to_string(), attempt, of },
    );
}

fn emit_current_file(app: &AppHandle, id: &str, name: &str) {
    crate::devlog::verbose("transfer", format!("file {id} {name:?}"));
    let _ = app.emit(
        "transfer-file",
        FileEvt { id: id.to_string(), name: name.to_string() },
    );
}

#[cfg(test)]
mod zip_resume_tests {
    // Proves the zip resume mechanism without touching S3: an interrupted member is thrown
    // away by restoring the checkpoint, and the archive stays readable.
    use std::io::Write;

    #[test]
    fn checkpoint_copy_survives_interrupted_member() {
        let dir = std::env::temp_dir().join(format!("bgzt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        let ckpt = dir.join(".t.zip.ckpt");

        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // A fresh archive with one member.
        let f = std::fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        zw.start_file("a.txt", opts).unwrap();
        zw.write_all(b"hello-a").unwrap();
        zw.finish().unwrap();

        // Checkpoint, exactly as the real code does before each member.
        std::fs::copy(&path, &ckpt).unwrap();

        // A second member is left half written, as a cancel would leave it.
        let f2 = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut zw2 = zip::ZipWriter::new_append(f2).unwrap();
        zw2.start_file("b.txt", opts).unwrap();
        zw2.write_all(b"partial-garbage-not-finished").unwrap();
        drop(zw2); // no finish: the file is broken from here on

        // Restore from the checkpoint.
        std::fs::copy(&ckpt, &path).unwrap();

        // The restored archive is valid again and holds only the finished member.
        let check = std::fs::File::open(&path).unwrap();
        let mut ar = zip::ZipArchive::new(check).unwrap();
        assert_eq!(ar.len(), 1, "only the finished member survives a restore");
        let mut a = ar.by_name("a.txt").unwrap();
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut a, &mut buf).unwrap();
        assert_eq!(buf, "hello-a");
        drop(a);
        assert!(ar.by_name("b.txt").is_err(), "the half written member must be gone");
        drop(ar);

        // The resume then writes that member fully.
        let f3 = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut zw3 = zip::ZipWriter::new_append(f3).unwrap();
        zw3.start_file("b.txt", opts).unwrap();
        zw3.write_all(b"hello-b").unwrap();
        zw3.finish().unwrap();

        // Both members are in the final archive with the right content.
        let final_f = std::fs::File::open(&path).unwrap();
        let mut ar2 = zip::ZipArchive::new(final_f).unwrap();
        assert_eq!(ar2.len(), 2);
        for (name, expect) in [("a.txt", "hello-a"), ("b.txt", "hello-b")] {
            let mut entry = ar2.by_name(name).unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut entry, &mut s).unwrap();
            assert_eq!(s, expect);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod page_marker_tests {
    use super::{next_page_marker, next_uploads_marker};

    // The answers below are what RunPod and MinIO really sent.

    #[test]
    fn list_parts_last_page_ends() {
        // RunPod: no marker at all.
        assert_eq!(next_page_marker(Some(false), None, None), None);
        // MinIO: "0" on the last page, parts or not.
        assert_eq!(next_page_marker(Some(false), Some("0"), None), None);
        assert_eq!(next_page_marker(Some(false), Some("0"), Some("4")), None);
    }

    #[test]
    fn list_parts_middle_page_continues() {
        assert_eq!(
            next_page_marker(Some(true), Some("2"), None),
            Some("2".into())
        );
        assert_eq!(
            next_page_marker(Some(true), Some("4"), Some("2")),
            Some("4".into())
        );
    }

    #[test]
    fn list_parts_stuck_marker_ends() {
        assert_eq!(next_page_marker(Some(true), Some("4"), Some("4")), None);
        assert_eq!(next_page_marker(Some(true), Some(""), None), None);
        assert_eq!(next_page_marker(Some(true), None, None), None);
        assert_eq!(next_page_marker(None, Some("2"), None), None);
    }

    #[test]
    fn uploads_last_page_ends() {
        // MinIO: everything on one page, an empty key marker.
        assert_eq!(
            next_uploads_marker(Some(false), (Some(""), None), (None, None)),
            None
        );
        // RunPod: no markers on the last page.
        assert_eq!(
            next_uploads_marker(Some(false), (None, None), (Some("/a.bin"), Some("u1"))),
            None
        );
    }

    #[test]
    fn uploads_middle_page_continues() {
        assert_eq!(
            next_uploads_marker(Some(true), (Some("/a.bin"), Some("u1")), (None, None)),
            Some(("/a.bin".into(), Some("u1".into())))
        );
        // The same key with its next upload: still moving.
        assert_eq!(
            next_uploads_marker(
                Some(true),
                (Some("/a.bin"), Some("u2")),
                (Some("/a.bin"), Some("u1"))
            ),
            Some(("/a.bin".into(), Some("u2".into())))
        );
    }

    #[test]
    fn uploads_stuck_marker_ends() {
        assert_eq!(
            next_uploads_marker(
                Some(true),
                (Some("/a.bin"), Some("u2")),
                (Some("/a.bin"), Some("u2"))
            ),
            None
        );
    }
}

#[cfg(test)]
mod name_tests {
    use super::{as_prefix, dot_sibling, nth_copy, safe_join, safe_rel, split_for_copy};
    use std::path::Path;

    #[test]
    fn a_relative_path_that_climbs_out_is_refused() {
        for rel in ["../x", "a/../../x", "/tmp/x", "", "."] {
            assert!(!safe_rel(rel), "{rel:?}");
            assert!(safe_join(Path::new("/dest"), rel).is_err(), "{rel:?}");
        }
        // A `.` between two names is dropped while the path is read, so the
        // result still lands inside the destination.
        for rel in ["x", "a/b/c.txt", "a b/.keep", "..a/b", "a..b", "a/./b"] {
            assert!(safe_rel(rel), "{rel:?}");
        }
        assert_eq!(
            safe_join(Path::new("/dest"), "a/b.txt").unwrap(),
            Path::new("/dest/a/b.txt")
        );
    }

    fn pair(stem: &str, ext: &str) -> (String, String) {
        (stem.to_string(), ext.to_string())
    }

    #[test]
    fn prefix_of_a_remote_path() {
        assert_eq!(as_prefix(""), "");
        assert_eq!(as_prefix("/"), "");
        assert_eq!(as_prefix("  "), "  /"); // spaces are a name
        assert_eq!(as_prefix("a/b"), "a/b/");
        assert_eq!(as_prefix("/a/b/"), "a/b/");
        assert_eq!(as_prefix("a//"), "a/");
    }

    #[test]
    fn copy_suffixes_do_not_pile_up() {
        assert_eq!(split_for_copy("report.pdf"), pair("report", ".pdf"));
        assert_eq!(split_for_copy("report copy.pdf"), pair("report", ".pdf"));
        assert_eq!(split_for_copy("report copy 7.pdf"), pair("report", ".pdf"));
        assert_eq!(split_for_copy("models copy 2"), pair("models", ""));
    }

    #[test]
    fn copy_like_words_stay() {
        assert_eq!(
            split_for_copy("my copy of it.txt"),
            pair("my copy of it", ".txt")
        );
        assert_eq!(
            split_for_copy("report copy x.pdf"),
            pair("report copy x", ".pdf")
        );
    }

    #[test]
    fn extension_is_the_last_dot_but_not_a_leading_one() {
        assert_eq!(split_for_copy(".bashrc"), pair(".bashrc", ""));
        assert_eq!(split_for_copy("a.tar.gz"), pair("a.tar", ".gz"));
    }

    #[test]
    fn nth_copy_names() {
        assert_eq!(nth_copy("report", ".pdf", 1), "report copy.pdf");
        assert_eq!(nth_copy("report", ".pdf", 2), "report copy 2.pdf");
        assert_eq!(nth_copy("models", "", 3), "models copy 3");
    }

    #[test]
    fn side_files_are_hidden_siblings() {
        assert_eq!(
            dot_sibling(Path::new("/d/movie.mp4"), ".bgul"),
            Path::new("/d/.movie.mp4.bgul")
        );
        assert_eq!(
            dot_sibling(Path::new("movie.mp4"), ".part"),
            Path::new(".movie.mp4.part")
        );
    }
}

#[cfg(test)]
mod nested_dir_tests {
    use super::nested_dir_prefixes;

    #[test]
    fn deepest_first_without_root() {
        let keys: Vec<String> = ["a/x", "a/b/c/.keep", "a/b/f.txt", "other/z"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(nested_dir_prefixes("a/", &keys), vec!["a/b/c/", "a/b/"]);
    }

    #[test]
    fn empty_subfolder_marker() {
        let keys = vec!["Tests/DoluKlasor-BosKlasor/hop/.keep".to_string()];
        assert_eq!(
            nested_dir_prefixes("Tests/DoluKlasor-BosKlasor/", &keys),
            vec!["Tests/DoluKlasor-BosKlasor/hop/"]
        );
    }
}

#[cfg(test)]
mod into_itself_tests {
    use super::local_is_within;
    use std::fs;

    #[test]
    fn local_nested_and_sibling_paths() {
        let d = std::env::temp_dir().join(format!("bgii-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let src = d.join("proj");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(d.join("proj copy")).unwrap();

        // The destination does not exist yet but lies under the source.
        assert!(local_is_within(&src, &src.join("sub").join("proj")));
        assert!(local_is_within(&src, &src));
        // Shared prefix, separate folders.
        assert!(!local_is_within(&src, &d.join("proj copy").join("proj")));
        assert!(!local_is_within(&src, &d.join("projX")));
        #[cfg(unix)]
        {
            // A destination that reaches into the source through a symlink.
            let link = d.join("alias");
            std::os::unix::fs::symlink(src.join("sub"), &link).unwrap();
            assert!(local_is_within(&src, &link.join("proj")));
        }
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod sent_files_tests {
    use super::remove_sent_files;
    use std::fs;

    fn touch(p: &std::path::Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn file_added_during_move_stays() {
        let d = std::env::temp_dir().join(format!("bgsent-a-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let root = d.join("T1Test");
        let big = root.join("big.bin");
        let sub = root.join("sub/b.txt");
        let added = root.join("taken.txt"); // dropped in while the transfer was running
        fs::create_dir_all(root.join("empty")).unwrap();
        for f in [&big, &sub, &added] {
            touch(f);
        }

        remove_sent_files(&root, &[big.clone(), sub.clone()]).unwrap();

        assert!(!big.exists() && !sub.exists(), "sent files are deleted");
        assert!(added.exists(), "a file added meanwhile stays");
        assert!(!root.join("sub").exists() && !root.join("empty").exists(), "emptied folders go");
        assert!(root.exists(), "a folder that still holds a file stays");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn everything_sent_removes_root() {
        let d = std::env::temp_dir().join(format!("bgsent-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let root = d.join("T1Test");
        let a = root.join("a.txt");
        let b = root.join("sub/b.txt");
        touch(&a);
        touch(&b);

        remove_sent_files(&root, &[a, b]).unwrap();

        assert!(!root.exists(), "the root goes when everything in it was sent");
        assert!(d.exists(), "nothing above the root is touched");
        let _ = fs::remove_dir_all(&d);
    }
}
