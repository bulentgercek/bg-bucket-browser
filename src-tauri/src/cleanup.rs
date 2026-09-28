//! Volume Cleanup: scans the active bucket, reports where the space goes and which objects look
//! like junk, and permanently deletes the ones the user selects.

use crate::config::active_client;
use crate::core::PageGuard;
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::task::JoinSet;

/// One object in the "largest" or "reclaimable" list.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanEntry {
    pub key: String,
    pub size: u64,
    /// The version the scan saw; a deletion is bound to it.
    pub etag: String,
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
    /// The largest reclaimable objects, at most `LIST_MAX` of them.
    pub reclaimable: Vec<ScanEntry>,
    /// All reclaimable objects, listed or not.
    pub reclaimable_count: u64,
    pub reclaimable_bytes: u64,
    /// Folders the scan did not enter, at most `LIST_MAX` of them; their
    /// contents are not in the totals.
    pub skipped: Vec<String>,
    pub skipped_count: u64,
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

/// Lists one folder, without descending into subfolders: its objects go into the
/// tally page by page, its subfolder prefixes are returned. Progress is reported
/// after every page.
async fn list_dir(
    client: Client,
    bucket: String,
    prefix: String,
    app: AppHandle,
    scanned: Arc<AtomicU64>,
    scanned_bytes: Arc<AtomicU64>,
    tally: Arc<Mutex<Tally>>,
) -> Result<Vec<String>, String> {
    let mut dirs = Vec::new();
    let mut req = client.list_objects_v2().bucket(&bucket).delimiter("/");
    if !prefix.is_empty() {
        req = req.prefix(&prefix);
    }
    // The SDK's paginator follows the continuation token and stops when it
    // repeats; it never looks at `is_truncated`.
    let mut pages = req.into_paginator().send();
    // The tally holds no entries, so only a looping token needs stopping.
    let mut guard = PageGuard::new(u64::MAX);
    loop {
        let page_started = Instant::now();
        let Some(page) = pages.next().await else { break };
        let resp = page.map_err(|e| {
            crate::devlog::verbose("cleanup", format!("list {prefix:?} failed: {e}"));
            e.to_string()
        })?;
        let elapsed = page_started.elapsed();
        guard
            .page(resp.contents().len(), resp.next_continuation_token())
            .map_err(|stop| stop.to_string())?;

        let mut page_count: u64 = 0;
        let mut page_bytes: u64 = 0;
        {
            let mut tally = tally.lock().unwrap_or_else(|e| e.into_inner());
            for obj in resp.contents() {
                let key = obj.key().unwrap_or_default();
                if key.is_empty() {
                    continue;
                }
                let size = obj.size().unwrap_or(0).max(0) as u64;
                page_count += 1;
                page_bytes += size;
                tally.add_object(key, size, obj.e_tag().unwrap_or_default());
            }
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
    Ok(dirs)
}

/// Length of the "reclaimable" and "not scanned" lists.
const LIST_MAX: usize = 1000;

/// The `cap` largest entries offered so far, by size, as (size, key, ETag).
struct Largest {
    cap: usize,
    heap: BinaryHeap<Reverse<(u64, String, String)>>,
}

impl Largest {
    fn new(cap: usize) -> Self {
        Largest { cap, heap: BinaryHeap::new() }
    }

    fn offer(&mut self, size: u64, key: &str, etag: &str) {
        if self.heap.len() < self.cap {
            self.heap.push(Reverse((size, key.to_string(), etag.to_string())));
        } else if self.heap.peek().is_some_and(|Reverse((min, _, _))| size > *min) {
            self.heap.pop();
            self.heap.push(Reverse((size, key.to_string(), etag.to_string())));
        }
    }

    /// The entries kept, largest first.
    fn into_sorted(self) -> Vec<(u64, String, String)> {
        self.heap.into_sorted_vec().into_iter().map(|Reverse(e)| e).collect()
    }
}

/// The running totals of a scan. However large the volume, only what a report
/// can show is held: the largest objects, the largest reclaimable ones and the
/// first folders not scanned, each up to a fixed count, next to plain counters.
struct Tally {
    total_objects: u64,
    total_bytes: u64,
    largest: Largest,
    reclaimable: Largest,
    reclaimable_count: u64,
    reclaimable_bytes: u64,
    /// Top-level folders; a file at the root is ranked among `root_files` instead.
    folders: HashMap<String, FolderStat>,
    root_files: Largest,
    skipped: Vec<String>,
    skipped_count: u64,
}

impl Tally {
    fn new() -> Self {
        Tally {
            total_objects: 0,
            total_bytes: 0,
            largest: Largest::new(TOP_N),
            reclaimable: Largest::new(LIST_MAX),
            reclaimable_count: 0,
            reclaimable_bytes: 0,
            folders: HashMap::new(),
            root_files: Largest::new(TOP_N),
            skipped: Vec::new(),
            skipped_count: 0,
        }
    }

    fn add_object(&mut self, key: &str, size: u64, etag: &str) {
        self.total_objects += 1;
        self.total_bytes += size;
        match key.split_once('/') {
            Some((top, _)) => {
                if let Some(stat) = self.folders.get_mut(top) {
                    stat.bytes += size;
                    stat.count += 1;
                } else {
                    let stat = FolderStat { prefix: top.to_string(), is_file: false, bytes: size, count: 1 };
                    self.folders.insert(top.to_string(), stat);
                }
            }
            None => self.root_files.offer(size, key, etag),
        }
        if reclaim_reason(key, size).is_some() {
            self.reclaimable_count += 1;
            self.reclaimable_bytes += size;
            self.reclaimable.offer(size, key, etag);
        }
        self.largest.offer(size, key, etag);
    }

    fn add_skipped(&mut self, prefix: &str) {
        self.skipped_count += 1;
        if self.skipped.len() < LIST_MAX {
            self.skipped.push(prefix.trim_end_matches('/').to_string());
        }
    }

    fn report(self) -> ScanReport {
        let entries = |l: Largest| -> Vec<ScanEntry> {
            l.into_sorted().into_iter().map(|(size, key, etag)| ScanEntry { key, size, etag }).collect()
        };
        let root_files = self.root_files.into_sorted().into_iter().map(|(size, key, _)| FolderStat {
            prefix: key,
            is_file: true,
            bytes: size,
            count: 1,
        });
        let mut top_folders: Vec<FolderStat> = self.folders.into_values().chain(root_files).collect();
        top_folders.sort_by_key(|f| Reverse(f.bytes));
        top_folders.truncate(TOP_N);
        let mut skipped = self.skipped;
        skipped.sort();
        ScanReport {
            total_objects: self.total_objects,
            total_bytes: self.total_bytes,
            top_folders,
            largest: entries(self.largest),
            reclaimable: entries(self.reclaimable),
            reclaimable_count: self.reclaimable_count,
            reclaimable_bytes: self.reclaimable_bytes,
            skipped,
            skipped_count: self.skipped_count,
        }
    }
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

    let tally = Arc::new(Mutex::new(Tally::new()));

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
                tally.clone(),
            ));
        }
        if running.is_empty() {
            break;
        }

        // Cancel is checked on a timer, so it does not wait for a slow page to finish.
        let dirs = tokio::select! {
            joined = running.join_next() => match joined {
                Some(result) => result.map_err(|e| e.to_string())??,
                None => continue,
            },
            _ = poll.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    running.abort_all();
                    crate::devlog::verbose(
                        "cleanup",
                        format!(
                            "scan canceled: {dirs_listed} folders, {} objects, {}ms",
                            scanned.load(Ordering::Relaxed),
                            started.elapsed().as_millis()
                        ),
                    );
                    return Ok(None);
                }
                continue;
            }
        };
        dirs_listed += 1;

        for dir in dirs {
            if is_skipped_dir(&dir) {
                tally.lock().unwrap_or_else(|e| e.into_inner()).add_skipped(&dir);
            } else {
                pending.push_back(dir);
            }
        }
    }

    // Every listing has finished, so the tally is no longer shared.
    let report = std::mem::replace(&mut *tally.lock().unwrap_or_else(|e| e.into_inner()), Tally::new()).report();
    crate::devlog::verbose(
        "cleanup",
        format!(
            "scan done: {dirs_listed} folders, {} skipped, {} objects, {} bytes, {}ms",
            report.skipped_count,
            report.total_objects,
            report.total_bytes,
            started.elapsed().as_millis()
        ),
    );
    Ok(Some(report))
}

/// Stops a running scan; `scan_volume` then returns `None`.
#[tauri::command]
pub fn cancel_scan(state: tauri::State<'_, CleanupState>) {
    state.0.store(true, Ordering::Relaxed);
}

/// One object the user picked for deletion, with the version the scan saw.
#[derive(Deserialize)]
pub struct ScanPick {
    key: String,
    etag: String,
}

/// What a deletion did: how many objects went, how many changed since the scan
/// and were kept, and how many could not be deleted.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    deleted: u64,
    changed: u64,
    failed: u64,
}

/// Deletes each pick only while it is still the version the scan saw; one that
/// changed since is kept and counted. Other failures are skipped.
async fn delete_picks(client: &Client, bucket: &str, picks: &[ScanPick]) -> DeleteReport {
    let (mut deleted, mut changed, mut failed) = (0, 0, 0);
    let status = |e: &aws_sdk_s3::error::SdkError<_, aws_sdk_s3::config::http::HttpResponse>| {
        e.raw_response().map(|r| r.status().as_u16())
    };
    for p in picks {
        if p.key.is_empty() {
            continue;
        }
        let version = (!p.etag.is_empty()).then(|| p.etag.clone());
        match client.delete_object().bucket(bucket).key(&p.key).set_if_match(version).send().await {
            Ok(_) => deleted += 1,
            // The gateway answers a key already gone with 412 too.
            Err(e) if status(&e) == Some(412) => {
                let head = client.head_object().bucket(bucket).key(&p.key).send().await;
                if head.is_err_and(|e| e.raw_response().is_some_and(|r| r.status().as_u16() == 404)) {
                    deleted += 1;
                } else {
                    changed += 1;
                }
            }
            Err(_) => failed += 1,
        }
    }
    DeleteReport { deleted, changed, failed }
}

/// Permanently deletes the objects the user picked, each only in the version
/// the scan saw.
#[tauri::command]
pub async fn delete_scanned(app: AppHandle, picks: Vec<ScanPick>) -> Result<DeleteReport, String> {
    let (client, bucket) = active_client(&app).map_err(|e| e.to_string())?;
    Ok(delete_picks(&client, &bucket, &picks).await)
}

#[cfg(test)]
mod tally_tests {
    use super::{LIST_MAX, TOP_N, Tally};

    #[test]
    fn a_huge_volume_keeps_only_what_the_report_shows() {
        let mut tally = Tally::new();
        for i in 0..20_000u64 {
            tally.add_object(&format!("junk/empty-{i}"), 0, "\"e\""); // reclaimable: 0 bytes
            tally.add_object(&format!("root-{i}.bin"), i, "\"e\""); // a file at the root
        }
        for i in 0..3000 {
            tally.add_skipped(&format!("proj-{i}/node_modules/"));
        }
        let report = tally.report();
        assert_eq!(report.total_objects, 40_000);
        assert_eq!(report.reclaimable.len(), LIST_MAX);
        assert_eq!(report.reclaimable_count, 20_001); // root-0.bin is 0 bytes too
        assert_eq!(report.skipped.len(), LIST_MAX);
        assert_eq!(report.skipped_count, 3000);
        assert_eq!(report.largest.len(), TOP_N);
        assert_eq!(report.largest[0].size, 19_999, "the largest come first");
        assert_eq!(report.top_folders.len(), TOP_N);
    }

    #[test]
    fn the_reclaimable_kept_are_the_largest() {
        let mut tally = Tally::new();
        for i in 0..5000u64 {
            tally.add_object(&format!("x/__pycache__/{i}.pyc"), i, "\"e\"");
        }
        let report = tally.report();
        assert_eq!(report.reclaimable.len(), LIST_MAX);
        assert_eq!(report.reclaimable[0].size, 4999, "largest first");
        assert!(report.reclaimable.iter().all(|e| e.size >= 5000 - LIST_MAX as u64));
        assert_eq!(report.reclaimable_bytes, (0..5000u64).sum::<u64>());
    }

    #[test]
    fn each_entry_keeps_its_own_version() {
        let mut tally = Tally::new();
        for i in 0..100u64 {
            tally.add_object(&format!("x/f{i}.bin"), i, &format!("\"e{i}\""));
        }
        let report = tally.report();
        assert!(report.largest.iter().all(|e| e.etag == format!("\"e{}\"", e.size)));
    }

    #[test]
    fn folders_add_up_and_root_files_stand_alone() {
        let mut tally = Tally::new();
        tally.add_object("a/x", 10, "\"e\"");
        tally.add_object("a/deep/y", 5, "\"e\"");
        tally.add_object("b/z", 1, "\"e\"");
        tally.add_object("root.bin", 100, "\"e\"");
        let report = tally.report();
        let row = |p: &str| report.top_folders.iter().find(|f| f.prefix == p).unwrap();
        assert!(row("root.bin").is_file && row("root.bin").bytes == 100);
        assert!(!row("a").is_file && row("a").bytes == 15 && row("a").count == 2);
        assert_eq!(report.top_folders[0].prefix, "root.bin");
        assert_eq!(report.total_bytes, 116);
    }
}

#[cfg(test)]
mod delete_tests {
    // A stand-in server keeps every object at ETag "\"v1\"" and answers a
    // delete for any other version with 412, as the RunPod gateway does.
    use super::{DeleteReport, ScanPick, delete_picks};
    use aws_sdk_s3::Client;
    use aws_sdk_s3::primitives::SdkBody;
    use aws_smithy_http_client::test_util::infallible_client_fn;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn a_cleanup_deletes_only_the_version_it_scanned() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let http = infallible_client_fn(move |req: http::Request<SdkBody>| {
            let cond = req.headers().get("if-match").and_then(|v| v.to_str().ok()).map(str::to_string);
            let path = req.uri().path().to_string();
            if req.method() == "DELETE" {
                log.lock().unwrap().push(cond.clone());
            }
            let status = match req.method().as_str() {
                _ if path.contains("broken") => 500,
                "HEAD" if path.contains("gone") => 404,
                "HEAD" => 200,
                _ if cond.as_deref().is_some_and(|c| c != "\"v1\"") => 412,
                _ => 204,
            };
            http::Response::builder().status(status).body(SdkBody::empty()).unwrap()
        });
        let (client, _) = crate::core::s3_client_from_parts(
            "https://gateway.test", "eu-ro-1", "b", "AKIDTEST", "not-a-secret",
        );
        let client = Client::from_conf(client.config().to_builder().http_client(http).build());
        let picks = [
            ScanPick { key: "same.bin".into(), etag: "\"v1\"".into() },
            ScanPick { key: "filled-since.bin".into(), etag: "\"v0\"".into() },
            // The gateway answers a key already gone with 412 too: that one went.
            ScanPick { key: "gone.bin".into(), etag: "\"v0\"".into() },
            ScanPick { key: "broken.bin".into(), etag: "\"v1\"".into() },
        ];
        assert_eq!(delete_picks(&client, "b", &picks).await, DeleteReport { deleted: 2, changed: 1, failed: 1 });
        assert!(seen.lock().unwrap().iter().all(Option::is_some), "every delete carries its version");
    }
}
