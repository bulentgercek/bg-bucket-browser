//! Directory listing for both sides: the network volume over S3
//! `ListObjectsV2`, and the local disk over `std::fs`.
//!
//! Both commands return the same entry shape, so a pane renders a remote and a
//! local directory with the same code.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
use aws_sdk_s3::primitives::DateTime;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::config::{active_client, client_by_id};
use crate::core::{DIR_MAX_ENTRIES, PageGuard, PageStop};
use crate::local_path::{HomeNotFound, resolve_local};

/// One entry in a directory, shared by the remote and the local side.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    /// The entry's own name, never a path.
    pub name: String,
    /// `"dir"` or `"file"`.
    pub kind: &'static str,
    /// Size in bytes; `None` for a directory, which the table shows as "—".
    pub size: Option<i64>,
    /// Modification time as Unix epoch milliseconds.
    pub modified: Option<i64>,
    /// Icon hint: `"text"`, `"zip"` or `"video"`; `None` draws a plain file.
    pub glyph: Option<&'static str>,
}

/// What went wrong, as a value the frontend can switch on.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ListErrKind {
    /// The path does not exist, remotely or locally.
    NotFound,
    /// Permission denied.
    AccessDenied,
    /// The server rejected our identity: a wrong key or a wrong signature.
    ///
    /// Separate from `AccessDenied`, which means the identity was accepted and
    /// the operation was not: this one is a connection problem, not a path one.
    Credentials,
    /// The server could not be reached (DNS, connection, timeout).
    Unreachable,
    /// Local filesystem I/O error.
    Io,
    /// The connection's endpoint may not carry credentials (plain `http` to
    /// another machine, or not a valid address).
    Endpoint,
    /// A newer listing started on the same tab, so this one stopped.
    Canceled,
    /// The folder holds more entries than a listing may (`DIR_MAX_ENTRIES`).
    TooLarge,
    /// Anything that could not be classified.
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListErr {
    pub kind: ListErrKind,
    /// A short technical line in English, for the log and for support.
    pub detail: String,
}

impl ListErr {
    fn new(kind: ListErrKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

// ── Shared helpers ───────────────────────────────────────────────

/// Maps a file extension to an icon hint.
///
/// The video hint is only the type icon: a real poster frame comes from
/// `thumbs.rs` and this is what is drawn until (or unless) one exists.
fn glyph_for(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("txt" | "md" | "json" | "log" | "toml" | "yaml" | "yml" | "csv" | "ini") => {
            Some("text")
        }
        Some("zip" | "gz" | "tar" | "tgz" | "7z" | "rar" | "bz2" | "xz") => Some("zip"),
        Some("mp4" | "webm" | "mov" | "mkv" | "avi" | "m4v") => Some("video"),
        _ => None,
    }
}

// Converts the SDK's timestamp to the epoch milliseconds the frontend expects.
fn dt_millis(dt: &DateTime) -> i64 {
    dt.secs().saturating_mul(1000) + (dt.subsec_nanos() / 1_000_000) as i64
}

/// Directories first, then files, each group by name and case-insensitive.
fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        let rank = |k: &str| if k == "dir" { 0 } else { 1 };
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
}

// ── Remote ──────────────────────────────────────────────────────────

/// Reduces an SDK error to the four cases the UI can say something about.
fn classify_remote<R>(e: &SdkError<ListObjectsV2Error, R>) -> ListErr {
    let message = e
        .message()
        .map(str::to_string)
        .unwrap_or_else(|| "S3 request failed".into());
    // The S3 code leads the detail line: without it the log says what went
    // wrong but not which case of it, and the classification below is invisible.
    let detail = match e.code() {
        Some(code) => format!("{code}: {message}"),
        None => message,
    };
    let kind = match e {
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => ListErrKind::Unreachable,
        SdkError::ServiceError(_) => match e.code() {
            Some("NoSuchBucket" | "NoSuchKey") => ListErrKind::NotFound,
            // The same codes the connection test classifies (commands.rs).
            Some("SignatureDoesNotMatch" | "InvalidAccessKeyId") => ListErrKind::Credentials,
            Some("AccessDenied") => ListErrKind::AccessDenied,
            _ => ListErrKind::Unknown,
        },
        _ => ListErrKind::Unknown,
    };
    ListErr::new(kind, detail)
}

/// The tab a listing belongs to, and its place among that tab's requests.
#[derive(Deserialize)]
pub struct ListLane {
    tab: String,
    seq: u64,
}

/// The newest listing request of each tab.
static LANES: LazyLock<Mutex<HashMap<String, watch::Sender<u64>>>> = LazyLock::new(Default::default);

/// Forgets a tab's entry when the listing that ends was still its newest, so
/// the map holds only tabs with a listing under way.
struct LaneEnd(String, u64);

impl Drop for LaneEnd {
    fn drop(&mut self) {
        let mut lanes = LANES.lock().unwrap_or_else(|e| e.into_inner());
        if lanes.get(&self.0).is_some_and(|tx| *tx.borrow() == self.1) {
            lanes.remove(&self.0);
        }
    }
}

/// Registers a listing as the newest on its tab; the receiver changes when a
/// newer one starts there.
fn enter_lane(lane: &str, seq: u64) -> watch::Receiver<u64> {
    let mut lanes = LANES.lock().unwrap_or_else(|e| e.into_inner());
    let tx = lanes.entry(lane.to_string()).or_insert_with(|| watch::channel(0).0);
    tx.send_modify(|newest| *newest = (*newest).max(seq));
    tx.subscribe()
}

/// Lists one remote directory. `path` is relative to the volume root: `""` is
/// the root, `"models/loras"` a subdirectory.
///
/// S3 stores keys, not directories. Listing with `delimiter("/")` makes the
/// service do the grouping: everything up to the next `/` comes back as a
/// common prefix (a directory), the rest as the files of this directory.
#[tauri::command]
pub async fn list_remote(
    app: tauri::AppHandle,
    path: String,
    conn_id: Option<String>,
    lane: Option<ListLane>,
) -> Result<Vec<DirEntry>, ListErr> {
    // The lane is taken first, so even a request that fails at once stops the
    // older listing on its tab.
    let mut newer = lane.map(|l| (enter_lane(&l.tab, l.seq), l.seq, LaneEnd(l.tab, l.seq)));
    // A transfer lists its destination on the connection it was started on.
    let client = match conn_id.as_deref() {
        Some(id) => client_by_id(&app, id),
        None => active_client(&app),
    };
    let (client, bucket) = client.map_err(|e| {
        let detail = e.to_string();
        let kind = if matches!(detail.as_str(), "insecureEndpoint" | "badEndpoint") {
            ListErrKind::Endpoint
        } else {
            ListErrKind::Unknown
        };
        ListErr::new(kind, detail)
    })?;

    // The root is the empty prefix; anything else carries exactly one trailing slash.
    let prefix = match path.trim_matches('/') {
        "" => String::new(),
        p => format!("{p}/"),
    };

    let mut entries: Vec<DirEntry> = Vec::new();
    // The SDK's paginator follows the continuation token and stops when it
    // repeats; it never looks at `is_truncated`.
    let mut pages = client
        .list_objects_v2()
        .bucket(&bucket)
        .prefix(&prefix)
        .delimiter("/")
        .into_paginator()
        .send();
    // A listing for a tab stops as soon as a newer one starts there: the page
    // in flight is dropped along with its request.
    let mut guard = PageGuard::new(DIR_MAX_ENTRIES);
    loop {
        let page = match newer.as_mut() {
            Some((rx, seq, _)) => tokio::select! {
                biased;
                Ok(_) = rx.wait_for(|newest| *newest > *seq) => {
                    return Err(ListErr::new(ListErrKind::Canceled, ""));
                }
                page = pages.next() => page,
            },
            None => pages.next().await,
        };
        let Some(page) = page else { break };
        let resp = page.map_err(|e| classify_remote(&e))?;
        let items = resp.common_prefixes().len() + resp.contents().len();
        guard.page(items, resp.next_continuation_token()).map_err(|stop| {
            let kind = match stop {
                PageStop::TooMany(_) => ListErrKind::TooLarge,
                PageStop::Loop => ListErrKind::Unknown,
            };
            ListErr::new(kind, stop.to_string())
        })?;

        // Subdirectories: "models/loras/" is shown as "loras".
        for cp in resp.common_prefixes() {
            let Some(name) = cp.prefix().and_then(|full| dir_entry_name(&prefix, full)) else {
                continue;
            };
            entries.push(DirEntry {
                name: name.to_string(),
                kind: "dir",
                size: None,
                modified: None,
                glyph: None,
            });
        }

        for o in resp.contents() {
            let Some(key) = o.key() else { continue };
            // An object whose key is the prefix itself is the directory marker, not a file.
            if key == prefix {
                continue;
            }
            let Some(name) = file_entry_name(&prefix, key) else {
                continue;
            };
            entries.push(DirEntry {
                name: name.to_string(),
                kind: "file",
                size: o.size(),
                modified: o.last_modified().map(dt_millis),
                glyph: glyph_for(name),
            });
        }
    }

    sort_entries(&mut entries);
    Ok(entries)
}

/// The name a common prefix shows as in the listing of `prefix`: the part
/// after it, without the trailing slash. `None` for one outside `prefix` or
/// with a slash inside, which only a broken or hostile server returns.
fn dir_entry_name<'a>(prefix: &str, full: &'a str) -> Option<&'a str> {
    let name = full.strip_prefix(prefix)?.trim_end_matches('/');
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

/// The name an object key shows as in the listing of `prefix`; `None` for the
/// same cases as `dir_entry_name`.
fn file_entry_name<'a>(prefix: &str, key: &'a str) -> Option<&'a str> {
    let name = key.strip_prefix(prefix)?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

// ── Local ───────────────────────────────────────────────────────────

fn io_kind(e: &io::Error) -> ListErrKind {
    match e.kind() {
        io::ErrorKind::NotFound => ListErrKind::NotFound,
        io::ErrorKind::PermissionDenied => ListErrKind::AccessDenied,
        _ => ListErrKind::Io,
    }
}

impl From<HomeNotFound> for ListErr {
    fn from(e: HomeNotFound) -> Self {
        ListErr::new(ListErrKind::Unknown, e.to_string())
    }
}

/// Lists one local directory.
///
/// The reading runs on a blocking thread, not on the main one: a directory on a
/// slow network mount can take seconds, and every other command would wait for
/// it, the window's own work included.
#[tauri::command]
pub async fn list_local(path: String) -> Result<Vec<DirEntry>, ListErr> {
    tauri::async_runtime::spawn_blocking(move || list_local_sync(&path))
        .await
        .map_err(|e| ListErr::new(ListErrKind::Unknown, format!("listing task failed: {e}")))?
}

fn list_local_sync(path: &str) -> Result<Vec<DirEntry>, ListErr> {
    let dir = resolve_local(path)?;

    let read = fs::read_dir(&dir)
        .map_err(|e| ListErr::new(io_kind(&e), format!("{}: {e}", dir.display())))?;

    let mut entries: Vec<DirEntry> = Vec::new();
    for item in read {
        let Ok(item) = item else { continue };
        // Symlinks are followed on purpose: the entry's own metadata describes
        // the link itself, so a link to a directory would be listed as a file.
        // A broken or unreadable target falls back to the link's metadata, so
        // the entry still appears.
        let Ok(meta) = fs::metadata(item.path()).or_else(|_| item.metadata()) else {
            continue;
        };
        let name = item.file_name().to_string_lossy().into_owned();
        let is_dir = meta.is_dir();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        entries.push(DirEntry {
            kind: if is_dir { "dir" } else { "file" },
            size: if is_dir { None } else { Some(meta.len() as i64) },
            glyph: if is_dir { None } else { glyph_for(&name) },
            modified,
            name,
        });
    }

    sort_entries(&mut entries);
    Ok(entries)
}

/// What a path dropped onto the window turns out to be.
///
/// The absolute path comes back with the entry because the frontend builds the
/// transfer source from it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStat {
    pub path: String,
    /// The last component of the path.
    pub name: String,
    pub is_dir: bool,
    pub size: Option<i64>,
    pub modified: Option<i64>,
}

/// Stats absolute paths, skipping whatever cannot be read. Runs off the main
/// thread for the same reason as `list_local`.
#[tauri::command]
pub async fn stat_paths(paths: Vec<String>) -> Vec<PathStat> {
    tauri::async_runtime::spawn_blocking(move || stat_paths_sync(paths))
        .await
        .unwrap_or_default()
}

fn stat_paths_sync(paths: Vec<String>) -> Vec<PathStat> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let path = PathBuf::from(&p);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.clone());
        let is_dir = meta.is_dir();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        out.push(PathStat {
            path: p,
            name,
            is_dir,
            size: if is_dir { None } else { Some(meta.len() as i64) },
            modified,
        });
    }
    out
}

#[cfg(test)]
mod name_tests {
    use super::{dir_entry_name, file_entry_name};

    #[test]
    fn a_listing_shows_only_names_directly_under_the_prefix() {
        assert_eq!(dir_entry_name("models/", "models/loras/"), Some("loras"));
        assert_eq!(dir_entry_name("", "models/"), Some("models"));
        assert_eq!(file_entry_name("models/", "models/a.bin"), Some("a.bin"));
        assert_eq!(file_entry_name("", "a.bin"), Some("a.bin"));
        // A server answering outside the prefix, or with a slash inside one
        // name, is not shown: that name would become a path on the way down.
        assert_eq!(dir_entry_name("models/", "other/x/"), None);
        assert_eq!(dir_entry_name("models/", "models/a/b/"), None);
        assert_eq!(dir_entry_name("models/", "models/"), None);
        assert_eq!(file_entry_name("models/", "other"), None);
        assert_eq!(file_entry_name("models/", "models/a/b"), None);
    }
}

#[cfg(test)]
mod lane_tests {
    use super::{LANES, LaneEnd, enter_lane};
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn a_newer_listing_on_the_same_tab_stops_the_older_one() {
        let short = Duration::from_millis(30);
        let mut first = enter_lane("lane-test-a", 1);
        assert!(timeout(short, first.wait_for(|v| *v > 1)).await.is_err(), "nothing newer yet");

        let mut other = enter_lane("lane-test-b", 2);
        let _second = enter_lane("lane-test-a", 3);
        assert!(
            matches!(timeout(short, first.wait_for(|v| *v > 1)).await, Ok(Ok(_))),
            "a newer listing on the same tab"
        );
        assert!(timeout(short, other.wait_for(|v| *v > 2)).await.is_err(), "another tab is not touched");
    }

    // The map keeps a tab only while it has a listing under way.
    #[tokio::test]
    async fn a_tab_is_forgotten_when_its_newest_listing_ends() {
        let has = |tab: &str| LANES.lock().unwrap().contains_key(tab);
        let _first = enter_lane("lane-test-c", 1);
        let _second = enter_lane("lane-test-c", 2);
        drop(LaneEnd("lane-test-c".into(), 1));
        assert!(has("lane-test-c"), "a newer listing still runs");
        drop(LaneEnd("lane-test-c".into(), 2));
        assert!(!has("lane-test-c"));
    }
}
