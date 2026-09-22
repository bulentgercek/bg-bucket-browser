//! Volume Cleanup: scans the active bucket, reports where the space goes and which objects look
//! like junk, and permanently deletes the ones the user selects.

use crate::config::active_client;
use aws_sdk_s3::Client;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::task::JoinSet;

/// One object in the "largest" or "reclaimable" list.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub key: String,
    pub size: u64,
}

/// Size and object count of one top-level folder, or of a single file at the bucket root.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FolderStat {
    pub prefix: String,
    /// True for a file at the bucket root, which is listed alongside the folders.
    pub is_file: bool,
    pub bytes: u64,
    pub count: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub total_objects: u64,
    pub total_bytes: u64,
    pub top_folders: Vec<FolderStat>,
    pub largest: Vec<ScanEntry>,
    pub reclaimable: Vec<ScanEntry>,
    /// Folders the scan did not enter; their contents are not in the totals.
    pub skipped: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    scanned: u64,
    bytes: u64,
}

/// Cancel flag shared between `scan_volume` and `cancel_scan`.
#[derive(Clone)]
pub struct CleanupState(Arc<AtomicBool>);

impl CleanupState {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Default for CleanupState {
    fn default() -> Self {
        Self::new()
    }
}

/// Why an object counts as junk, or `None` if it does not.
fn reclaim_reason(key: &str, size: u64) -> Option<&'static str> {
    if key.ends_with("/.keep") || key == ".keep" {
        return None;
    }
    let lower = key.to_lowercase();
    if lower.contains("__pycache__/") || lower.ends_with(".pyc") {
        return Some("__pycache__");
    }
    if lower.contains(".ipynb_checkpoints/") {
        return Some(".ipynb_checkpoints");
    }
    if size == 0 {
        return Some("0-byte");
    }
    None
}

/// Length of the "top folders" and "largest objects" lists.
const TOP_N: usize = 40;

/// Folder names the scan never enters: dependency and version-control trees full of small files
/// that nobody cleans up by hand.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "site-packages"];

/// How many folder listings run at the same time.
const SCAN_CONCURRENCY: usize = 8;

/// A page slower than this is written to the verbose log.
const SLOW_PAGE: Duration = Duration::from_secs(5);

/// How often the scan loop checks for cancellation while listings are in flight.
const CANCEL_POLL: Duration = Duration::from_millis(200);

fn is_skipped_dir(prefix: &str) -> bool {
    let name = prefix.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    SKIP_DIRS.contains(&name)
}

/// Direct contents of one folder: its objects and its subfolder prefixes.
struct DirListing {
    files: Vec<ScanEntry>,
    dirs: Vec<String>,
}

/// Lists one folder, without descending into subfolders, reporting progress after every page.
async fn list_dir(
    client: Client,
    bucket: String,
    prefix: String,
    app: AppHandle,
    scanned: Arc<AtomicU64>,
    scanned_bytes: Arc<AtomicU64>,
) -> Result<DirListing, String> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut req = client.list_objects_v2().bucket(&bucket).delimiter("/");
    if !prefix.is_empty() {
        req = req.prefix(&prefix);
    }
    // The SDK's paginator follows the continuation token and stops when it
    // repeats; it never looks at `is_truncated`.
    let mut pages = req.into_paginator().send();
    loop {
        let page_started = Instant::now();
        let Some(page) = pages.next().await else { break };
        let resp = page.map_err(|e| {
            crate::devlog::verbose("cleanup", format!("list {prefix:?} failed: {e}"));
            e.to_string()
        })?;
        let elapsed = page_started.elapsed();

        let mut page_count: u64 = 0;
        let mut page_bytes: u64 = 0;
        for obj in resp.contents() {
            let key = obj.key().unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            let size = obj.size().unwrap_or(0).max(0) as u64;
            page_count += 1;
            page_bytes += size;
            files.push(ScanEntry { key: key.to_string(), size });
        }
        dirs.extend(
            resp.common_prefixes()
                .iter()
                .filter_map(|p| p.prefix())
                .map(str::to_string),
        );
        if elapsed > SLOW_PAGE {
            crate::devlog::verbose(
                "cleanup",
                format!("slow page {prefix:?}: {page_count} objects in {}ms", elapsed.as_millis()),
            );
        }

        // Counters are shared by all running listings, so the event carries the scan-wide total.
        let _ = app.emit(
            "cleanup-scan-progress",
            ScanProgress {
                scanned: scanned.fetch_add(page_count, Ordering::Relaxed) + page_count,
                bytes: scanned_bytes.fetch_add(page_bytes, Ordering::Relaxed) + page_bytes,
            },
        );
    }
    Ok(DirListing { files, dirs })
}

/// Top-level entry of a key: its first folder, or the key itself for a file at the root.
fn top_level(key: &str) -> &str {
    key.split_once('/').map_or(key, |(first, _)| first)
}

/// Scans the active bucket. Returns `None` when the user cancels.
#[tauri::command]
pub async fn scan_volume(
    app: AppHandle,
    state: tauri::State<'_, CleanupState>,
) -> Result<Option<ScanReport>, String> {
    let cancel = state.0.clone();
    cancel.store(false, Ordering::Relaxed);

    let (client, bucket) = active_client(&app).map_err(|e| e.to_string())?;

    let started = Instant::now();
    let scanned = Arc::new(AtomicU64::new(0));
    let scanned_bytes = Arc::new(AtomicU64::new(0));

    // Folders waiting to be listed start with the bucket root; each listing adds its subfolders.
    let mut pending: VecDeque<String> = VecDeque::from([String::new()]);
    let mut running = JoinSet::new();
    let mut poll = tokio::time::interval(CANCEL_POLL);
    let mut dirs_listed: u64 = 0;

    let mut total_objects: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut all: Vec<ScanEntry> = Vec::new();
    let mut reclaimable: Vec<ScanEntry> = Vec::new();
    let mut folders: HashMap<String, FolderStat> = HashMap::new();
    let mut skipped: Vec<String> = Vec::new();

    loop {
        while running.len() < SCAN_CONCURRENCY {
            let Some(prefix) = pending.pop_front() else { break };
            running.spawn(list_dir(
                client.clone(),
                bucket.clone(),
                prefix,
                app.clone(),
                scanned.clone(),
                scanned_bytes.clone(),
            ));
        }
        if running.is_empty() {
            break;
        }

        // Cancel is checked on a timer, so it does not wait for a slow page to finish.
        let listing = tokio::select! {
            joined = running.join_next() => match joined {
                Some(result) => result.map_err(|e| e.to_string())??,
                None => continue,
            },
            _ = poll.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    running.abort_all();
                    crate::devlog::verbose(
                        "cleanup",
                        format!("scan canceled: {dirs_listed} folders, {total_objects} objects, {}ms", started.elapsed().as_millis()),
                    );
                    return Ok(None);
                }
                continue;
            }
        };
        dirs_listed += 1;

        for dir in listing.dirs {
            if is_skipped_dir(&dir) {
                skipped.push(dir.trim_end_matches('/').to_string());
            } else {
                pending.push_back(dir);
            }
        }
        for entry in listing.files {
            total_objects += 1;
            total_bytes += entry.size;
            let stat = folders
                .entry(top_level(&entry.key).to_string())
                .or_insert_with_key(|k| FolderStat {
                    prefix: k.clone(),
                    is_file: !entry.key.contains('/'),
                    bytes: 0,
                    count: 0,
                });
            stat.bytes += entry.size;
            stat.count += 1;
            if reclaim_reason(&entry.key, entry.size).is_some() {
                reclaimable.push(entry.clone());
            }
            all.push(entry);
        }
    }

    crate::devlog::verbose(
        "cleanup",
        format!(
            "scan done: {dirs_listed} folders, {} skipped, {total_objects} objects, {total_bytes} bytes, {}ms",
            skipped.len(),
            started.elapsed().as_millis()
        ),
    );

    all.sort_by_key(|a| std::cmp::Reverse(a.size));
    all.truncate(TOP_N);
    reclaimable.sort_by_key(|a| std::cmp::Reverse(a.size));

    let mut top_folders: Vec<FolderStat> = folders.into_values().collect();
    top_folders.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    top_folders.truncate(TOP_N);
    skipped.sort();

    Ok(Some(ScanReport {
        total_objects,
        total_bytes,
        top_folders,
        largest: all,
        reclaimable,
        skipped,
    }))
}

/// Stops a running scan; `scan_volume` then returns `None`.
#[tauri::command]
pub fn cancel_scan(state: tauri::State<'_, CleanupState>) {
    state.0.store(true, Ordering::Relaxed);
}

/// Permanently deletes the given keys and returns how many were deleted; failures are skipped.
#[tauri::command]
pub async fn delete_scanned(app: AppHandle, keys: Vec<String>) -> Result<u64, String> {
    let (client, bucket) = active_client(&app).map_err(|e| e.to_string())?;
    let mut done: u64 = 0;
    for key in keys {
        if key.is_empty() {
            continue;
        }
        if client
            .delete_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .is_ok()
        {
            done += 1;
        }
    }
    Ok(done)
}
