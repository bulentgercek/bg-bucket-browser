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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_sdk_s3::Client;
use aws_sdk_s3::config::interceptors::{
    BeforeTransmitInterceptorContextRef, FinalizerInterceptorContextRef,
};
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::{ConfigBag, Intercept, RuntimeComponents};
use aws_sdk_s3::error::{BoxError, DisplayErrorContext, ProvideErrorMetadata};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinSet;

use crate::config::{active_client, active_connection_id, client_by_id};
use crate::core::{PageGuard, TREE_MAX_ENTRIES, put_object_verified, s3_err, verify_written};
use crate::zip_writer::ZipOut;
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

/// What a remote copy reports when its source was replaced while it ran.
const ERR_SOURCE_CHANGED: &str =
    "changed on the volume while it was being copied; start the copy again";

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
            Ok(obj) => match crate::core::read_capped(obj.body, want).await {
                Ok(data) => {
                    if data.len() as u64 == want {
                        Ok(data)
                    } else {
                        Err(format!("{range} came back with {} bytes instead of {want}", data.len()))
                    }
                }
                Err(e) => Err(e),
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
    /// Connection the user started this job on; `None` for local-only jobs.
    conn_id: Option<String>,
    cancel: Arc<AtomicBool>,
    /// Local source files that really reached the destination; a Move deletes only these.
    sent_local: Mutex<Vec<Sent>>,
    /// Remote source keys that really reached the destination, `.keep` markers
    /// included, each with the ETag of the version that was read.
    sent_remote: Mutex<Vec<SentRemote>>,
    /// Remote keys a Move must leave where they are: folder markers that hold
    /// data and were not transferred. The folders above them stay too.
    keep_remote: Mutex<Vec<String>>,
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
    /// The connection of the job the worker is running, if any.
    current: Arc<Mutex<Option<String>>>,
}

impl TransferManager {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            counter: Arc::new(AtomicU64::new(1)),
            current: Arc::new(Mutex::new(None)),
        }
    }

    /// True when a job of connection `id` is waiting or running.
    // The queue lock is taken first, as the worker does when it moves a job from
    // the queue to `current`: a job is always seen in one of the two.
    fn busy_with(&self, id: &str) -> bool {
        let queue = self.queue.lock().unwrap();
        queue
            .iter()
            .any(|j| j.conn_id.as_deref() == Some(id) && !j.cancel.load(Ordering::Relaxed))
            || self.current.lock().unwrap().as_deref() == Some(id)
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

/// True when a transfer of connection `id` is waiting or running. Settings asks
/// before changing where that connection points.
pub fn connection_busy(app: &AppHandle, id: &str) -> bool {
    app.state::<TransferManager>().busy_with(id)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressEvt {
    id: String,
    bytes_done: u64,
    bytes_total: u64,
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
    /// The job's name as its queue row shows it; the error can arrive before
    /// the row does.
    name: String,
    detail: String,
}

/// A job's name as its queue row shows it: an archive carries its `.zip`.
fn shown_name(job: &Job) -> String {
    if job.kind == "downloadZip" {
        format!("{}.zip", job.name)
    } else {
        job.name.clone()
    }
}

/// What a finished folder job left out on purpose.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct NoteEvt {
    id: String,
    name: String,
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
    conn_id: Option<String>,
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
    // The connection is the one the user started the job on; the frontend
    // takes it before anything waits. Without one, the active connection now.
    // A local-only job uses none, and must not hold one busy.
    let conn_id = if matches!(kind.as_str(), "localCopy" | "localMove") {
        None
    } else {
        conn_id.or_else(|| active_connection_id(app.clone()))
    };
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
        keep_remote: Mutex::new(Vec::new()),
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
    conn_id: Option<String>,
) -> Result<String, String> {
    if srcs.is_empty() {
        return Err("no sources for zip".into());
    }
    let mgr = (*manager).clone();
    let id = mgr.next_id();
    let cancel = Arc::new(AtomicBool::new(false));
    mgr.cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    let conn_id = conn_id.or_else(|| active_connection_id(app.clone()));
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
        keep_remote: Mutex::new(Vec::new()),
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
    conn_id: Option<String>,
) -> Result<String, String> {
    let mgr = (*manager).clone();
    let id = mgr.next_id();
    let cancel = Arc::new(AtomicBool::new(false));
    mgr.cancels.lock().unwrap().insert(id.clone(), cancel.clone());
    let conn_id = conn_id.or_else(|| active_connection_id(app.clone()));
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
        keep_remote: Mutex::new(Vec::new()),
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
        // The running job's connection stays marked until its iteration ends,
        // even if it panics.
        struct Current(Arc<Mutex<Option<String>>>);
        impl Drop for Current {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = None;
            }
        }
        loop {
            // Taken from the queue and marked as current under one lock, so
            // Settings never sees the job in neither place.
            let job = {
                let mut queue = mgr.queue.lock().unwrap();
                let job = queue.pop_front();
                *mgr.current.lock().unwrap() = job.as_ref().and_then(|j| j.conn_id.clone());
                job
            };
            let _current = Current(mgr.current.clone());
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
                                name: shown_name(&job),
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
                    let _ = app.emit("transfer-error", ErrEvt { id, name: shown_name(&job), detail });
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

/// Marks a downloaded file as coming from the network, the way a browser does,
/// so the system's own checks (SmartScreen, Gatekeeper, Office's protected view)
/// apply when it is opened. Linux has no such mark. A filesystem without the
/// feature (FAT, exFAT) keeps the file unmarked; on macOS no `._` side file is
/// written in its place, on a network share without attributes either.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(unused_variables))]
fn mark_from_network(path: &Path) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        if let Err(e) = fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n") {
            crate::devlog::verbose("transfer", format!("network mark not set on {}: {e}", path.display()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return;
        };
        // A volume that cannot hold extended attributes itself (FAT, exFAT, a
        // share without them) would keep the mark in a `._` side file; there
        // the file stays unmarked rather than gaining one.
        if !keeps_attributes_itself(&c_path) {
            crate::devlog::verbose(
                "transfer",
                format!("network mark not set on {}: the volume keeps no attributes of its own", path.display()),
            );
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let value = format!("0081;{secs:08x};BG Bucket Browser;");
        // SAFETY: both names are NUL-terminated C strings that outlive the call,
        // and the value is passed as a byte buffer with its length.
        let rc = unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            crate::devlog::verbose("transfer", format!("network mark not set on {}: {e}", path.display()));
        }
    }
}

/// Whether the volume `path` is on says it stores extended attributes with
/// the file, rather than in `._` side files. A volume that does not say, or
/// cannot be asked, is taken not to.
#[cfg(target_os = "macos")]
fn keeps_attributes_itself(path: &std::ffi::CStr) -> bool {
    // SAFETY: statfs fills the zeroed struct it is given; the path is a
    // NUL-terminated C string that outlives the call.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut st) } != 0 {
        return false;
    }
    // Volume attributes are asked of the volume's root.
    #[repr(C)]
    struct Answer {
        length: u32,
        caps: libc::vol_capabilities_attr_t,
    }
    let mut ask = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // SAFETY: the answer is plain integers, so zeroed is a valid value; the
    // mount point from statfs is NUL-terminated, and the buffer and its size
    // describe the same struct.
    let mut answer: Answer = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::getattrlist(
            st.f_mntonname.as_ptr(),
            (&mut ask as *mut libc::attrlist).cast(),
            (&mut answer as *mut Answer).cast(),
            std::mem::size_of::<Answer>(),
            0,
        )
    };
    if rc != 0 {
        return false;
    }
    let i = libc::VOL_CAPABILITIES_INTERFACES;
    let bit = libc::VOL_CAP_INT_EXTENDED_ATTR;
    answer.caps.valid[i] & bit != 0 && answer.caps.capabilities[i] & bit != 0
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

    let cache_path = join_name(
        &opened_dir(app)?.join(opened_hash(&bucket, &key, &etag)),
        &job.name,
    )?;

    // Already in the cache with the right size: open it without downloading.
    if let Ok(meta) = fs::metadata(&cache_path)
        && meta.is_file()
        && meta.len() == size
    {
        emit_progress(app, &job.id, size, size);
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
    .0
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

/// Deletes one remote source object of a finished Move, only while it is still
/// the version that was sent. `Ok(false)` when it changed since and was kept.
async fn delete_sent_object(
    client: &Client,
    bucket: &str,
    key: &str,
    etag: Option<&str>,
) -> Result<bool, String> {
    let version = etag.filter(|e| !e.is_empty()).map(str::to_string);
    match client.delete_object().bucket(bucket).key(key).set_if_match(version).send().await {
        Ok(_) => Ok(true),
        // The gateway answers a key that is no longer there with 412 too, which is
        // also how a retried delete whose first answer was lost ends.
        Err(e) if e.raw_response().is_some_and(|r| r.status().as_u16() == 412) => {
            Ok(client.head_object().bucket(bucket).key(key).send().await.is_err_and(|e| {
                e.raw_response().is_some_and(|r| r.status().as_u16() == 404)
            }))
        }
        Err(e) => Err(s3_err(&e)),
    }
}

/// Deletes the remote sources of a finished Move: each object only while it is
/// the version that was sent, then the folder markers that emptied. A folder
/// keeps its markers above anything left behind.
async fn delete_remote_sources(
    client: &Client,
    bucket: &str,
    src: &str,
    is_dir: bool,
    sent: &[SentRemote],
    keep: &[String],
) -> Result<(), String> {
    if !is_dir {
        let key = remote_key(src);
        let etag = sent.iter().find(|(k, _)| *k == key).and_then(|(_, e)| e.as_deref());
        if !delete_sent_object(client, bucket, &key, etag).await? {
            return Err(format!("{key}: changed after it was sent, so it was kept"));
        }
        return Ok(());
    }
    let prefix = as_prefix(src);
    let (mut kept, mut failed) = (Vec::new(), 0usize);
    for (k, etag) in sent {
        match delete_sent_object(client, bucket, k, etag.as_deref()).await {
            Ok(true) => {}
            Ok(false) => kept.push(k.strip_prefix(&prefix).unwrap_or(k).to_string()),
            Err(_) => failed += 1,
        }
    }
    let keys: Vec<String> = sent.iter().map(|(k, _)| k.clone()).collect();
    // What stays: objects that changed or failed, and markers never transferred.
    let mut left: Vec<String> = kept.iter().map(|rel| format!("{prefix}{rel}")).collect();
    left.extend(keep.iter().cloned());
    // Subfolders go from the deepest outwards, because the gateway only removes an empty one.
    for dir in nested_dir_prefixes(&prefix, &keys) {
        if !left.iter().any(|k| k.starts_with(&dir)) {
            let _ = client.delete_object().bucket(bucket).key(&dir).send().await;
        }
    }
    if left.is_empty() && failed == 0 {
        let _ = client.delete_object().bucket(bucket).key(&prefix).send().await;
    }
    let mut problems = Vec::new();
    if !kept.is_empty() {
        problems.push(format!("changed after they were sent and were kept: {}", some_names(&kept)));
    }
    if failed > 0 {
        problems.push(format!("{failed} object(s) could not be removed"));
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Deletes the source of a finished Move.
async fn delete_source(app: &AppHandle, job: &Job) -> Result<(), String> {
    match job.kind.as_str() {
        "download" | "remoteCopy" | "remoteMove" => {
            let (client, bucket) = s3_job(app, &job.conn_id)?;
            // Only what was really transferred is deleted; the source is never re-listed.
            let sent = job.sent_remote.lock().unwrap().clone();
            let keep = job.keep_remote.lock().unwrap().clone();
            return delete_remote_sources(&client, &bucket, &job.src, job.is_dir, &sent, &keep).await;
        }
        "upload" | "localCopy" | "localMove" => {
            let p = resolve_local(&job.src)?;
            let sent = job.sent_local.lock().unwrap().clone();
            return remove_local_source(&p, job.is_dir, &sent);
        }
        _ => {}
    }
    Ok(())
}

/// Deletes the local source of a finished Move.
fn remove_local_source(p: &Path, is_dir: bool, sent: &[Sent]) -> Result<(), String> {
    // A folder chosen through a symbolic link was sent with its content; the
    // move takes the link away and leaves the folder it points to as it was.
    if is_dir && is_link(p) {
        return remove_link(p).map_err(|e| e.to_string());
    }
    if is_dir {
        // Same rule locally: delete the sent files, not the whole tree.
        return remove_sent_files(p, sent);
    }
    // Only a file on record as sent is deleted: a rename already took it away,
    // and whatever holds its name now arrived afterwards.
    let Some((_, stamp)) = sent.iter().find(|(f, _)| f == p) else {
        return Ok(());
    };
    // A file saved again since it was read is not the one that was sent.
    if let Some(stamp) = stamp
        && fs::metadata(p).is_ok_and(|m| source_stamp(&m) != *stamp)
    {
        return Err(format!("{}: changed after it was sent, so it was kept", p.display()));
    }
    match fs::remove_file(p) {
        Ok(()) => {
            let _ = fs::remove_file(sidecar_of(p));
        }
        // A local move may already have renamed the file away; nothing left to delete.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

/// Removes a symbolic link standing where a side file is about to be written,
/// so the write makes a new file instead of going through the link.
fn clear_link(path: &Path) -> std::io::Result<()> {
    if is_link(path) {
        return remove_link(path);
    }
    Ok(())
}

/// True when `p` itself is a symbolic link (on Windows also a junction).
fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// Deletes a symbolic link itself, never what it points to.
fn remove_link(p: &Path) -> std::io::Result<()> {
    // Windows removes a link to a folder, and a junction, as a directory.
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if fs::symlink_metadata(p)?.file_type().is_symlink_dir() {
            return fs::remove_dir(p);
        }
    }
    fs::remove_file(p)
}

/// Writes a small side file (a resume record), never through a link.
fn write_side(path: &Path, contents: &str) {
    if clear_link(path).is_ok() {
        let _ = fs::write(path, contents);
    }
}

/// Creates a side file (a `.part`) for writing, never through a link.
fn create_side(path: &Path) -> std::io::Result<fs::File> {
    clear_link(path)?;
    fs::File::create(path)
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
fn remove_sent_files(root: &std::path::Path, sent: &[Sent]) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    let (mut failed, mut changed) = (0usize, 0usize);
    for (f, stamp) in sent {
        // A file saved again since it was read is not the one that was sent.
        if let Some(stamp) = stamp {
            match fs::metadata(f) {
                Ok(m) if source_stamp(&m) != *stamp => {
                    changed += 1;
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                _ => {}
            }
        }
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
                // A folder the walk left out for its name was not sent, so it stays.
                if e.file_name().to_str().is_none() {
                    continue;
                }
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
    let mut problems = Vec::new();
    if changed > 0 {
        problems.push(format!("{changed} file(s) changed after they were sent and were kept"));
    }
    if failed > 0 {
        problems.push(format!("{failed} file(s) could not be removed"));
    }
    if !problems.is_empty() {
        return Err(problems.join("; "));
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

/// Why a download refuses a name: it cannot be one file or folder name here.
const UNSAFE_NAME: &str = "not transferred: the name is not a safe file name on this system";

/// True when `name` can be exactly one file or folder name: not empty, not `.`
/// or `..`, no `/`. With Windows rules it also carries none of the forms that
/// change what a path points at (`\`, a drive or stream `:`, a device name, a
/// name Windows trims to nothing) and no character Windows cannot store.
fn is_local_name(name: &str, windows: bool) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return false;
    }
    if !windows {
        return true;
    }
    if name.chars().any(|c| c < ' ' || "\\:<>\"|?*".contains(c)) {
        return false;
    }
    // Windows drops trailing dots and spaces, so a name made only of them
    // names the folder itself.
    if name.trim_end_matches(['.', ' ']).is_empty() {
        return false;
    }
    // Device names are reserved with any extension: `con.txt` is the console.
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let port_number = |s: &str| {
        matches!(s, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}")
    };
    let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|d| stem.eq_ignore_ascii_case(d))
        || ["COM", "LPT"].iter().any(|port| {
            stem.get(..3).is_some_and(|head| head.eq_ignore_ascii_case(port))
                && stem.get(3..).is_some_and(port_number)
        });
    !device
}

/// The same check with the rules of the system the app runs on.
fn local_name_ok(name: &str) -> bool {
    is_local_name(name, cfg!(windows))
}

/// Joins one remote-derived name onto a local folder, refusing a name that is
/// not a single safe name here.
fn join_name(root: &Path, name: &str) -> Result<PathBuf, String> {
    if !local_name_ok(name) {
        return Err(UNSAFE_NAME.into());
    }
    Ok(root.join(name))
}

/// The parts of a remote-relative path, each checked with `is_local_name`, or
/// `None` when one is unsafe. Empty and `.` parts are dropped as a path reader
/// would; a leading `/` is refused.
fn rel_parts(rel: &str, windows: bool) -> Option<Vec<&str>> {
    if rel.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    (!parts.is_empty() && parts.iter().all(|p| is_local_name(p, windows))).then_some(parts)
}

/// Joins a remote-relative path onto a local root, refusing one that would land
/// outside it. Each part is pushed on its own, so no part can replace the root.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let parts = rel_parts(rel, cfg!(windows))
        .ok_or_else(|| format!("unsafe path in object name: {rel}"))?;
    let mut out = root.to_path_buf();
    out.extend(parts);
    Ok(out)
}

/// True when `part` can be one part of a zip member name wherever the archive
/// is extracted: an ordinary name, no `\`, which a Windows extractor reads as
/// a separator, and no drive prefix such as `C:`.
fn zip_part_ok(part: &str) -> bool {
    let drive = part.as_bytes().get(1) == Some(&b':') && part.as_bytes()[0].is_ascii_alphabetic();
    is_local_name(part, false) && !part.contains('\\') && !drive
}

/// The path of one member inside a zip archive, built from checked parts.
/// Empty and `.` parts are dropped as a path reader would.
fn zip_member(top: &str, rel: &str) -> Result<String, String> {
    let unsafe_path = || {
        let shown = if top.is_empty() { rel.to_string() } else { format!("{top}/{rel}") };
        format!("unsafe path for a zip entry: {shown}")
    };
    if rel.starts_with('/') {
        return Err(unsafe_path());
    }
    let rel_parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if rel_parts.is_empty() {
        return Err(unsafe_path());
    }
    let parts: Vec<&str> = (!top.is_empty()).then_some(top).into_iter().chain(rel_parts).collect();
    if !parts.iter().all(|p| zip_part_ok(p)) {
        return Err(unsafe_path());
    }
    Ok(parts.join("/"))
}

/// What one listed key of a folder download becomes locally.
enum FolderItem<'a> {
    /// A folder to create, from a `.keep` marker or a key ending in `/`; `""`
    /// is the downloaded folder itself.
    Dir(&'a str),
    /// A file at this path, relative to the downloaded folder.
    File(&'a str),
    /// A folder marker that holds data: it cannot arrive as a folder, and is
    /// left where it is.
    Held(&'a str),
}

/// Sorts one key listed under `prefix` by its name and size; `None` for the
/// prefix's own marker. Only an empty marker stands for a folder: one that
/// holds data would be lost if it arrived as a folder, and a Move would then
/// delete the data that never arrived.
fn folder_item<'a>(prefix: &str, key: &'a str, size: u64) -> Option<FolderItem<'a>> {
    let rel = key.strip_prefix(prefix).unwrap_or(key);
    if rel.is_empty() {
        return None;
    }
    // A `.keep` marker stands for an empty remote folder and becomes a real directory here.
    if key.ends_with("/.keep") && size == 0 {
        return Some(FolderItem::Dir(rel.rsplit_once('/').map_or("", |(dir, _)| dir)));
    }
    // Some servers keep a folder as an object whose key ends in `/`.
    if let Some(dir) = rel.strip_suffix('/') {
        return Some(if size == 0 { FolderItem::Dir(dir) } else { FolderItem::Held(rel) });
    }
    Some(FolderItem::File(rel))
}

/// Progress context of a folder job: bytes already done and the folder total.
type Agg = Option<(u64, u64)>;

async fn run_download(app: &AppHandle, job: &Job) -> Result<Outcome, String> {
    let (client, bucket) = s3_job(app, &job.conn_id)?;
    let dest_root = resolve_local(&job.dest_dir)?;
    // One set of stream clients for the whole job: a folder reuses their
    // connections from file to file.
    let clients = stream_clients(&client);

    if !job.is_dir {
        let key = remote_key(&job.src);
        let out = join_name(&dest_root, &job.name)?;
        let (outcome, etag) = download_one(
            app, &clients, &bucket, &key, &out, &job.id, &job.cancel, None, &job.conflict,
        )
        .await?;
        if matches!(outcome, Outcome::Done) {
            job.sent_remote.lock().unwrap().push((key, etag));
        }
        return Ok(outcome);
    }

    // A folder job downloads every object under the prefix and rebuilds the tree locally.
    let prefix = as_prefix(&job.src);
    let folder_root = join_name(&dest_root, &job.name)?;
    let Some(objects) = list_objects_all(&client, &bucket, &prefix, &job.cancel).await? else {
        return Ok(Outcome::Canceled);
    };
    // Every path is checked before anything is written, so a refused folder
    // leaves nothing half-downloaded behind.
    for (key, sz, _) in &objects {
        match folder_item(&prefix, key, *sz) {
            Some(FolderItem::Dir(dir)) if !dir.is_empty() => {
                safe_join(&folder_root, dir)?;
            }
            Some(FolderItem::File(rel)) => {
                safe_join(&folder_root, rel)?;
            }
            _ => {}
        }
    }
    fs::create_dir_all(&folder_root).map_err(|e| e.to_string())?; // an empty folder still arrives
    // Only what comes down counts: folder markers do not, a marker that holds
    // data and so arrives as a file does.
    let total: u64 = objects
        .iter()
        .filter(|(k, sz, _)| matches!(folder_item(&prefix, k, *sz), Some(FolderItem::File(_))))
        .map(|(_, sz, _)| *sz)
        .sum();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total);

    let mut left = LeftOut::default();
    // The folder's own marker never arrives; one that holds data must stay.
    if objects.iter().any(|(k, sz, _)| *k == prefix && *sz > 0) {
        job.keep_remote.lock().unwrap().push(prefix.clone());
    }
    for (key, sz, listed_etag) in &objects {
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        let rel = match folder_item(&prefix, key, *sz) {
            None => continue,
            Some(FolderItem::Held(rel)) => {
                left.held.push(rel.to_string());
                job.keep_remote.lock().unwrap().push(key.clone());
                continue;
            }
            Some(FolderItem::Dir(dir)) => {
                if !dir.is_empty() {
                    fs::create_dir_all(safe_join(&folder_root, dir)?).map_err(|e| e.to_string())?;
                }
                job.sent_remote.lock().unwrap().push((key.clone(), Some(listed_etag.clone())));
                continue;
            }
            Some(FolderItem::File(rel)) => rel,
        };
        let out = safe_join(&folder_root, rel)?;
        // A skipped file was not transferred, so a Move must leave it in place.
        if job.conflict == "skip" && out.exists() {
            done += *sz;
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match download_one(
            app, &clients, &bucket, key, &out, &job.id, &job.cancel,
            Some((done, total)), &job.conflict,
        )
        .await
        {
            Ok((Outcome::Canceled, _)) => return Ok(Outcome::Canceled),
            Ok((Outcome::Done, etag)) => job.sent_remote.lock().unwrap().push((key.clone(), etag)),
            Ok((Outcome::Skipped(_), _)) => {}
            // One object written again while it came down is left out and named;
            // the rest of the folder still arrives.
            Err(e) if e == ERR_CHANGED => left.changed.push(rel.to_string()),
            Err(e) => return Err(e),
        }
        done += *sz;
    }
    emit_progress(app, &job.id, total, total);
    emit_skipped(app, &job.id, &job.name, &left);
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

/// Downloads one object into `<out>.part` in parallel chunks and renames it when
/// complete. A finished download comes back with the ETag of the version read.
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
) -> Result<(Outcome, Option<String>), String> {
    // Name already taken: skip returns, rename picks a free name, overwrite keeps going.
    let target: PathBuf = if out_path.exists() {
        match conflict {
            "skip" => return Ok((Outcome::Skipped(SKIP_TAKEN.into()), None)),
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
    clear_link(&part_path).map_err(|e| e.to_string())?;
    clear_link(&meta_path).map_err(|e| e.to_string())?;

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
        write_side(&meta_path, &serde_json::to_string(&rec).unwrap_or_default());
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

    let start_bytes = watermark * DOWNLOAD_CHUNK;
    let fetched = Arc::new(AtomicU64::new(start_bytes));
    let (base, grand_total) = agg.unwrap_or((0, total));
    let emit = |done: u64| emit_progress(app, id, base + done, grand_total);
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
                    return Ok((Outcome::Canceled, None)); // `.part` and its sidecar stay for a resume
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
    // Marked before the rename, which carries the mark along: on Windows a final
    // name ending in a dot or a space is trimmed by the rename, and a mark put on
    // the untrimmed name afterwards would land on another, empty file.
    mark_from_network(&part_path);
    fs::rename(&part_path, out_path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&meta_path);
    Ok((Outcome::Done, Some(etag)))
}

/// Every object under a prefix, paged, as (key, size, etag).
async fn list_objects_all(
    client: &Client,
    bucket: &str,
    prefix: &str,
    cancel: &AtomicBool,
) -> Result<Option<Vec<(String, u64, String)>>, String> {
    let mut out = Vec::new();
    let mut guard = PageGuard::new(TREE_MAX_ENTRIES);
    // The SDK's paginator follows the continuation token and stops when it
    // repeats; it never looks at `is_truncated`.
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    loop {
        // A cancel does not wait for the page on its way: a page can take seconds.
        let page = tokio::select! {
            biased;
            () = canceled(cancel) => return Ok(None),
            page = pages.next() => page,
        };
        let Some(page) = page else { break };
        let resp = page.map_err(|e| s3_err(&e))?;
        guard
            .page(resp.contents().len(), resp.next_continuation_token())
            .map_err(|stop| stop.to_string())?;
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
    Ok(Some(out))
}

/// Resolves once the job is canceled, looking every 200 ms.
async fn canceled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
    let final_path = join_name(&dest_root, &zip_name)?;
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
            let Some(listed) = list_objects_all(&client, &bucket, &prefix, &job.cancel).await? else {
                return Ok(Outcome::Canceled);
            };
            for (key, sz, etag) in listed {
                // Folder markers carry no data; a folder is in the archive through its files.
                if key.ends_with("/.keep") || key.ends_with('/') {
                    continue;
                }
                let rel = key.strip_prefix(&prefix).unwrap_or(&key);
                if rel.is_empty() {
                    continue;
                }
                let arc = zip_member(&top, rel)?;
                members.push((key, arc, sz, etag));
            }
        } else {
            let key = remote_key(&zs.path);
            let name = zip_member("", key.rsplit('/').next().unwrap_or(&key))?;
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

    // A zip is resumable: an archive this run's writer left keeps the members
    // whose name, size and ETag still match, and the run goes on after them.
    // Anything else in its place is thrown away and the archive starts over.
    let _ = fs::remove_file(dot_sibling(&final_path, ".ckpt")); // left by 1.1 and earlier
    clear_link(&out_path).map_err(|e| e.to_string())?;
    let wanted: HashMap<&str, (u64, &str)> = members
        .iter()
        .map(|(_, arc, sz, etag)| (arc.as_str(), (*sz, etag.as_str())))
        .collect();
    let resumed = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&out_path)
        .ok()
        .and_then(|f| ZipOut::resume(f, |name, size, etag| wanted.get(name) == Some(&(size, etag))));
    let mut out = match resumed {
        Some(out) => out,
        None => ZipOut::create(create_side(&out_path).map_err(|e| e.to_string())?),
    };
    let done: std::collections::HashSet<String> = out.names().map(str::to_string).collect();

    // On a resume the already archived members count as progress from the start.
    let mut written: u64 = members
        .iter()
        .filter(|(_, arc, _, _)| done.contains(arc))
        .map(|(_, _, sz, _)| *sz)
        .sum();
    let mut last_emit = Instant::now() - EMIT_EVERY;
    emit_progress(app, &job.id, written, grand_total);

    for (key, arc_name, sz, etag) in &members {
        if done.contains(arc_name) {
            continue; // this member is already in the archive
        }
        if job.cancel.load(Ordering::Relaxed) {
            // Every whole member stays, for a resume.
            out.finish().map_err(|e| e.to_string())?;
            return Ok(Outcome::Canceled);
        }
        out.start(arc_name, etag).map_err(|e| e.to_string())?;

        let total = *sz;
        let mut off: u64 = 0;
        while off < total {
            if job.cancel.load(Ordering::Relaxed) {
                return Ok(Outcome::Canceled); // dropping `out` cuts the half member away
            }
            let end = (off + CHUNK).min(total) - 1;
            let etag = (!etag.is_empty()).then_some(etag.as_str());
            let data =
                get_range_retry(app, &job.id, &client, &bucket, key, off, end, etag, &job.cancel)
                    .await?;
            out.write(&data).map_err(|e| e.to_string())?;
            off += data.len() as u64;
            written += data.len() as u64;
            if last_emit.elapsed() >= EMIT_EVERY {
                emit_progress(app, &job.id, written, grand_total);
                last_emit = Instant::now();
            }
        }
        out.end_member().map_err(|e| e.to_string())?;
    }
    drop(out.finish().map_err(|e| e.to_string())?);

    // A name taken in the meantime, or from the start, gets the next " copy" name.
    let name = free_local_name(&dest_root, &zip_name);
    mark_from_network(&out_path); // before the rename, as in `download_one`
    fs::rename(&out_path, dest_root.join(&name)).map_err(|e| e.to_string())?;
    emit_progress(app, &job.id, grand_total, grand_total);
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
    // A source chosen through a link is copied instead, so its content arrives on
    // every disk alike and only the link is removed afterwards.
    if is_move && !is_link(&src) && !dest.exists() && fs::rename(&src, &dest).is_ok() {
        emit_progress(app, &job.id, 1, 1);
        return Ok(Outcome::Done);
    }

    if job.is_dir {
        fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let Some(walk) = walk_files(&src, &job.cancel)? else {
            return Ok(Outcome::Canceled);
        };
        let (files, empty_dirs) = (&walk.files, &walk.empty_dirs);
        // Empty subfolders are created explicitly; walking only files would lose them.
        for d in empty_dirs {
            fs::create_dir_all(dest.join(d)).map_err(|e| e.to_string())?;
        }
        let total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
        let mut done: u64 = 0;
        let mut changed: Vec<String> = Vec::new();
        emit_progress(app, &job.id, 0, total);
        for (path, sz, rel) in files {
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
                Some((done, total)),
                &job.conflict,
            )? {
                (Outcome::Canceled, _) => return Ok(Outcome::Canceled),
                (Outcome::Done, stamp) => job.sent_local.lock().unwrap().push((path.clone(), stamp)),
                // A file that changed while it was read stays out and is named in the note.
                (Outcome::Skipped(why), _) if why == CHANGED => changed.push(rel.clone()),
                (Outcome::Skipped(_), _) => {}
            }
            done += *sz;
        }
        emit_progress(app, &job.id, total, total);
        emit_skipped(app, &job.id, &job.name, &LeftOut { changed, ..LeftOut::of_walk(&walk) });
        Ok(Outcome::Done)
    } else {
        let total = fs::metadata(&src).map_err(|e| e.to_string())?.len();
        emit_progress(app, &job.id, 0, total);
        let (outcome, stamp) =
            copy_file_chunked(app, &job.id, &job.cancel, &src, &dest, None, &job.conflict)?;
        if matches!(outcome, Outcome::Done) {
            job.sent_local.lock().unwrap().push((src.clone(), stamp));
        }
        Ok(outcome)
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
) -> Result<(Outcome, Option<SourceStamp>), String> {
    use std::io::Read;

    let target: PathBuf = if dst.exists() {
        match conflict {
            "skip" => return Ok((Outcome::Skipped(SKIP_TAKEN.into()), None)),
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
    // The stamp comes from the open file, before and after the copy: a file
    // edited meanwhile is not taken, and a Move later deletes only this version.
    let before = source_stamp(&reader.metadata().map_err(|e| e.to_string())?);
    let mut writer = create_side(&tmp).map_err(|e| e.to_string())?;

    let total = before.size;
    let (base, grand_total) = agg.unwrap_or((0, total));
    let mut done: u64 = 0;
    let mut last_emit = Instant::now() - EMIT_EVERY;
    let mut buf = vec![0u8; CHUNK as usize];

    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = fs::remove_file(&tmp);
            return Ok((Outcome::Canceled, None));
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last_emit.elapsed() >= EMIT_EVERY {
            emit_progress(app, id, base + done, grand_total);
            last_emit = Instant::now();
        }
    }

    writer.sync_data().ok();
    drop(writer);
    let after = reader.metadata().map(|m| source_stamp(&m)).map_err(|e| e.to_string())?;
    if after != before || done != before.size {
        let _ = fs::remove_file(&tmp);
        return Ok((Outcome::Skipped(CHANGED.into()), None));
    }
    fs::rename(&tmp, dst).map_err(|e| e.to_string())?;
    Ok((Outcome::Done, Some(before)))
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
        let (outcome, etag) = remote_copy_stream(
            app, &client, &bucket, &src_key, &dest_key, &job.id, None, &job.cancel,
        )
        .await
        .map_err(|e| if e == ERR_SOURCE_CHANGED { format!("{src_key}: {e}") } else { e })?;
        if matches!(outcome, Outcome::Done) {
            job.sent_remote.lock().unwrap().push((src_key, etag));
        }
        return Ok(outcome);
    }

    // A folder copies every object under the prefix, `.keep` markers included.
    let src_prefix = as_prefix(&job.src);
    let dest_prefix = format!("{}{}/", as_prefix(&job.dest_dir), name);
    // Copying into itself would write into the source tree, and a Move would then delete both.
    if dest_prefix.starts_with(&src_prefix) {
        return Err(ERR_INTO_ITSELF.into());
    }
    let Some(objects) = list_objects_all(&client, &bucket, &src_prefix, &job.cancel).await? else {
        return Ok(Outcome::Canceled);
    };
    let total: u64 = objects.iter().map(|(_, sz, _)| *sz).sum();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total);

    // With `skip`, an object already at the destination is left as it is and its
    // source is not counted as sent, the same as a folder download does.
    let taken: std::collections::HashSet<String> = if job.conflict == "skip" {
        let Some(listed) = list_objects_all(&client, &bucket, &dest_prefix, &job.cancel).await? else {
            return Ok(Outcome::Canceled);
        };
        listed.into_iter().map(|(k, _, _)| k).collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut changed: Vec<String> = Vec::new();
    for (key, sz, _) in &objects {
        let rel = key.strip_prefix(&src_prefix).unwrap_or(key);
        let dest_key = format!("{dest_prefix}{rel}");
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        if rel.is_empty() {
            // The folder's own marker is not copied; one that holds data must stay.
            if *sz > 0 {
                job.keep_remote.lock().unwrap().push(key.clone());
            }
            continue;
        }
        if taken.contains(&dest_key) {
            done += *sz;
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match remote_copy_stream(
            app, &client, &bucket, key, &dest_key, &job.id,
            Some((done, total)), &job.cancel,
        )
        .await
        {
            Ok((Outcome::Canceled, _)) => return Ok(Outcome::Canceled),
            Ok((Outcome::Done, etag)) => job.sent_remote.lock().unwrap().push((key.clone(), etag)),
            Ok((Outcome::Skipped(_), _)) => {}
            // One object written again meanwhile is left out and named; the rest still goes.
            Err(e) if e == ERR_SOURCE_CHANGED => changed.push(rel.to_string()),
            Err(e) => return Err(e),
        }
        done += *sz;
    }
    emit_progress(app, &job.id, total, total);
    emit_skipped(app, &job.id, &job.name, &LeftOut { changed, ..Default::default() });
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

/// Server-side copy of one object, bound to the version with `etag` and
/// checked by size afterwards. A source replaced meanwhile fails with
/// `ERR_SOURCE_CHANGED`.
async fn copy_object_checked(
    client: &Client,
    bucket: &str,
    src_key: &str,
    dest_key: &str,
    total: u64,
    etag: &str,
) -> Result<(), String> {
    // The source goes unencoded: the gateway looks the header up as written.
    let version = (!etag.is_empty()).then(|| etag.to_string());
    match client
        .copy_object()
        .bucket(bucket)
        .key(dest_key)
        .copy_source(format!("{bucket}/{src_key}"))
        .set_copy_source_if_match(version)
        .send()
        .await
    {
        Ok(o) => {
            let written = o.copy_object_result().and_then(|r| r.e_tag());
            verify_written(client, bucket, dest_key, total, written).await
        }
        Err(e) if e.raw_response().is_some_and(|r| r.status().as_u16() == 412) => {
            Err(ERR_SOURCE_CHANGED.into())
        }
        Err(e) => Err(s3_err(&e)),
    }
}

/// The server-side copy, tried once more when the source changed between the
/// HEAD and the copy: the copy is atomic, so the new version arrives whole, and
/// its ETag is what a Move later deletes. Returns that ETag.
async fn server_copy(
    client: &Client,
    bucket: &str,
    src_key: &str,
    dest_key: &str,
    total: u64,
    etag: &str,
) -> Result<String, String> {
    match copy_object_checked(client, bucket, src_key, dest_key, total, etag).await {
        Err(e) if e == ERR_SOURCE_CHANGED => {}
        other => return other.map(|()| etag.to_string()),
    }
    let head = client
        .head_object()
        .bucket(bucket)
        .key(src_key)
        .send()
        .await
        .map_err(|e| format!("HEAD {src_key}: {}", s3_err(&e)))?;
    let (total, etag) = (
        head.content_length().unwrap_or(0).max(0) as u64,
        head.e_tag().unwrap_or_default().to_string(),
    );
    if total > COPY_OBJECT_MAX {
        return Err(ERR_SOURCE_CHANGED.into());
    }
    copy_object_checked(client, bucket, src_key, dest_key, total, &etag).await.map(|()| etag)
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
) -> Result<(Outcome, Option<String>), String> {
    let head = retry3(|| client.head_object().bucket(bucket).key(src_key).send())
        .await
        .map_err(|e| format!("HEAD {src_key}: {}", s3_err(&e)))?;
    let total = head.content_length().unwrap_or(0).max(0) as u64;
    // Everything below reads this version: the copy and every range are bound
    // to it, so a source replaced meanwhile fails instead of arriving mixed.
    let etag = head.e_tag().unwrap_or_default().to_string();
    let version = (!etag.is_empty()).then_some(etag.as_str());
    let (base, grand_total) = agg.unwrap_or((0, total));

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
        emit_progress(app, id, base, grand_total);
        return Ok((Outcome::Done, Some(etag.clone())));
    }

    if total <= COPY_OBJECT_MAX {
        if cancel.load(Ordering::Relaxed) {
            return Ok((Outcome::Canceled, None));
        }
        match server_copy(client, bucket, src_key, dest_key, total, &etag).await {
            Ok(copied) => {
                emit_progress(app, id, base + total, grand_total);
                return Ok((Outcome::Done, Some(copied)));
            }
            // Reading it through the app would only copy the new version.
            Err(e) if e == ERR_SOURCE_CHANGED => return Err(e),
            Err(e) => crate::devlog::verbose(
                "transfer",
                format!(
                    "CopyObject {src_key:?} -> {dest_key:?} failed ({e}), copying through the app"
                ),
            ),
        }
    }

    if total <= CHUNK {
        let data = get_range_retry(app, id, client, bucket, src_key, 0, total - 1, version, cancel)
            .await
            .map_err(|e| if e == ERR_CHANGED { ERR_SOURCE_CHANGED.to_string() } else { e })?;
        put_object_verified(client, bucket, dest_key, &data).await?;
        emit_progress(app, id, base + total, grand_total);
        return Ok((Outcome::Done, Some(etag.clone())));
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
    let mut last_emit = Instant::now() - EMIT_EVERY;

    // A part that fails leaves nothing half-made on the server.
    let abort = || async {
        let _ = client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(dest_key)
            .upload_id(&upload_id)
            .send()
            .await;
    };
    for pn in 1..=num_parts {
        if cancel.load(Ordering::Relaxed) {
            abort().await;
            return Ok((Outcome::Canceled, None));
        }
        let off = (pn as u64 - 1) * part_size;
        let end = (off + part_size).min(total) - 1;
        // The same read as a download's: bound to the version, retried, and
        // refused unless exactly the range comes back.
        let data = match get_range_retry(app, id, client, bucket, src_key, off, end, version, cancel).await {
            Ok(d) => d,
            Err(e) => {
                abort().await;
                return Err(if e == ERR_CHANGED { ERR_SOURCE_CHANGED.to_string() } else { e });
            }
        };
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
        .await;
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                abort().await;
                return Err(format!("UploadPart {dest_key} #{pn}: {}", s3_err(&e)));
            }
        };
        parts.push(
            CompletedPart::builder()
                .part_number(pn)
                .set_e_tag(out.e_tag().map(str::to_string))
                .build(),
        );
        sent += n;
        if last_emit.elapsed() >= EMIT_EVERY {
            emit_progress(app, id, base + sent, grand_total);
            last_emit = Instant::now();
        }
    }

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    let done = match retry3(|| {
        client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(dest_key)
            .upload_id(&upload_id)
            .multipart_upload(completed.clone())
            .send()
    })
    .await
    {
        Ok(done) => done,
        Err(e) => {
            abort().await;
            return Err(format!("CompleteMultipartUpload {dest_key}: {}", s3_err(&e)));
        }
    };
    verify_written(client, bucket, dest_key, total, done.e_tag()).await?;
    Ok((Outcome::Done, Some(etag.clone())))
}

/// A local source file that reached the destination, with the stamp it had
/// when it was read: a Move deletes it only while it still has that stamp.
type Sent = (PathBuf, Option<SourceStamp>);

/// A remote source object that reached the destination, with the ETag of the
/// version read: a Move deletes that version and no newer one.
type SentRemote = (String, Option<String>);

/// Sidecar next to an uploaded file; lets a later run continue the same multipart upload.
#[derive(Serialize, Deserialize)]
struct UploadResume {
    key: String,
    upload_id: String,
    part_size: u64,
    source: SourceStamp,
}

/// What a resumed upload is bound to: the file's size, its modification time to
/// the nanosecond and its identity on disk. A file rewritten or replaced under
/// the same name stops matching, even within the same second.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
struct SourceStamp {
    size: u64,
    mtime_ns: u64,
    /// The creation time in nanoseconds, or empty where it is not known.
    id: String,
}

fn source_stamp(meta: &fs::Metadata) -> SourceStamp {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    SourceStamp { size: meta.len(), mtime_ns, id: file_identity(meta) }
}

/// When the file was created, as the file system keeps it: a file replaced
/// under the same name has a new creation time. Empty where the system does
/// not say, and the stamp then rests on size and modification time.
fn file_identity(meta: &fs::Metadata) -> String {
    meta.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
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
        let (outcome, stamp) = upload_one(
            app, &client, &bucket, &key, &src_root, &job.id, &job.cancel, None,
        )
        .await?;
        if matches!(outcome, Outcome::Done) {
            job.sent_local.lock().unwrap().push((src_root.clone(), stamp));
        }
        return Ok(outcome);
    }

    // A folder job walks the tree and uploads every file under the same relative path.
    let dest_prefix = format!("{}{}/", as_prefix(&job.dest_dir), name);
    let Some(walk) = walk_files(&src_root, &job.cancel)? else {
        return Ok(Outcome::Canceled);
    };
    let (files, empty_dirs) = (&walk.files, &walk.empty_dirs);
    let total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
    let mut done: u64 = 0;
    emit_progress(app, &job.id, 0, total);

    // With `skip`, an object already at the destination is left as it is and its
    // file is not counted as sent. That is also what lets a half-done folder
    // upload continue: an unfinished multipart upload is not an object yet.
    let existing: std::collections::HashSet<String> = if job.conflict == "skip" {
        let Some(listed) = list_objects_all(&client, &bucket, &dest_prefix, &job.cancel).await? else {
            return Ok(Outcome::Canceled);
        };
        listed.into_iter().map(|(key, _, _)| key).collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut changed: Vec<String> = Vec::new();
    for (path, sz, rel) in files {
        let key = format!("{dest_prefix}{rel}");
        if job.cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::Canceled);
        }
        if existing.contains(&key) {
            done += *sz;
            emit_progress(app, &job.id, done, total);
            continue;
        }
        emit_current_file(app, &job.id, rel);
        match upload_one(
            app, &client, &bucket, &key, path, &job.id, &job.cancel,
            Some((done, total)),
        )
        .await?
        {
            (Outcome::Canceled, _) => return Ok(Outcome::Canceled),
            (Outcome::Done, stamp) => job.sent_local.lock().unwrap().push((path.clone(), stamp)),
            // A file that changed while it was read stays out and is named in the note.
            (Outcome::Skipped(_), _) => changed.push(rel.clone()),
        }
        done += *sz;
    }
    // Empty folders are represented by a `.keep` object, the same way New Folder does it.
    for d in empty_dirs {
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
    emit_progress(app, &job.id, total, total);
    emit_skipped(app, &job.id, &job.name, &LeftOut { changed, ..LeftOut::of_walk(&walk) });
    Ok(Outcome::Done)
}

/// A walked directory tree, and what was left out of it on purpose.
struct Walk {
    /// Every file as (path, size, path relative to the root).
    files: Vec<(PathBuf, u64, String)>,
    /// Leaf folders with nothing to send, relative to the root.
    empty_dirs: Vec<String>,
    /// Symbolic links inside the tree: neither followed nor sent.
    skipped_links: usize,
    /// Relative paths, shown lossily, of items whose names are not valid UTF-8.
    skipped_names: Vec<String>,
}

/// Walks the tree under `root`; `None` when the job is canceled meanwhile.
fn walk_files(root: &std::path::Path, cancel: &AtomicBool) -> Result<Option<Walk>, String> {
    let mut walk = Walk {
        files: Vec::new(),
        empty_dirs: Vec::new(),
        skipped_links: 0,
        skipped_names: Vec::new(),
    };
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel_dir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let rd = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut any_entry = false;
        for item in rd.flatten() {
            let p = item.path();
            let meta = item.metadata();
            // A link inside the tree could lead anywhere, so it is left out. The
            // folder the user chose may itself be a link; that one is followed.
            if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
                walk.skipped_links += 1;
                continue;
            }
            // A remote key is text: a name that is not valid UTF-8 cannot travel
            // under its own name, and two such names could land on one key.
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                let lossy = item.file_name().to_string_lossy().into_owned();
                walk.skipped_names.push(if rel_dir.is_empty() {
                    lossy
                } else {
                    format!("{rel_dir}/{lossy}")
                });
                continue;
            };
            any_entry = true;
            let Ok(meta) = meta else { continue };
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
            walk.files.push((p, meta.len(), child_rel));
        }
        if !any_entry {
            walk.empty_dirs.push(rel_dir);
        }
    }
    Ok(Some(walk))
}

/// What a folder job left out on purpose, for the note after it.
#[derive(Default)]
struct LeftOut {
    /// Symbolic links inside the tree.
    links: usize,
    /// Names that are not valid UTF-8.
    bad_names: Vec<String>,
    /// Items written again while they were being read.
    changed: Vec<String>,
    /// Folder markers that hold data.
    held: Vec<String>,
}

impl LeftOut {
    fn of_walk(walk: &Walk) -> Self {
        LeftOut { links: walk.skipped_links, bad_names: walk.skipped_names.clone(), ..Default::default() }
    }
}

/// A few names and how many more, for a note.
fn some_names(names: &[String]) -> String {
    let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
    let more = names.len() - shown.len();
    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
    format!("{}{tail}", shown.join(", "))
}

/// Tells the interface what a folder job left out on purpose; it becomes a toast.
fn emit_skipped(app: &AppHandle, id: &str, name: &str, left: &LeftOut) {
    let mut parts = Vec::new();
    if left.links > 0 {
        parts.push(format!("{} symbolic link(s) not transferred", left.links));
    }
    if !left.bad_names.is_empty() {
        parts.push(format!("not transferred, the name is not valid UTF-8: {}", some_names(&left.bad_names)));
    }
    if !left.changed.is_empty() {
        parts.push(format!("not transferred, changed while being sent: {}", some_names(&left.changed)));
    }
    if !left.held.is_empty() {
        parts.push(format!("not transferred, a folder marker that holds data: {}", some_names(&left.held)));
    }
    if parts.is_empty() {
        return;
    }
    let detail = parts.join("; ");
    crate::devlog::verbose("transfer", format!("note {id} {detail}"));
    let _ = app.emit(
        "transfer-note",
        NoteEvt { id: id.to_string(), name: name.to_string(), detail },
    );
}

/// Why an upload leaves a file out: it changed while it was being read.
const CHANGED: &str = "the file changed while it was being sent, so it was not sent";

/// Drops an upload whose file changed while it went up: its record goes, and
/// the parts on the server with it.
async fn drop_changed_upload(
    client: &Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    sidecar: &Path,
) {
    let _ = fs::remove_file(sidecar);
    let aborted = client
        .abort_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .send()
        .await;
    let result = aborted.map_or_else(|e| s3_err(&e), |_| "done".to_string());
    crate::devlog::verbose("transfer", format!("unfinished upload of {key} aborted: {result}"));
}

/// The key and upload named by a resume record of any version.
#[derive(Deserialize)]
struct RecordedUpload {
    key: String,
    upload_id: String,
}

/// Drops the upload an earlier attempt left for this key: its record goes, and
/// its parts on the server with it, since they hold content this upload does
/// not continue. The upload behind a record for another key is not aborted,
/// since it may be running in another window of the app; this upload's record
/// takes its place.
async fn drop_earlier_attempt(client: &Client, bucket: &str, key: &str, sidecar: &Path) {
    let Ok(text) = fs::read_to_string(sidecar) else {
        return;
    };
    if let Ok(old) = serde_json::from_str::<RecordedUpload>(&text)
        && old.key == key
    {
        drop_changed_upload(client, bucket, key, &old.upload_id, sidecar).await;
    }
}

/// The sidecar's record when it still describes this file going to this key;
/// otherwise an earlier attempt for this key is dropped.
async fn usable_resume(
    client: &Client,
    bucket: &str,
    key: &str,
    source: &SourceStamp,
    sidecar: &Path,
) -> Option<UploadResume> {
    let text = fs::read_to_string(sidecar).ok()?;
    if let Ok(r) = serde_json::from_str::<UploadResume>(&text)
        && r.key == key
        && &r.source == source
    {
        return Some(r);
    }
    drop_earlier_attempt(client, bucket, key, sidecar).await;
    None
}

/// The upload a resume record points at, with the parts the server already
/// holds: its id, its part size and the parts. `None` means a fresh upload.
///
/// Only an upload the server says it no longer has starts over. Any other
/// failure of the listing ends the job with the record and the upload in
/// place, so the next try continues from the parts already sent.
async fn parts_to_continue(
    client: &Client,
    bucket: &str,
    key: &str,
    r: UploadResume,
    sidecar: &Path,
) -> Result<Option<(String, u64, HashMap<i32, String>)>, String> {
    match list_parts_all(client, bucket, key, &r.upload_id).await {
        Ok(done) => Ok(Some((r.upload_id, r.part_size, done))),
        Err(PartsErr::Gone) => {
            // The record goes with the upload it described.
            drop_changed_upload(client, bucket, key, &r.upload_id, sidecar).await;
            Ok(None)
        }
        Err(PartsErr::Other(e)) => Err(format!("ListParts {key}: {e}")),
    }
}

/// Why the parts of an upload could not be listed.
enum PartsErr {
    /// The server has no such upload any more.
    Gone,
    Other(String),
}

/// Uploads one file, in parts, continuing an earlier attempt when the sidecar
/// matches. A file sent in full comes back with the stamp it was read under, so
/// a Move can tell it was replaced since; one that changed while it was read is
/// skipped.
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
) -> Result<(Outcome, Option<SourceStamp>), String> {
    use std::io::{Read, Seek, SeekFrom};

    let meta = fs::metadata(src).map_err(|e| e.to_string())?;
    let total = meta.len();
    let sidecar = sidecar_of(src);
    // The resume record is read and written only as a plain file (N4).
    clear_link(&sidecar).map_err(|e| e.to_string())?;

    let (base, grand_total) = agg.unwrap_or((0, total));

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
        drop_earlier_attempt(client, bucket, key, &sidecar).await;
        emit_progress(app, id, base, grand_total);
        return Ok((Outcome::Done, Some(source_stamp(&meta))));
    }

    // A small file goes in one verified request; splitting it would cost three and gain nothing.
    if total <= CHUNK {
        if cancel.load(Ordering::Relaxed) {
            return Ok((Outcome::Canceled, None));
        }
        // Read through one open file, stamped before and after, so a change
        // during the read is seen.
        let mut file = fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let before = source_stamp(&file.metadata().map_err(|e| e.to_string())?);
        let mut data = Vec::with_capacity(total as usize);
        (&mut file)
            .take(total + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("{}: {e}", src.display()))?;
        let after = source_stamp(&file.metadata().map_err(|e| e.to_string())?);
        if data.len() as u64 != total || before.size != total || after != before {
            return Ok((Outcome::Skipped(CHANGED.into()), None));
        }
        put_object_verified(client, bucket, key, &data).await?;
        // An earlier multipart attempt, when the file was larger, is over.
        drop_earlier_attempt(client, bucket, key, &sidecar).await;
        emit_progress(app, id, base + total, grand_total);
        return Ok((Outcome::Done, Some(before)));
    }

    let default_part = std::cmp::max(CHUNK, total.div_ceil(10_000));

    // The stamp is read from the open file, so it describes the file the parts
    // are read from.
    let mut file = fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let source = source_stamp(&file.metadata().map_err(|e| e.to_string())?);
    if source.size != total {
        return Ok((Outcome::Skipped(CHANGED.into()), None));
    }

    let resumed = usable_resume(client, bucket, key, &source, &sidecar).await;

    let continued = match resumed {
        Some(r) => parts_to_continue(client, bucket, key, r, &sidecar).await?,
        None => None,
    };
    let (upload_id, part_size, mut done) = match continued {
        Some(c) => c,
        None => fresh_upload(client, bucket, key, default_part, &source, &sidecar).await?,
    };

    let num_parts: i32 = total.div_ceil(part_size) as i32;
    let mut buf = vec![0u8; part_size as usize];
    let mut parts: Vec<CompletedPart> = Vec::new();
    let mut sent: u64 = 0;
    let mut last_emit = Instant::now() - EMIT_EVERY;

    for pn in 1..=num_parts {
        let offset = (pn as u64 - 1) * part_size;
        let part_len = (total - offset).min(part_size);

        if let Some(etag) = done.remove(&pn) {
            parts.push(CompletedPart::builder().part_number(pn).e_tag(etag).build());
            sent += part_len;
            continue;
        }

        if cancel.load(Ordering::Relaxed) {
            // No abort: the sidecar and the server-side upload stay, so this can continue
            return Ok((Outcome::Canceled, None));
        }

        let slice = &mut buf[..part_len as usize];
        if let Err(e) = file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(slice)) {
            // A file cut short meanwhile has changed; any other failure is
            // reported with the file's name.
            if file.metadata().is_ok_and(|m| source_stamp(&m) != source) {
                drop_changed_upload(client, bucket, key, &upload_id, &sidecar).await;
                return Ok((Outcome::Skipped(CHANGED.into()), None));
            }
            return Err(format!("{}: {e}", src.display()));
        }

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
            emit_progress(app, id, base + sent, grand_total);
            last_emit = Instant::now();
        }
    }

    // A file edited while its parts went up is not completed: the parts would
    // join two versions of it. The upload on the server goes with its record.
    let now = source_stamp(&file.metadata().map_err(|e| e.to_string())?);
    if now != source {
        drop_changed_upload(client, bucket, key, &upload_id, &sidecar).await;
        return Ok((Outcome::Skipped(CHANGED.into()), None));
    }

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    let done = client
        .complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(&upload_id)
        .multipart_upload(completed)
        .send()
        .await
        .map_err(|e| s3_err(&e))?; // the sidecar survives, so a retry resumes

    let _ = fs::remove_file(&sidecar);
    verify_written(client, bucket, key, total, done.e_tag()).await?;
    Ok((Outcome::Done, Some(source)))
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
    source: &SourceStamp,
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
        source: source.clone(),
    };
    write_side(sidecar, &serde_json::to_string(&rec).unwrap_or_default());
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
) -> Result<HashMap<i32, String>, PartsErr> {
    let mut done = HashMap::new();
    let mut marker: Option<String> = None;
    // S3 allows at most 10 000 parts to an upload.
    let mut guard = PageGuard::new(10_000);
    loop {
        let mut req = client
            .list_parts()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id);
        if let Some(m) = &marker {
            req = req.part_number_marker(m);
        }
        let resp = req.send().await.map_err(|e| {
            if e.code() == Some("NoSuchUpload") {
                PartsErr::Gone
            } else {
                PartsErr::Other(s3_err(&e))
            }
        })?;
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
        guard
            .page(resp.parts().len(), marker.as_deref())
            .map_err(|stop| PartsErr::Other(stop.to_string()))?;
        if marker.is_none() {
            break;
        }
    }
    Ok(done)
}

// Multipart uploads that were never completed keep taking space on the volume, invisible in
// the listing. They are listed and aborted from Settings, only when the user confirms.

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
    let mut guard = PageGuard::new(TREE_MAX_ENTRIES);
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
                // The two markers together name the page asked for next.
                let token = format!("{k}\0{}", i.as_deref().unwrap_or(""));
                guard.page(resp.uploads().len(), Some(&token)).map_err(|stop| stop.to_string())?;
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
    let mut guard = PageGuard::new(10_000);
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
        // The sum is only shown, so a server going in circles just ends it.
        if marker.is_none() || guard.page(resp.parts().len(), marker.as_deref()).is_err() {
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

/// Reports how far a job has got. The speed is worked out where it is shown,
/// from these reports.
fn emit_progress(app: &AppHandle, id: &str, done: u64, total: u64) {
    let _ = app.emit(
        "transfer-progress",
        ProgressEvt {
            id: id.to_string(),
            bytes_done: done,
            bytes_total: total,
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
    use super::{as_prefix, dot_sibling, nth_copy, rel_parts, safe_join, split_for_copy};
    use std::path::Path;

    #[test]
    fn a_relative_path_that_climbs_out_is_refused() {
        for rel in ["../x", "a/../../x", "/tmp/x", "", "."] {
            assert!(rel_parts(rel, cfg!(windows)).is_none(), "{rel:?}");
            assert!(safe_join(Path::new("/dest"), rel).is_err(), "{rel:?}");
        }
        // A `.` between two names is dropped while the path is read, so the
        // result still lands inside the destination.
        for rel in ["x", "a/b/c.txt", "a b/.keep", "..a/b", "a..b", "a/./b"] {
            assert!(rel_parts(rel, cfg!(windows)).is_some(), "{rel:?}");
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

        remove_sent_files(&root, &[(big.clone(), None), (sub.clone(), None)]).unwrap();

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

        remove_sent_files(&root, &[(a, None), (b, None)]).unwrap();

        assert!(!root.exists(), "the root goes when everything in it was sent");
        assert!(d.exists(), "nothing above the root is touched");
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod local_name_tests {
    use super::{folder_item, is_local_name, join_name, zip_member};
    use std::path::Path;

    #[test]
    fn a_remote_name_is_one_ordinary_name_on_every_system() {
        for name in ["", ".", "..", "a/b", "/abs"] {
            for windows in [false, true] {
                assert!(!is_local_name(name, windows), "{name:?} windows={windows}");
            }
        }
        // Outside Windows a backslash or a colon is an ordinary character.
        for name in ["a.txt", "..a", "a..b", "models ", "x.", "a\\b", "C:x", "CON"] {
            assert!(is_local_name(name, false), "{name:?}");
        }
    }

    #[test]
    fn on_windows_a_name_cannot_change_what_the_path_points_at() {
        for name in [
            "a\\b", "\\x", "..\\x", "C:x", "C:\\x", "\\\\host\\share", "a:stream", "CON",
            "con.txt", "NUL", "COM1", "lpt9.log", "...", " ", ". .", "a<b", "a|b", "a?", "a*",
            "a\"b", "tab\t", "COM\u{b9}", "lpt\u{b2}.txt", "COM\u{b3} .log", "CONIN$", "conout$.txt",
        ] {
            assert!(!is_local_name(name, true), "{name:?}");
        }
        for name in ["a.txt", "..a", "models ", "x.", "CONSOLE", "COM10", "com0", "COM\u{b9}0", "CONIN", "Café menu.png"] {
            assert!(is_local_name(name, true), "{name:?}");
        }
    }

    #[test]
    fn a_downloaded_name_stays_in_its_folder() {
        let root = Path::new("/dest");
        assert_eq!(join_name(root, "a.txt").unwrap(), Path::new("/dest/a.txt"));
        for name in ["..", "/etc", "a/b", "", "."] {
            assert!(join_name(root, name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_zip_entry_cannot_climb_out_on_any_system() {
        assert_eq!(zip_member("top", "a/b.txt").unwrap(), "top/a/b.txt");
        assert_eq!(zip_member("", "a.txt").unwrap(), "a.txt");
        assert_eq!(zip_member("top", "a/./b").unwrap(), "top/a/b");
        // Names only Windows cannot store still go into an archive made elsewhere.
        for (top, rel) in [("proj", "aux.py"), ("proj", "a?b.txt"), ("", "con.txt"), ("proj", "12:30.log")] {
            assert!(zip_member(top, rel).is_ok(), "{top:?} + {rel:?}");
        }
        for (top, rel) in [
            ("..", "x"),
            ("top", "../x"),
            ("top", "a\\..\\x"),
            ("C:", "x"),
            ("top", "C:x"),
            ("top", "/x"),
            ("", ".."),
            ("a/b", "x"),
            ("top", ""),
        ] {
            assert!(zip_member(top, rel).is_err(), "{top:?} + {rel:?}");
        }
    }

    #[test]
    fn a_folder_download_sorts_every_key_the_same_way() {
        use super::FolderItem::{Dir, File, Held};
        assert!(matches!(folder_item("m/", "m/a.bin", 5), Some(File("a.bin"))));
        assert!(matches!(folder_item("m/", "m/sub/a.bin", 5), Some(File("sub/a.bin"))));
        assert!(matches!(folder_item("m/", "m/sub/.keep", 0), Some(Dir("sub"))));
        assert!(matches!(folder_item("m/", "m/sub/", 0), Some(Dir("sub"))));
        assert!(matches!(folder_item("m/", "m/.keep", 0), Some(Dir(""))));
        assert!(matches!(folder_item("m/", "m//.keep", 0), Some(Dir(""))));
        assert!(folder_item("m/", "m/", 0).is_none());
        // A marker that holds data is not a folder: the data arrives, or stays.
        assert!(matches!(folder_item("m/", "m/sub/.keep", 2), Some(File("sub/.keep"))));
        assert!(matches!(folder_item("m/", "m/sub/", 2), Some(Held("sub/"))));
    }
}

#[cfg(test)]
mod listing_guard_tests {
    // A stand-in server lists pages as each test scripts them.
    use super::list_objects_all;
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// A client whose server answers ListObjectsV2 from `page`: given the
    /// prefix and the continuation token asked for (empty for the first page),
    /// it returns the keys, the folders and the next token. Requests are counted.
    fn lister(
        page: impl Fn(&str, &str) -> (Vec<String>, Vec<String>, Option<String>) + Send + Sync + 'static,
    ) -> (Client, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            count.fetch_add(1, Ordering::SeqCst);
            let query = req.uri().query().unwrap_or("").to_string();
            let param = |name: &str| {
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix(name))
                    .map(|v| percent_encoding::percent_decode_str(v).decode_utf8_lossy().to_string())
                    .unwrap_or_default()
            };
            let (keys, dirs, next) = page(&param("prefix="), &param("continuation-token="));
            let mut xml = String::from("<ListBucketResult><Name>b</Name>");
            for k in keys {
                xml += &format!("<Contents><Key>{k}</Key><Size>1</Size><ETag>\"e\"</ETag></Contents>");
            }
            for d in dirs {
                xml += &format!("<CommonPrefixes><Prefix>{d}</Prefix></CommonPrefixes>");
            }
            match next {
                Some(n) => xml += &format!("<IsTruncated>true</IsTruncated><NextContinuationToken>{n}</NextContinuationToken>"),
                None => xml += "<IsTruncated>false</IsTruncated>",
            }
            xml += "</ListBucketResult>";
            http::Response::builder().status(200).body(SdkBody::from(xml)).unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), asked)
    }

    #[tokio::test]
    async fn a_listing_that_comes_round_again_fails() {
        let (client, _) = lister(|_, token| match token {
            "" => (vec!["p/1".into()], vec![], Some("A".into())),
            "A" => (vec!["p/2".into()], vec![], Some("B".into())),
            _ => (vec!["p/3".into()], vec![], Some("A".into())),
        });
        let got = tokio::time::timeout(Duration::from_secs(10), list_objects_all(&client, "b", "p/", &AtomicBool::new(false)))
            .await
            .expect("the listing ends");
        assert!(got.is_err_and(|e| e.contains("page token")));
    }

    #[tokio::test]
    async fn a_cancel_stops_a_listing_that_never_ends() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (client, asked) = lister(move |_, token| {
            let n: u64 = token.parse().unwrap_or(0);
            if n == 5 {
                flag.store(true, Ordering::SeqCst);
            }
            (vec![format!("p/{n}")], vec![], Some((n + 1).to_string()))
        });
        let got = tokio::time::timeout(Duration::from_secs(10), list_objects_all(&client, "b", "p/", &cancel))
            .await
            .expect("the listing stops");
        assert_eq!(got, Ok(None));
        assert!(asked.load(Ordering::SeqCst) < 10, "stopped right after the cancel");
    }
}

#[cfg(test)]
mod conditional_tests {
    // A stand-in server keeps each object at ETag "\"v1\"" and answers a
    // condition on any other version with 412, as the RunPod gateway does. Keys
    // named `gone*` are not there, `broken*` fail, `moved*` are at "\"v2\"".
    use super::{
        ERR_SOURCE_CHANGED, copy_object_checked, delete_remote_sources, delete_sent_object,
        server_copy,
    };
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::{Arc, Mutex};

    /// Every request as "METHOD path condition", where the condition is the
    /// `If-Match` or `x-amz-copy-source-if-match` header, or "-".
    fn server() -> (Client, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let header = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
            let cond = header("if-match").or_else(|| header("x-amz-copy-source-if-match"));
            let copy_src = header("x-amz-copy-source").unwrap_or_default();
            let path = req.uri().path().to_string();
            log.lock().unwrap().push(format!("{} {path} {}", req.method(), cond.clone().unwrap_or("-".into())));
            let subject = if copy_src.is_empty() { path.clone() } else { copy_src };
            let now = if subject.contains("moved") { "\"v2\"" } else { "\"v1\"" };
            let stale = cond.is_some_and(|c| c != now);
            let method = req.method().as_str().to_string();
            let (status, body) = match method.as_str() {
                _ if subject.contains("broken") => (500, ""),
                "HEAD" if subject.contains("gone") => (404, ""),
                _ if stale => (412, ""),
                "PUT" => (200, "<CopyObjectResult><ETag>\"v1\"</ETag></CopyObjectResult>"),
                "HEAD" => (200, ""),
                _ => (204, ""),
            };
            http::Response::builder()
                .status(status)
                .header("etag", now)
                .header("content-length", if method == "HEAD" { "9" } else { "0" })
                .body(SdkBody::from(body))
                .unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), seen)
    }

    #[tokio::test]
    async fn a_copy_is_bound_to_the_version_it_names() {
        let (client, seen) = server();
        copy_object_checked(&client, "b", "a.bin", "c.bin", 9, "\"v1\"").await.unwrap();
        assert!(seen.lock().unwrap()[0].ends_with("\"v1\""), "{:?}", seen.lock().unwrap());
        let err = copy_object_checked(&client, "b", "a.bin", "c.bin", 9, "\"v0\"").await.unwrap_err();
        assert_eq!(err, ERR_SOURCE_CHANGED);
    }

    // A source that changed between the HEAD and the copy is copied once more,
    // at its new version, which is what a Move then deletes.
    #[tokio::test]
    async fn a_copy_tries_again_at_the_new_version() {
        let (client, _) = server();
        assert_eq!(server_copy(&client, "b", "moved.bin", "c.bin", 9, "\"v1\"").await, Ok("\"v2\"".to_string()));
    }

    #[tokio::test]
    async fn a_move_deletes_only_the_version_it_sent() {
        let (client, seen) = server();
        assert_eq!(delete_sent_object(&client, "b", "a.bin", Some("\"v1\"")).await, Ok(true));
        assert_eq!(delete_sent_object(&client, "b", "a.bin", Some("\"v0\"")).await, Ok(false), "a newer version stays");
        let seen = seen.lock().unwrap().clone();
        assert!(seen.iter().filter(|r| r.starts_with("DELETE")).all(|r| !r.ends_with(" -")), "every delete carries its version: {seen:?}");
    }

    // The gateway answers a key already gone with 412 as well: that one was deleted.
    #[tokio::test]
    async fn a_delete_that_finds_nothing_left_counts_as_done() {
        let (client, _) = server();
        assert_eq!(delete_sent_object(&client, "b", "gone.bin", Some("\"v0\"")).await, Ok(true));
    }

    // Without a version the request goes without a condition, as before.
    #[tokio::test]
    async fn no_version_means_no_condition() {
        let (client, seen) = server();
        delete_sent_object(&client, "b", "a.bin", None).await.unwrap();
        delete_sent_object(&client, "b", "a.bin", Some("")).await.unwrap();
        assert!(seen.lock().unwrap().iter().all(|r| r.ends_with(" -")), "{:?}", seen.lock().unwrap());
    }

    // A folder Move names what it kept and what failed, and leaves the folder
    // markers above them; the rest goes.
    #[tokio::test]
    async fn a_folder_move_keeps_what_changed_and_its_folders() {
        let (client, seen) = server();
        let v = |e: &str| Some(e.to_string());
        let sent = vec![
            ("src/a.bin".to_string(), v("\"v1\"")),
            ("src/sub/b.bin".to_string(), v("\"v0\"")),
            ("src/other/broken.bin".to_string(), v("\"v1\"")),
            ("src/done/c.bin".to_string(), v("\"v1\"")),
        ];
        let err = delete_remote_sources(&client, "b", "src", true, &sent, &[]).await.unwrap_err();
        assert!(err.contains("sub/b.bin") && err.contains("1 object(s) could not be removed"), "{err}");
        let seen = seen.lock().unwrap().clone();
        let deleted = |p: &str| seen.iter().any(|r| r.starts_with(&format!("DELETE /b/{p} ")));
        assert!(deleted("src/done/"), "an emptied folder goes: {seen:?}");
        assert!(!deleted("src/sub/"), "the folder of a kept object stays");
        assert!(!deleted("src/"), "the moved folder stays while something is left in it");
    }

    // A folder marker that holds data was not transferred: it stays, and so do
    // the folders above it, the moved folder included.
    #[tokio::test]
    async fn a_marker_that_holds_data_stays() {
        let (client, seen) = server();
        let sent = vec![("src/sub/a.bin".to_string(), Some("\"v1\"".to_string()))];
        delete_remote_sources(&client, "b", "src", true, &sent, &["src/sub/".to_string()]).await.unwrap();
        let seen = seen.lock().unwrap().clone();
        let deleted = |p: &str| seen.iter().any(|r| r.starts_with(&format!("DELETE /b/{p} ")));
        assert!(deleted("src/sub/a.bin"));
        assert!(!deleted("src/sub/"), "the marker with data stays: {seen:?}");
        assert!(!deleted("src/"), "and the folder above it");
    }

    #[tokio::test]
    async fn a_single_move_uses_its_own_version() {
        let (client, _) = server();
        let sent = vec![("x/a.bin".to_string(), Some("\"v0\"".to_string()))];
        let err = delete_remote_sources(&client, "b", "x/a.bin", false, &sent, &[]).await.unwrap_err();
        assert!(err.contains("kept"), "{err}");
    }

}

#[cfg(test)]
mod walk_cancel_tests {
    use super::walk_files;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_canceled_job_stops_walking_the_tree() {
        let d = std::env::temp_dir().join(format!("bgwalk-cancel-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/a.txt"), b"a").unwrap();
        assert!(walk_files(&d, &AtomicBool::new(true)).unwrap().is_none(), "canceled");
        assert!(walk_files(&d, &AtomicBool::new(false)).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(all(test, unix))]
mod local_link_tests {
    use super::{create_side, remove_local_source, walk_files, write_side};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bglink-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_folder_walk_leaves_out_links_and_unreadable_names() {
        let d = scratch("walk");
        let root = d.join("tree");
        let outside = d.join("outside.txt");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(&outside, b"secret").unwrap();
        fs::write(root.join("a.txt"), b"a").unwrap();
        symlink(&outside, root.join("link.txt")).unwrap();
        symlink(&d, root.join("sub/uplink")).unwrap();

        let walk = walk_files(&root, &std::sync::atomic::AtomicBool::new(false)).unwrap().unwrap();
        let mut rels: Vec<&str> = walk.files.iter().map(|(_, _, r)| r.as_str()).collect();
        rels.sort();
        assert_eq!(rels, ["a.txt"]);
        // A folder holding only a left-out link still arrives, empty.
        assert_eq!(walk.empty_dirs, ["sub"]);
        assert_eq!(walk.skipped_links, 2);
        assert!(walk.skipped_names.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    // macOS (APFS) refuses to create a name that is not valid UTF-8 at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_left_out_and_kept_by_a_move() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let d = scratch("utf8");
        let root = d.join("tree");
        fs::create_dir_all(root.join(OsStr::from_bytes(b"dir\xfe"))).unwrap();
        fs::write(root.join(OsStr::from_bytes(b"bad\xff.txt")), b"b").unwrap();
        let good = root.join("a.txt");
        fs::write(&good, b"a").unwrap();

        let walk = walk_files(&root, &std::sync::atomic::AtomicBool::new(false)).unwrap().unwrap();
        let mut rels: Vec<&str> = walk.files.iter().map(|(_, _, r)| r.as_str()).collect();
        rels.sort();
        assert_eq!(rels, ["a.txt"]);
        let mut names = walk.skipped_names.clone();
        names.sort();
        assert_eq!(names, ["bad\u{fffd}.txt", "dir\u{fffd}"]);

        remove_local_source(&root, true, &[(good.clone(), None)]).unwrap();
        assert!(!good.exists(), "a sent file is removed");
        assert!(root.join(OsStr::from_bytes(b"dir\xfe")).is_dir(), "a left-out folder stays");
        assert!(root.join(OsStr::from_bytes(b"bad\xff.txt")).exists(), "a left-out file stays");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn moving_a_folder_chosen_through_a_link_removes_only_the_link() {
        let d = scratch("rootlink");
        let real = d.join("real");
        fs::create_dir_all(&real).unwrap();
        let file = real.join("a.txt");
        fs::write(&file, b"a").unwrap();
        let link = d.join("link");
        symlink(&real, &link).unwrap();

        remove_local_source(&link, true, &[(link.join("a.txt"), None)]).unwrap();

        assert!(fs::symlink_metadata(&link).is_err(), "the link is gone");
        assert!(file.exists(), "the folder it pointed to keeps its files");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_side_file_is_never_written_through_a_link() {
        let d = scratch("side");
        let victim = d.join("victim.txt");
        fs::write(&victim, b"keep").unwrap();
        let sidecar = d.join(".x.bgul");
        let part = d.join(".x.part");
        symlink(&victim, &sidecar).unwrap();
        symlink(&victim, &part).unwrap();

        write_side(&sidecar, "{}");
        drop(create_side(&part).unwrap());

        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        for p in [&sidecar, &part] {
            assert!(!fs::symlink_metadata(p).unwrap().file_type().is_symlink(), "{p:?}");
        }
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod busy_tests {
    use super::{Job, TransferManager};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    fn job(conn: &str) -> Job {
        Job {
            id: "t1".into(),
            kind: "upload".into(),
            src: String::new(),
            dest_dir: String::new(),
            name: String::new(),
            is_dir: false,
            move_src: false,
            conflict: "skip".into(),
            zip_srcs: Vec::new(),
            conn_id: Some(conn.into()),
            cancel: Arc::new(AtomicBool::new(false)),
            sent_local: Mutex::new(Vec::new()),
            sent_remote: Mutex::new(Vec::new()),
            keep_remote: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn a_connection_is_busy_while_its_job_waits_or_runs() {
        let mgr = TransferManager::new();
        assert!(!mgr.busy_with("A"));
        mgr.queue.lock().unwrap().push_back(job("A"));
        assert!(mgr.busy_with("A") && !mgr.busy_with("B"));
        // A canceled job still in the queue no longer holds its connection.
        mgr.queue.lock().unwrap()[0]
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(!mgr.busy_with("A"));
        *mgr.current.lock().unwrap() = Some("B".into());
        assert!(mgr.busy_with("B"));
    }
}

#[cfg(test)]
mod upload_stamp_tests {
    use super::{UploadResume, remove_local_source, remove_sent_files, source_stamp};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bgstamp-{name}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    fn t0() -> SystemTime {
        UNIX_EPOCH + Duration::new(1_800_000_000, 100)
    }

    #[test]
    fn a_rewrite_within_the_same_second_breaks_the_resume() {
        let d = dir("rewrite");
        let p = d.join("a.bin");
        fs::write(&p, b"aaaa").unwrap();
        set_mtime(&p, t0());
        let before = source_stamp(&fs::metadata(&p).unwrap());
        fs::write(&p, b"bbbb").unwrap();
        set_mtime(&p, t0() + Duration::from_nanos(500));
        assert_ne!(before, source_stamp(&fs::metadata(&p).unwrap()));
        let _ = fs::remove_dir_all(&d);
    }

    // Windows passes a replaced file's creation time on to its successor for a
    // few seconds ("tunneling"), so there the identity cannot tell them apart.
    #[cfg(unix)]
    #[test]
    fn a_file_replaced_under_the_same_name_breaks_the_resume() {
        let d = dir("replace");
        let (p, q) = (d.join("a.bin"), d.join("b.bin"));
        fs::write(&p, b"aaaa").unwrap();
        set_mtime(&p, t0());
        let before = source_stamp(&fs::metadata(&p).unwrap());
        // Created later than the first, beyond the file system clock's grain.
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&q, b"bbbb").unwrap();
        set_mtime(&q, t0());
        fs::rename(&q, &p).unwrap();
        assert_ne!(before, source_stamp(&fs::metadata(&p).unwrap()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_untouched_file_keeps_its_resume() {
        let d = dir("same");
        let p = d.join("a.bin");
        fs::write(&p, b"aaaa").unwrap();
        let first = source_stamp(&fs::metadata(&p).unwrap());
        assert_eq!(first, source_stamp(&fs::metadata(&p).unwrap()));
        let _ = fs::remove_dir_all(&d);
    }

    // The check before completing reads the open file, so it sees an edit made
    // meanwhile through the file's name.
    #[test]
    fn the_open_file_sees_an_edit_made_meanwhile() {
        let d = dir("open");
        let p = d.join("a.bin");
        fs::write(&p, b"aaaa").unwrap();
        set_mtime(&p, t0());
        let file = fs::File::open(&p).unwrap();
        let before = source_stamp(&file.metadata().unwrap());
        fs::write(&p, b"bbbb").unwrap();
        set_mtime(&p, t0() + Duration::from_nanos(500));
        assert_ne!(before, source_stamp(&file.metadata().unwrap()));
        let _ = fs::remove_dir_all(&d);
    }

    // A Move deletes what was sent, not whatever holds its name by then: a file
    // saved again meanwhile (a new file renamed over it) stays.
    #[test]
    fn a_folder_move_keeps_a_file_replaced_after_it_was_sent() {
        let d = dir("movekeep");
        let root = d.join("T");
        fs::create_dir_all(&root).unwrap();
        let (a, b) = (root.join("a.bin"), root.join("b.bin"));
        fs::write(&a, b"old").unwrap();
        fs::write(&b, b"sent").unwrap();
        let sa = source_stamp(&fs::metadata(&a).unwrap());
        let sb = source_stamp(&fs::metadata(&b).unwrap());
        let tmp = root.join("a.tmp");
        fs::write(&tmp, b"new!").unwrap();
        fs::rename(&tmp, &a).unwrap();

        let err = remove_sent_files(&root, &[(a.clone(), Some(sa)), (b.clone(), Some(sb))]);
        assert!(a.exists(), "the file saved again stays");
        assert!(!b.exists(), "an unchanged sent file goes");
        assert!(err.is_err_and(|e| e.contains("changed")), "the user hears why a.bin stayed");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_single_file_move_keeps_a_file_replaced_after_it_was_sent() {
        let d = dir("movekeep1");
        let a = d.join("a.bin");
        fs::write(&a, b"old").unwrap();
        let sa = source_stamp(&fs::metadata(&a).unwrap());
        let tmp = d.join("a.tmp");
        fs::write(&tmp, b"new!").unwrap();
        fs::rename(&tmp, &a).unwrap();

        assert!(remove_local_source(&a, false, &[(a.clone(), Some(sa))]).is_err());
        assert!(a.exists(), "the file saved again stays");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_written_now_is_read_back() {
        let d = dir("roundtrip");
        let p = d.join("a.bin");
        fs::write(&p, b"aaaa").unwrap();
        let source = source_stamp(&fs::metadata(&p).unwrap());
        let rec = UploadResume { key: "k".into(), upload_id: "u".into(), part_size: 8, source: source.clone() };
        let back: UploadResume = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert!(back.key == "k" && back.upload_id == "u" && back.part_size == 8);
        assert_eq!(back.source, source);
        let _ = fs::remove_dir_all(&d);
    }

    // A file with no record of being sent is left alone: a rename took the one
    // that was, and this one arrived afterwards.
    #[test]
    fn a_move_leaves_a_file_it_did_not_send() {
        let d = dir("norecord");
        let a = d.join("a.bin");
        fs::write(&a, b"arrived later").unwrap();
        remove_local_source(&a, false, &[]).unwrap();
        assert!(a.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_resume_record_from_an_earlier_version_is_not_trusted() {
        let old = r#"{"key":"k","upload_id":"u","part_size":8388608,"size":4,"mtime":1800000000}"#;
        assert!(serde_json::from_str::<UploadResume>(old).is_err());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod network_mark_tests {
    use super::mark_from_network;
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn quarantine_of(p: &Path) -> Option<String> {
        let c = CString::new(p.as_os_str().as_bytes()).unwrap();
        let mut buf = [0u8; 256];
        // SAFETY: both names are NUL-terminated and the buffer outlives the call.
        let n = unsafe {
            libc::getxattr(
                c.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                0,
            )
        };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bgmark-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_downloaded_file_carries_the_quarantine_mark() {
        let d = scratch("apfs");
        let f = d.join("x.txt");
        fs::write(&f, b"x").unwrap();
        mark_from_network(&f);
        let mark = quarantine_of(&f);
        let _ = fs::remove_dir_all(&d);
        let mark = mark.expect("the file has no quarantine mark");
        assert!(mark.starts_with("0081;") && mark.ends_with(";BG Bucket Browser;"), "{mark}");
    }

    /// Marks a file on a fresh disk image of the given format; returns the mark
    /// and whether a `._` side file appeared.
    fn mark_on_image(fs_name: &str, tag: &str) -> (Option<String>, bool) {
        let d = scratch(tag);
        let image = d.join("disk.dmg");
        let made = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", "5m", "-fs", fs_name, "-volname", "BGMARK", "-o"])
            .arg(&image)
            .status()
            .unwrap();
        assert!(made.success(), "hdiutil create failed for {fs_name}");
        let mount = d.join("mnt");
        fs::create_dir_all(&mount).unwrap();
        let attached = Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image)
            .status()
            .unwrap();
        assert!(attached.success(), "hdiutil attach failed for {fs_name}");
        let f = mount.join("x.txt");
        fs::write(&f, b"x").unwrap();
        mark_from_network(&f);
        let mark = quarantine_of(&f);
        let side = mount.join("._x.txt").exists();
        let _ = Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mount).status();
        let _ = fs::remove_dir_all(&d);
        (mark, side)
    }

    #[test]
    fn a_fat_volume_gets_no_mark_and_no_side_file() {
        assert_eq!(mark_on_image("MS-DOS", "fat"), (None, false));
    }

    #[test]
    fn an_exfat_volume_gets_no_mark_and_no_side_file() {
        assert_eq!(mark_on_image("ExFAT", "exfat"), (None, false));
    }
}

#[cfg(test)]
mod stale_resume_tests {
    use super::{SourceStamp, UploadResume, drop_earlier_attempt, parts_to_continue, usable_resume};
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// A client whose server answers every request with an empty success and
    /// records it as "METHOD path?query".
    fn recorder() -> (Client, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let target = req.uri().path_and_query().map_or("", |p| p.as_str()).to_string();
            log.lock().unwrap().push(format!("{} {target}", req.method()));
            http::Response::builder().status(204).body(SdkBody::empty()).unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), seen)
    }

    fn stamp(mtime_ns: u64) -> SourceStamp {
        SourceStamp { size: 4, mtime_ns, id: "7".into() }
    }

    /// A client whose server answers `ListParts` with `status` and `body`, any
    /// other request with an empty success, and records each as "METHOD path?query".
    fn answering_list_parts(status: u16, body: &'static str) -> (Client, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let target = req.uri().path_and_query().map_or("", |p| p.as_str()).to_string();
            log.lock().unwrap().push(format!("{} {target}", req.method()));
            if req.method() == http::Method::GET {
                http::Response::builder().status(status).body(SdkBody::from(body)).unwrap()
            } else {
                http::Response::builder().status(204).body(SdkBody::empty()).unwrap()
            }
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), seen)
    }

    fn resume(upload_id: &str) -> UploadResume {
        UploadResume { key: "k".into(), upload_id: upload_id.into(), part_size: 8, source: stamp(1) }
    }

    const ONE_PART: &str = "<ListPartsResult><IsTruncated>false</IsTruncated>\
        <Part><PartNumber>1</PartNumber><ETag>\"e1\"</ETag><Size>8</Size></Part></ListPartsResult>";
    const GONE: &str =
        "<Error><Code>NoSuchUpload</Code><Message>Upload session not found</Message></Error>";
    const DENIED: &str = "<Error><Code>AccessDenied</Code><Message>no</Message></Error>";

    #[tokio::test]
    async fn an_upload_the_server_still_holds_continues_from_its_parts() {
        let (d, p) = sidecar("parts", &record("k", "u1", stamp(1)));
        let (client, seen) = answering_list_parts(200, ONE_PART);
        let got = parts_to_continue(&client, "b", "k", resume("u1"), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        let (id, part_size, done) = got.unwrap().expect("the upload continues");
        assert_eq!((id.as_str(), part_size, done.get(&1).map(String::as_str)), ("u1", 8, Some("\"e1\"")));
        assert!(kept && seen.lock().unwrap().iter().all(|r| r.starts_with("GET ")));
    }

    #[tokio::test]
    async fn an_upload_the_server_no_longer_holds_starts_over() {
        let (d, p) = sidecar("gone", &record("k", "u1", stamp(1)));
        let (client, _) = answering_list_parts(404, GONE);
        let got = parts_to_continue(&client, "b", "k", resume("u1"), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        assert!(matches!(got, Ok(None)), "a fresh upload is what is left");
        assert!(!kept, "the record of an upload that is gone goes too");
    }

    #[tokio::test]
    async fn a_parts_listing_that_fails_keeps_the_upload_and_its_record() {
        let (d, p) = sidecar("failed", &record("k", "u1", stamp(1)));
        let (client, seen) = answering_list_parts(403, DENIED);
        let got = parts_to_continue(&client, "b", "k", resume("u1"), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        assert!(got.is_err(), "the job fails instead of starting over");
        assert!(kept, "the record stays for the next try");
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|r| !r.starts_with("DELETE ")), "the upload was aborted: {seen:?}");
    }

    /// A sidecar holding `text`, in a folder of its own; the folder is returned
    /// for the test to remove.
    fn sidecar(tag: &str, text: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("bgresume-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let p = d.join(".a.bin.bgul");
        fs::write(&p, text).unwrap();
        (d, p)
    }

    fn record(key: &str, upload_id: &str, source: SourceStamp) -> String {
        let rec = UploadResume { key: key.into(), upload_id: upload_id.into(), part_size: 8, source };
        serde_json::to_string(&rec).unwrap()
    }

    #[tokio::test]
    async fn a_record_for_the_same_file_is_kept() {
        let (d, p) = sidecar("same", &record("k", "u1", stamp(1)));
        let (client, seen) = recorder();
        let got = usable_resume(&client, "b", "k", &stamp(1), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        assert!(got.is_some_and(|r| r.upload_id == "u1"));
        assert!(seen.lock().unwrap().is_empty(), "nothing is aborted");
        assert!(kept);
    }

    #[tokio::test]
    async fn a_record_for_a_changed_file_aborts_its_upload() {
        let (d, p) = sidecar("changed", &record("k", "u1", stamp(1)));
        let (client, seen) = recorder();
        let got = usable_resume(&client, "b", "k", &stamp(2), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        assert!(got.is_none());
        let seen = seen.lock().unwrap();
        assert!(seen.len() == 1 && seen[0].starts_with("DELETE ") && seen[0].contains("uploadId=u1"), "{seen:?}");
        assert!(!kept, "the record goes with its upload");
    }

    // Another key's upload may be running in another window of the app.
    #[tokio::test]
    async fn a_record_for_another_key_is_left_alone() {
        let (d, p) = sidecar("key", &record("k", "u1", stamp(1)));
        let (client, seen) = recorder();
        let got = usable_resume(&client, "b", "other", &stamp(1), &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        assert!(got.is_none());
        assert!(seen.lock().unwrap().is_empty(), "nothing is aborted");
        assert!(kept);
    }

    #[tokio::test]
    async fn a_record_from_an_earlier_version_aborts_its_upload() {
        let old = r#"{"key":"k","upload_id":"u0","part_size":8388608,"size":4,"mtime":1800000000}"#;
        let (d, p) = sidecar("old", old);
        let (client, seen) = recorder();
        let got = usable_resume(&client, "b", "k", &stamp(1), &p).await;
        let _ = fs::remove_dir_all(&d);
        assert!(got.is_none());
        let seen = seen.lock().unwrap();
        assert!(seen.len() == 1 && seen[0].starts_with("DELETE ") && seen[0].contains("uploadId=u0"), "{seen:?}");
    }

    // A file that shrank below one part goes up in a single request; the
    // multipart attempt it made while larger is dropped all the same.
    #[tokio::test]
    async fn a_whole_upload_drops_the_earlier_attempt_for_its_key() {
        let (d, p) = sidecar("small", &record("k", "u2", stamp(1)));
        let (client, seen) = recorder();
        drop_earlier_attempt(&client, "b", "k", &p).await;
        let kept = p.exists();
        let _ = fs::remove_dir_all(&d);
        let seen = seen.lock().unwrap();
        assert!(seen.len() == 1 && seen[0].contains("uploadId=u2"), "{seen:?}");
        assert!(!kept);
    }
}
