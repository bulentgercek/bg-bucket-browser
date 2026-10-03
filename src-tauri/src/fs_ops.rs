//! Creating, renaming and deleting, on both sides of the window.
//!
//! A remote folder is only a prefix over keys, so renaming or deleting one is
//! never a single call: every object under the prefix is listed and then copied
//! or deleted, a few requests at a time. Large folders are slow for that reason.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::sync::{LazyLock, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aws_sdk_s3::Client;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::config::active_client;
use crate::core::{PageGuard, PageStop, TREE_MAX_ENTRIES};
use crate::local_path::{HomeNotFound, home, resolve_local};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FsErrKind {
    NotFound,
    AccessDenied,
    AlreadyExists,
    Unsupported,
    Io,
    S3,
    /// The window that asked for it closed first.
    Canceled,
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsErr {
    pub kind: FsErrKind,
    pub detail: String,
}

impl FsErr {
    fn new(kind: FsErrKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// One item of a multi-selection, as the frontend sends it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelItem {
    pub path: String,
    pub is_dir: bool,
}

// ── Shared helpers ───────────────────────────────────────────────

/// The gateway's own code and message, instead of the SDK's bare "service error".
fn s3_detail<E, R>(e: &SdkError<E, R>) -> String
where
    SdkError<E, R>: ProvideErrorMetadata,
{
    let code = e.code().unwrap_or("UnknownCode");
    let msg = e
        .message()
        .map(str::to_string)
        .unwrap_or_else(|| e.to_string());
    format!("{code}: {msg}")
}

async fn remote_client(app: &AppHandle) -> Result<(Client, String), FsErr> {
    active_client(app).map_err(|e| FsErr::new(FsErrKind::Unknown, e.to_string()))
}

fn io_kind(e: &io::Error) -> FsErrKind {
    match e.kind() {
        io::ErrorKind::NotFound => FsErrKind::NotFound,
        io::ErrorKind::PermissionDenied => FsErrKind::AccessDenied,
        io::ErrorKind::AlreadyExists => FsErrKind::AlreadyExists,
        _ => FsErrKind::Io,
    }
}

impl From<HomeNotFound> for FsErr {
    fn from(e: HomeNotFound) -> Self {
        FsErr::new(FsErrKind::Unknown, e.to_string())
    }
}

// A remote path as a prefix: the root stays empty, anything else gets one trailing slash.
// Paths are never trimmed: a name may begin or end with a space, and trimming it
// points at a different object.
fn as_prefix(path: &str) -> String {
    match path.trim_matches('/') {
        "" => String::new(),
        p => format!("{p}/"),
    }
}

/// Refuses paths that would make a destructive operation hit the whole
/// bucket (remote), the home directory or the filesystem root (local).
/// The UI never sends these; this is a second line of defence.
pub fn ensure_not_root(remote: bool, path: &str) -> Result<(), String> {
    let refused = if remote {
        path.trim_matches('/').is_empty()
    } else {
        let p = path;
        if p.is_empty() || p == "~" || p == "~/" {
            true
        } else {
            match resolve_local(p) {
                Ok(resolved) => {
                    let canon = |x: &std::path::Path| fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
                    let r = canon(&resolved);
                    // A pane path is always absolute or under `~`; anything else
                    // would resolve against the working directory.
                    !resolved.has_root()
                        || r.parent().is_none()
                        || home().is_ok_and(|h| canon(&h) == r)
                }
                Err(_) => true,
            }
        }
    };
    if refused {
        Err(format!("refusing to modify a root location: {path:?}"))
    } else {
        Ok(())
    }
}

// A remote path as an object key.
fn as_key(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

// The copy's stored size is checked against the source before the source goes,
// the same check every transfer write passes.
async fn copy_then_delete(
    client: &Client,
    bucket: &str,
    old_key: &str,
    new_key: &str,
) -> Result<(), FsErr> {
    let head = client
        .head_object()
        .bucket(bucket)
        .key(old_key)
        .send()
        .await
        .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
    let size = head.content_length().unwrap_or(0).max(0) as u64;
    // The copy and the delete are bound to the version the HEAD saw: a key
    // written again meanwhile is neither copied half-way through nor deleted.
    let version = head.e_tag().filter(|e| !e.is_empty()).map(str::to_string);
    match client
        .copy_object()
        .bucket(bucket)
        .key(new_key)
        .copy_source(format!("{bucket}/{old_key}"))
        .set_copy_source_if_match(version.clone())
        .send()
        .await
    {
        Ok(o) => {
            let written = o.copy_object_result().and_then(|r| r.e_tag());
            crate::core::verify_written(client, bucket, new_key, size, written)
                .await
                .map_err(|e| FsErr::new(FsErrKind::S3, e))?;
        }
        Err(e) if precondition_failed(&e) => {
            return Err(FsErr::new(
                FsErrKind::S3,
                format!("{old_key}: changed on the volume while it was being renamed; try again"),
            ));
        }
        Err(e) => return Err(FsErr::new(FsErrKind::S3, s3_detail(&e))),
    }
    match client
        .delete_object()
        .bucket(bucket)
        .key(old_key)
        .set_if_match(version)
        .send()
        .await
    {
        Ok(_) => Ok(()),
        // The gateway answers a key already gone with 412 too.
        Err(e) if precondition_failed(&e) && gone(client, bucket, old_key).await => Ok(()),
        Err(e) if precondition_failed(&e) => Err(FsErr::new(
            FsErrKind::S3,
            format!("{old_key}: changed after it was copied to the new name, so it was kept"),
        )),
        Err(e) => Err(FsErr::new(FsErrKind::S3, s3_detail(&e))),
    }
}

/// Whether a key is no longer there.
async fn gone(client: &Client, bucket: &str, key: &str) -> bool {
    client.head_object().bucket(bucket).key(key).send().await.is_err_and(|e| {
        e.raw_response().is_some_and(|r| r.status().as_u16() == 404)
    })
}

/// Whether a request was refused for its condition (`If-Match` and kin): the
/// object is no longer the version the condition named.
fn precondition_failed<E>(e: &SdkError<E, aws_sdk_s3::config::http::HttpResponse>) -> bool {
    e.raw_response().is_some_and(|r| r.status().as_u16() == 412)
}

/// Whether the name is taken, as a file or as a folder.
///
/// The file side is a HEAD, which is strongly consistent; the folder side is the
/// same delimiter listing the panes use, so an empty directory counts as taken
/// too.
async fn remote_name_taken(client: &Client, bucket: &str, key: &str) -> Result<bool, FsErr> {
    if client.head_object().bucket(bucket).key(key).send().await.is_ok() {
        return Ok(true);
    }
    let resp = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(format!("{key}/"))
        .delimiter("/")
        .max_keys(1)
        .send()
        .await
        .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
    Ok(!resp.contents().is_empty() || !resp.common_prefixes().is_empty())
}

/// Deletes everything under a prefix, depth first.
///
/// How many single-object requests a folder operation keeps in flight.
const PARALLEL_REQUESTS: usize = 8;

/// Runs `op` for every key, at most `PARALLEL_REQUESTS` at a time, and calls
/// `on_done` after each one. The first failure stops it; requests already in
/// flight are dropped with it.
async fn for_each_key<F, Fut>(
    keys: &[String],
    op: F,
    on_done: &(dyn Fn() + Send + Sync),
) -> Result<(), FsErr>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), FsErr>> + Send + 'static,
{
    let mut running: JoinSet<Result<(), FsErr>> = JoinSet::new();
    let mut pending = keys.iter();
    loop {
        while running.len() < PARALLEL_REQUESTS {
            let Some(k) = pending.next() else { break };
            running.spawn(op(k.clone()));
        }
        match running.join_next().await {
            Some(res) => {
                res.map_err(|e| FsErr::new(FsErrKind::Unknown, e.to_string()))??;
                on_done();
            }
            None => return Ok(()),
        }
    }
}

/// The order matters: the gateway removes a directory marker only once the
/// directory is empty, so sub-directories go before this level's files, and the
/// marker itself goes last. The files of one level have no order among
/// themselves and go in parallel. The descent walks one level at a time, the same way
/// the panes list, because a flat listing can come back empty here.
///
/// `on_file` is called after each deleted object, for the progress counter.
fn delete_prefix_recursive<'a>(
    client: &'a Client,
    bucket: &'a str,
    prefix: &'a str,
    on_file: &'a (dyn Fn() + Send + Sync),
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), FsErr>> + Send + 'a>> {
    Box::pin(async move {
        too_deep(depth)?;
        let mut files: Vec<String> = Vec::new();
        let mut subdirs: Vec<String> = Vec::new();
        // The SDK's paginator follows the continuation token and stops when it
        // repeats; it never looks at `is_truncated`.
        let mut pages = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .delimiter("/")
            .into_paginator()
            .send();
        let mut guard = PageGuard::new(TREE_MAX_ENTRIES);
        while let Some(page) = pages.next().await {
            let resp = page.map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
            guard_page(&mut guard, &resp)?;
            for cp in resp.common_prefixes() {
                if let Some(p) = cp.prefix().filter(|p| below(prefix, p)) {
                    subdirs.push(p.to_string());
                }
            }
            for o in resp.contents() {
                if let Some(k) = o.key().filter(|k| k.starts_with(prefix)) {
                    files.push(k.to_string());
                }
            }
        }

        for sub in &subdirs {
            delete_prefix_recursive(client, bucket, sub, on_file, depth + 1).await?;
        }
        for_each_key(
            &files,
            |k| {
                let (client, bucket) = (client.clone(), bucket.to_string());
                async move {
                    client
                        .delete_object()
                        .bucket(&bucket)
                        .key(&k)
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))
                }
            },
            on_file,
        )
        .await?;
        // The directory marker itself: it may not exist at all, so failure is ignored.
        let marker = prefix.trim_end_matches('/');
        if !marker.is_empty() {
            let _ = client
                .delete_object()
                .bucket(bucket)
                .key(marker)
                .send()
                .await;
        }
        Ok(())
    })
}

type Tree = (Vec<String>, Vec<String>);

/// Deepest a folder descent goes. Real trees stay far above it; a server that
/// keeps answering with ever deeper folders is stopped before the nested
/// requests run the thread's stack out.
const MAX_TREE_DEPTH: usize = 128;

fn too_deep(depth: usize) -> Result<(), FsErr> {
    if depth > MAX_TREE_DEPTH {
        return Err(FsErr::new(FsErrKind::S3, "the folder tree is deeper than the app follows"));
    }
    Ok(())
}

/// Whether a folder prefix a listing of `prefix` returned lies below it. A
/// broken server can return anything, and a descent must neither leave the
/// folder it was asked for nor go round in it.
fn below(prefix: &str, p: &str) -> bool {
    p.len() > prefix.len() && p.starts_with(prefix)
}

/// Counts a listing page against its guard (`core::PageGuard`).
fn guard_page(
    guard: &mut PageGuard,
    resp: &aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Output,
) -> Result<(), FsErr> {
    let items = resp.common_prefixes().len() + resp.contents().len();
    guard
        .page(items, resp.next_continuation_token())
        .map_err(|stop| FsErr::new(FsErrKind::S3, stop.to_string()))
}

/// All object keys (including `.keep` markers) and all sub-directory prefixes
/// under `prefix`, found by delimiter-based descent (a flat listing can come
/// back empty on the RunPod gateway for deep prefixes). `prefix` itself is not
/// in the directory list.
fn collect_tree_deep<'a>(
    client: &'a Client,
    bucket: &'a str,
    prefix: &'a str,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Tree, FsErr>> + Send + 'a>>
{
    Box::pin(async move {
        too_deep(depth)?;
        let mut out: Vec<String> = Vec::new();
        let mut subdirs: Vec<String> = Vec::new();
        // Paged like `delete_prefix_recursive`.
        let mut pages = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .delimiter("/")
            .into_paginator()
            .send();
        let mut guard = PageGuard::new(TREE_MAX_ENTRIES);
        while let Some(page) = pages.next().await {
            let resp = page.map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
            guard_page(&mut guard, &resp)?;
            for cp in resp.common_prefixes() {
                if let Some(p) = cp.prefix().filter(|p| below(prefix, p)) {
                    subdirs.push(p.to_string());
                }
            }
            for o in resp.contents() {
                if let Some(k) = o.key().filter(|k| k.starts_with(prefix)) {
                    out.push(k.to_string());
                }
            }
        }
        let mut dirs = subdirs.clone();
        for sub in &subdirs {
            let (k, d) = collect_tree_deep(client, bucket, sub, depth + 1).await?;
            out.extend(k);
            dirs.extend(d);
            // The whole tree is held, so its size counts across the levels too.
            if (out.len() + dirs.len()) as u64 > TREE_MAX_ENTRIES {
                return Err(FsErr::new(FsErrKind::S3, PageStop::TooMany(TREE_MAX_ENTRIES).to_string()));
            }
        }
        Ok((out, dirs))
    })
}

// ── Command'lar ─────────────────────────────────────────────────────

/// Rejects a name that would point somewhere other than a new entry inside the
/// parent. `.` and `..` are directory shorthands, not names: the gateway
/// resolves them on its side, so a folder named `..` ends up in the volume root.
fn check_name(name: &str, message: &str) -> Result<(), FsErr> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(FsErr::new(FsErrKind::Unsupported, message));
    }
    Ok(())
}

/// Creates a folder and returns its name.
#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    remote: bool,
    parent: String,
    name: String,
) -> Result<String, FsErr> {
    check_name(&name, "invalid folder name")?;
    if remote {
        let (client, bucket) = remote_client(&app).await?;
        // S3 has no "create directory", and the usual `key/` marker does not
        // work here: the gateway drops the trailing slash and leaves a zero-byte
        // file. A hidden placeholder inside the folder is what makes the prefix
        // exist.
        let key = format!("{}{}/.keep", as_prefix(&parent), name);
        client
            .put_object()
            .bucket(&bucket)
            .key(&key)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
    } else {
        let dir = resolve_local(&parent)?.join(&name);
        fs::create_dir(&dir)
            .map_err(|e| FsErr::new(io_kind(&e), format!("{}: {e}", dir.display())))?;
    }
    Ok(name)
}

/// The recursive size of a folder, for the Properties window.
///
/// Placeholder objects are left out of both the byte count and the file count.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub bytes: i64,
    pub files: i64,
}

/// Folder-size scans in flight, by the id the Properties window gave them.
static SIZE_SCANS: LazyLock<Mutex<HashMap<String, watch::Sender<bool>>>> =
    LazyLock::new(Default::default);

/// Registers a folder-size scan; the receiver turns true when it is called off.
fn begin_size_scan(id: &str) -> watch::Receiver<bool> {
    let mut scans = SIZE_SCANS.lock().unwrap_or_else(|e| e.into_inner());
    scans.entry(id.to_string()).or_insert_with(|| watch::channel(false).0).subscribe()
}

/// Forgets a scan when it ends, however it ends.
struct SizeScanEnd(String);

impl Drop for SizeScanEnd {
    fn drop(&mut self) {
        SIZE_SCANS.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// Calls off a folder-size scan. A call-off that arrives before its scan has
/// started is kept, and the scan stops as soon as it registers.
#[tauri::command]
pub fn cancel_folder_size(scan: String) {
    let mut scans = SIZE_SCANS.lock().unwrap_or_else(|e| e.into_inner());
    scans.entry(scan).or_insert_with(|| watch::channel(false).0).send_replace(true);
}

#[tauri::command]
pub async fn folder_size(
    app: AppHandle,
    remote: bool,
    path: String,
    scan: Option<String>,
) -> Result<FolderSize, FsErr> {
    let canceled = || FsErr::new(FsErrKind::Canceled, "");
    // Without an id the scan cannot be called off: its sender is dropped at once.
    let (mut off, _end) = match scan {
        Some(id) => (begin_size_scan(&id), Some(SizeScanEnd(id))),
        None => (watch::channel(false).1, None),
    };
    if remote {
        let (client, bucket) = remote_client(&app).await?;
        let prefix = as_prefix(&path);
        let mut bytes: i64 = 0;
        let mut files: i64 = 0;
        // The SDK's paginator follows the continuation token and stops when it
        // repeats; it never looks at `is_truncated`.
        let mut pages = client
            .list_objects_v2()
            .bucket(&bucket)
            .prefix(&prefix)
            .into_paginator()
            .send();
        // Only counters are kept, so only a looping token needs stopping.
        let mut guard = PageGuard::new(u64::MAX);
        loop {
            // A call-off drops the page in flight along with its request.
            let page = tokio::select! {
                biased;
                Ok(_) = off.wait_for(|off| *off) => return Err(canceled()),
                page = pages.next() => page,
            };
            let Some(page) = page else { break };
            let resp = page.map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
            guard_page(&mut guard, &resp)?;
            for o in resp.contents() {
                if o.key().is_some_and(|k| k.ends_with("/.keep")) {
                    continue;
                }
                bytes += o.size().unwrap_or(0);
                files += 1;
            }
        }
        Ok(FolderSize { bytes, files })
    } else {
        let root = resolve_local(&path)?;
        let mut bytes: i64 = 0;
        let mut files: i64 = 0;
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            if *off.borrow() {
                return Err(canceled());
            }
            let Ok(rd) = fs::read_dir(&dir) else { continue }; // izin yoksa atla
            for item in rd.flatten() {
                let Ok(meta) = item.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(item.path());
                } else {
                    bytes += meta.len() as i64;
                    files += 1;
                }
            }
        }
        Ok(FolderSize { bytes, files })
    }
}

/// Creates an empty file and returns its name.
#[tauri::command]
pub async fn create_file(
    app: AppHandle,
    remote: bool,
    parent: String,
    name: String,
) -> Result<String, FsErr> {
    check_name(&name, "invalid file name")?;
    if remote {
        let (client, bucket) = remote_client(&app).await?;
        let key = format!("{}{}", as_prefix(&parent), name);
        client
            .put_object()
            .bucket(&bucket)
            .key(&key)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
    } else {
        let path = resolve_local(&parent)?.join(&name);
        if path.exists() {
            return Err(FsErr::new(
                FsErrKind::AlreadyExists,
                path.display().to_string(),
            ));
        }
        fs::File::create(&path)
            .map_err(|e| FsErr::new(io_kind(&e), format!("{}: {e}", path.display())))?;
    }
    Ok(name)
}

/// Renames one item in place; `new_name` is a name, never a path.
///
/// Remote has no rename: a file is copied to the new key and the old one is
/// deleted, and a folder repeats that for every object under it.
#[tauri::command]
pub async fn rename(
    app: AppHandle,
    remote: bool,
    path: String,
    is_dir: bool,
    new_name: String,
) -> Result<String, FsErr> {
    check_name(&new_name, "invalid name")?;
    ensure_not_root(remote, &path).map_err(|e| FsErr::new(FsErrKind::Unsupported, e))?;
    if remote {
        let (client, bucket) = remote_client(&app).await?;
        if is_dir {
            let old_prefix = as_prefix(&path);
            let trimmed = old_prefix.trim_end_matches('/');
            let parent = trimmed
                .rsplit_once('/')
                .map(|(p, _)| format!("{p}/"))
                .unwrap_or_default();
            let new_prefix = format!("{parent}{new_name}/");
            // Renaming to the same name would copy the folder onto itself and
            // then delete it, so it stops here.
            if new_prefix == old_prefix {
                return Ok(new_name);
            }
            // An existing name is refused rather than silently merged into.
            if remote_name_taken(&client, &bucket, &format!("{parent}{new_name}")).await? {
                return Err(FsErr::new(FsErrKind::AlreadyExists, new_prefix));
            }

            // The objects themselves move first, a few at a time: each one is
            // independent of the others.
            let (keys, dirs) = collect_tree_deep(&client, &bucket, &old_prefix, 0).await?;
            for_each_key(
                &keys,
                |k| {
                    let (client, bucket) = (client.clone(), bucket.clone());
                    let nk = format!("{new_prefix}{}", k.strip_prefix(&old_prefix).unwrap_or(&k));
                    async move { copy_then_delete(&client, &bucket, &k, &nk).await }
                },
                &|| {},
            )
            .await?;
            // Sub-folders without any object would otherwise vanish: recreate
            // them under the new name with a marker.
            for d in &dirs {
                if !keys.iter().any(|k| k.starts_with(d.as_str())) {
                    let suffix = d.strip_prefix(&old_prefix).unwrap_or(d);
                    let _ = client
                        .put_object()
                        .bucket(&bucket)
                        .key(format!("{new_prefix}{suffix}.keep"))
                        .body(ByteStream::from_static(b""))
                        .send()
                        .await;
                }
            }

            // The new folder gets a placeholder of its own and the old one is
            // dropped, so an empty source folder still ends up renamed.
            let _ = client
                .put_object()
                .bucket(&bucket)
                .key(format!("{new_prefix}.keep"))
                .body(ByteStream::from_static(b""))
                .send()
                .await;
            let _ = client
                .delete_object()
                .bucket(&bucket)
                .key(format!("{old_prefix}.keep"))
                .send()
                .await;
            // The gateway removes a directory only once it is empty, so old
            // sub-directories go first, deepest first. Only directory entries
            // are deleted here; a folder that still holds files stays.
            let mut old_dirs = dirs;
            old_dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
            for d in &old_dirs {
                let _ = client.delete_object().bucket(&bucket).key(d).send().await;
            }
            let _ = client
                .delete_object()
                .bucket(&bucket)
                .key(&old_prefix)
                .send()
                .await;
            Ok(new_name)
        } else {
            let old_key = as_key(&path);
            let parent = old_key
                .rsplit_once('/')
                .map(|(p, _)| format!("{p}/"))
                .unwrap_or_default();
            let new_key = format!("{parent}{new_name}");
            // Same name: copying onto itself and deleting would destroy the file.
            if new_key == old_key {
                return Ok(new_name);
            }
            // An existing name is refused, the same way the local side refuses it.
            if remote_name_taken(&client, &bucket, &new_key).await? {
                return Err(FsErr::new(FsErrKind::AlreadyExists, new_key));
            }
            copy_then_delete(&client, &bucket, &old_key, &new_key)
                .await
                .map(|()| new_name)
        }
    } else {
        let old = resolve_local(&path)?;
        let new = old
            .parent()
            .map(|p| p.join(&new_name))
            .ok_or_else(|| FsErr::new(FsErrKind::Unsupported, "no parent directory"))?;
        if new.exists() {
            return Err(FsErr::new(
                FsErrKind::AlreadyExists,
                new.display().to_string(),
            ));
        }
        fs::rename(&old, &new)
            .map(|()| new_name)
            .map_err(|e| FsErr::new(io_kind(&e), e.to_string()))
    }
}

/// Deletes a selection.
///
/// Locally this goes to the OS trash unless `permanent` is set; remote deletion
/// is always permanent, because S3 has no trash. Progress is emitted as it goes,
/// so the toast can count.
#[tauri::command]
pub async fn delete(
    app: AppHandle,
    remote: bool,
    permanent: bool,
    items: Vec<DelItem>,
) -> Result<(), FsErr> {
    for it in &items {
        ensure_not_root(remote, &it.path).map_err(|e| FsErr::new(FsErrKind::Unsupported, e))?;
    }
    let total = items.len();
    // Progress counts finished top-level items and, inside a remote folder, the
    // objects deleted so far — otherwise one big folder would look frozen.
    let done_items = AtomicUsize::new(0);
    let files = AtomicUsize::new(0);
    let last_emit = Mutex::new(Instant::now());
    let emit = || {
        let _ = app.emit(
            "delete-progress",
            DelProgress {
                done: done_items.load(Ordering::Relaxed),
                total,
                files: files.load(Ordering::Relaxed),
            },
        );
    };
    let tick = |done: usize| {
        done_items.store(done, Ordering::Relaxed);
        emit();
    };
    let on_file = || {
        files.fetch_add(1, Ordering::Relaxed);
        let mut last = last_emit.lock().unwrap();
        if last.elapsed() >= Duration::from_millis(150) {
            *last = Instant::now();
            emit();
        }
    };

    if remote {
        let (client, bucket) = remote_client(&app).await?;
        for (i, it) in items.iter().enumerate() {
            if it.is_dir {
                delete_prefix_recursive(&client, &bucket, &as_prefix(&it.path), &on_file, 0)
                    .await?;
            } else {
                client
                    .delete_object()
                    .bucket(&bucket)
                    .key(as_key(&it.path))
                    .send()
                    .await
                    .map_err(|e| FsErr::new(FsErrKind::S3, s3_detail(&e)))?;
            }
            tick(i + 1);
        }
    } else if permanent {
        // Permanent: the trash is skipped entirely.
        for (i, it) in items.iter().enumerate() {
            let p = resolve_local(&it.path)?;
            let r = if it.is_dir {
                fs::remove_dir_all(&p)
            } else {
                fs::remove_file(&p)
            };
            r.map_err(|e| FsErr::new(io_kind(&e), format!("{}: {e}", p.display())))?;
            tick(i + 1);
        }
    } else {
        // Default: the OS trash, one item at a time so progress stays live.
        for (i, it) in items.iter().enumerate() {
            let p = resolve_local(&it.path)?;
            trash::delete(&p).map_err(|e| FsErr::new(FsErrKind::Io, e.to_string()))?;
            tick(i + 1);
        }
    }
    Ok(())
}

#[derive(Serialize, Clone)]
struct DelProgress {
    done: usize,
    total: usize,
    /// Objects deleted so far, while a remote folder is being emptied.
    files: usize,
}

#[cfg(test)]
mod root_path_tests {
    use super::{as_prefix, home, resolve_local};

    #[test]
    fn empty_remote_path_targets_whole_bucket() {
        for p in ["", "/", "//"] {
            assert_eq!(as_prefix(p), "", "{p:?}");
        }
        // A space is a name, not the root.
        assert_eq!(as_prefix(" "), " /");
        assert_eq!(as_prefix("models "), "models /");
    }

    #[test]
    fn guard_refuses_remote_root() {
        for p in ["", "/", "//"] {
            assert!(super::ensure_not_root(true, p).is_err(), "{p:?}");
        }
        for p in ["a", "/a/", "Tests/x.txt", " ", "models "] {
            assert!(super::ensure_not_root(true, p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn guard_refuses_local_root_and_home() {
        let h = home().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let with_slash = format!("{hs}/");
        for p in ["", " ", "~", "~/", "/", hs.as_str(), with_slash.as_str()] {
            assert!(super::ensure_not_root(false, p).is_err(), "{p:?}");
        }
        for p in ["~/Downloads", "/tmp", "~/keep "] {
            assert!(super::ensure_not_root(false, p).is_ok(), "{p:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn guard_refuses_windows_drive_root_and_home() {
        let fwd = home().unwrap().to_string_lossy().replace('\\', "/");
        for p in ["C:\\", "C:/", fwd.as_str()] {
            assert!(super::ensure_not_root(false, p).is_err(), "{p:?}");
        }
        for p in ["C:/Windows/Temp", "C:\\Users"] {
            assert!(super::ensure_not_root(false, p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn empty_local_path_targets_home() {
        let h = home().unwrap();
        for p in ["", "~"] {
            assert_eq!(resolve_local(p).unwrap(), h, "{p:?}");
        }
        assert_eq!(resolve_local("~/keep ").unwrap(), h.join("keep "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_names_are_refused_like_empty_and_slashed_ones() {
        for name in ["", ".", "..", "a/b", "/"] {
            assert!(check_name(name, "invalid name").is_err(), "{name:?}");
        }
        for name in ["a", ".keep", "...", "a.b", "..a", "a..", "Memes & Shorts"] {
            assert!(check_name(name, "invalid name").is_ok(), "{name:?}");
        }
    }
}

#[cfg(test)]
mod rename_copy_tests {
    // A stand-in server answers every request; the test checks that a copy
    // stored with the wrong size is removed and the source is left alone.
    use super::copy_then_delete;
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::{Arc, Mutex};

    /// A client whose server reports `copy_size` for the new key and records
    /// every request as "METHOD path".
    fn client_with_copy_size(copy_size: u64) -> (Client, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let path = req.uri().path().to_string();
            let cond = req
                .headers()
                .get("if-match")
                .or_else(|| req.headers().get("x-amz-copy-source-if-match"))
                .and_then(|v| v.to_str().ok())
                .map(|c| format!(" if {c}"))
                .unwrap_or_default();
            log.lock().unwrap().push(format!("{} {path}{cond}", req.method()));
            let resp = http::Response::builder();
            match (req.method().as_str(), path.as_str()) {
                ("HEAD", "/b/old.bin") => resp
                    .status(200)
                    .header("content-length", "100")
                    .header("etag", "\"v1\""),
                ("HEAD", "/b/new.bin") => {
                    resp.status(200).header("content-length", copy_size.to_string())
                }
                ("PUT", _) => resp.status(200),
                _ => resp.status(204),
            }
            .body(match req.method().as_str() {
                "PUT" => SdkBody::from(
                    "<CopyObjectResult><ETag>\"x\"</ETag></CopyObjectResult>",
                ),
                _ => SdkBody::empty(),
            })
            .unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), seen)
    }

    #[tokio::test]
    async fn wrong_size_copy_is_removed_and_the_source_kept() {
        let (client, seen) = client_with_copy_size(50);
        assert!(copy_then_delete(&client, "b", "old.bin", "new.bin").await.is_err());
        let seen = seen.lock().unwrap().clone();
        assert!(seen.iter().any(|r| r.starts_with("DELETE /b/new.bin")), "{seen:?}");
        assert!(!seen.iter().any(|r| r.starts_with("DELETE /b/old.bin")), "{seen:?}");
    }

    #[tokio::test]
    async fn right_size_copy_deletes_the_source() {
        let (client, seen) = client_with_copy_size(100);
        copy_then_delete(&client, "b", "old.bin", "new.bin").await.unwrap();
        let seen = seen.lock().unwrap().clone();
        assert!(seen.iter().any(|r| r.starts_with("DELETE /b/old.bin")), "{seen:?}");
        assert!(!seen.iter().any(|r| r.starts_with("DELETE /b/new.bin")), "{seen:?}");
    }

    // The copy and the delete are both bound to the version the HEAD saw: a
    // key written again meanwhile is neither half copied nor deleted.
    #[tokio::test]
    async fn a_rename_is_bound_to_the_version_it_read() {
        let (client, seen) = client_with_copy_size(100);
        copy_then_delete(&client, "b", "old.bin", "new.bin").await.unwrap();
        let seen = seen.lock().unwrap().clone();
        assert!(seen.contains(&"PUT /b/new.bin if \"v1\"".to_string()), "{seen:?}");
        assert!(seen.contains(&"DELETE /b/old.bin if \"v1\"".to_string()), "{seen:?}");
    }
}

#[cfg(test)]
mod tree_bound_tests {
    // A stand-in server answers folder listings as each test scripts them.
    use super::{MAX_TREE_DEPTH, collect_tree_deep};
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    // A folder answered with itself, or with a folder elsewhere, as a
    // subfolder: the descent stays below the folder it was asked for.
    #[tokio::test]
    async fn a_descent_stays_below_its_folder() {
        let (client, _) = lister(|prefix, _| {
            (vec![format!("{prefix}a.txt"), "elsewhere.txt".into()], vec![prefix.to_string(), "outside/".into()], None)
        });
        let (keys, dirs) = collect_tree_deep(&client, "b", "p/", 0).await.unwrap();
        assert_eq!(keys, ["p/a.txt"]);
        assert!(dirs.is_empty(), "{dirs:?}");
    }

    #[tokio::test]
    async fn a_descent_that_never_ends_is_stopped() {
        let (client, asked) = lister(|prefix, _| (vec![], vec![format!("{prefix}x/")], None));
        assert!(collect_tree_deep(&client, "b", "p/", 0).await.is_err());
        assert!(asked.load(Ordering::SeqCst) <= MAX_TREE_DEPTH + 1);
    }
}

#[cfg(test)]
mod rename_version_tests {
    // A stand-in server where `old.bin` is at "\"v1\"" for the HEAD and then
    // changes under the rename: at the copy, or after it.
    use super::copy_then_delete;
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy)]
    enum Change {
        /// Written again between the HEAD and the copy.
        AtCopy,
        /// Written again between the copy and the delete.
        AfterIt,
        /// Deleted by someone else after the copy.
        Gone,
    }

    fn server(change: Change) -> (Client, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let heads = Arc::new(Mutex::new(0));
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let path = req.uri().path().to_string();
            let method = req.method().as_str().to_string();
            log.lock().unwrap().push(format!("{method} {path}"));
            let copy_cond = req.headers().contains_key("x-amz-copy-source-if-match");
            let del_cond = req.headers().contains_key("if-match");
            let resp = http::Response::builder();
            let (status, len) = match (method.as_str(), path.as_str(), change) {
                ("HEAD", "/b/old.bin", Change::Gone) => {
                    let mut n = heads.lock().unwrap();
                    *n += 1;
                    if *n > 1 { (404, "0") } else { (200, "100") }
                }
                ("HEAD", _, _) => (200, "100"),
                ("PUT", _, Change::AtCopy) if copy_cond => (412, "0"),
                ("PUT", _, _) => (200, "0"),
                ("DELETE", "/b/old.bin", Change::AfterIt | Change::Gone) if del_cond => (412, "0"),
                _ => (204, "0"),
            };
            let body = if method == "PUT" && status == 200 {
                "<CopyObjectResult><ETag>\"v1\"</ETag></CopyObjectResult>"
            } else {
                ""
            };
            resp.status(status).header("content-length", len).header("etag", "\"v1\"").body(SdkBody::from(body)).unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let conf = client.config().to_builder().http_client(http).build();
        (Client::from_conf(conf), seen)
    }

    #[tokio::test]
    async fn a_source_changed_before_the_copy_is_left_alone() {
        let (client, seen) = server(Change::AtCopy);
        assert!(copy_then_delete(&client, "b", "old.bin", "new.bin").await.is_err());
        assert!(!seen.lock().unwrap().iter().any(|r| r.starts_with("DELETE /b/old.bin")));
    }

    #[tokio::test]
    async fn a_source_changed_after_the_copy_is_kept() {
        let (client, _) = server(Change::AfterIt);
        let err = copy_then_delete(&client, "b", "old.bin", "new.bin").await.unwrap_err();
        assert!(err.detail.contains("kept"), "{}", err.detail);
    }

    #[tokio::test]
    async fn a_source_already_gone_is_done() {
        let (client, _) = server(Change::Gone);
        assert!(copy_then_delete(&client, "b", "old.bin", "new.bin").await.is_ok());
    }
}

#[cfg(test)]
mod size_scan_tests {
    use super::{begin_size_scan, cancel_folder_size};
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn closing_the_window_stops_its_scan() {
        let short = Duration::from_millis(30);
        let mut scan = begin_size_scan("size-test-a");
        let mut other = begin_size_scan("size-test-b");
        assert!(timeout(short, scan.wait_for(|off| *off)).await.is_err(), "running");

        cancel_folder_size("size-test-a".to_string());
        assert!(matches!(timeout(short, scan.wait_for(|off| *off)).await, Ok(Ok(_))), "called off");
        assert!(timeout(short, other.wait_for(|off| *off)).await.is_err(), "another scan runs on");
    }

    // The window can close before its scan has registered; the scan then
    // stops as soon as it does.
    #[test]
    fn a_call_off_that_comes_first_is_kept() {
        cancel_folder_size("size-test-early".to_string());
        let scan = begin_size_scan("size-test-early");
        assert!(*scan.borrow(), "already called off");
    }
}
