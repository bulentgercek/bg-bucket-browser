//! Directory listing for both sides: the network volume over S3
//! `ListObjectsV2`, and the local disk over `std::fs`.
//!
//! Both commands return the same entry shape, so a pane renders a remote and a
//! local directory with the same code.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
use aws_sdk_s3::primitives::DateTime;
use serde::Serialize;

use crate::config::active_client;
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
) -> Result<Vec<DirEntry>, ListErr> {
    let (client, bucket) = active_client(&app)
        .map_err(|e| ListErr::new(ListErrKind::Unknown, e.to_string()))?;

    // The root is the empty prefix; anything else carries exactly one trailing slash.
    let prefix = match path.trim().trim_matches('/') {
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
    while let Some(page) = pages.next().await {
        let resp = page.map_err(|e| classify_remote(&e))?;

        // Subdirectories: "models/loras/" is shown as "loras".
        for cp in resp.common_prefixes() {
            let Some(full) = cp.prefix() else { continue };
            let name = full.strip_prefix(&prefix).unwrap_or(full).trim_end_matches('/');
            if name.is_empty() {
                continue;
            }
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
            let name = key.strip_prefix(&prefix).unwrap_or(key);
            if name.is_empty() || name.contains('/') {
                continue;
            }
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
