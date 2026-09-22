//! Local disks for the sidebar, free space for the status bar, and the home
//! directory path for display.

use std::path::Path;

use serde::Serialize;
use sysinfo::Disks;

use crate::local_path::resolve_local;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub label: String,
    /// Where the local pane navigates when the device is clicked.
    pub mount_path: String,
    /// One of `ssd`, `hdd`, `removable`, `disk`; picks the sidebar icon.
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
}

// Virtual filesystems that are not user storage.
const PSEUDO_FS: &[&str] = &[
    "tmpfs",
    "devtmpfs",
    "squashfs",
    "overlay",
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "autofs",
    "efivarfs",
    "tracefs",
    "debugfs",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "configfs",
    "securityfs",
    "pstore",
    "bpf",
    "fusectl",
    "ramfs",
    "devpts",
];

// Keeps real filesystems mounted where users keep data, on all three platforms.
fn keep(mount: &str, fs: &str) -> bool {
    if PSEUDO_FS.contains(&fs) {
        return false;
    }
    mount == "/"
        || mount.starts_with("/mnt/")
        || mount.starts_with("/media/")
        || mount.starts_with("/run/media/")
        || mount.starts_with("/home/")
        || mount.starts_with("/Volumes/")
        || (mount.len() == 3 && mount.as_bytes().get(1) == Some(&b':'))
}

// The root disk uses its volume name when it has one; others use the last path segment.
fn label_for(mount: &str, disk_name: &str) -> String {
    if mount == "/" {
        if !disk_name.is_empty() {
            return disk_name.to_string();
        }
        return "System".to_string();
    }
    std::path::Path::new(mount)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount.to_string())
}

/// Mounted local disks, filtered and sorted by mount path.
///
/// Reading every mount's size runs off the main thread: a network mount that
/// stopped answering would otherwise hold the whole window.
#[tauri::command]
pub async fn list_devices() -> Vec<Device> {
    tauri::async_runtime::spawn_blocking(list_devices_sync)
        .await
        .unwrap_or_default()
}

fn list_devices_sync() -> Vec<Device> {
    let disks = Disks::new_with_refreshed_list();
    let mut out: Vec<Device> = Vec::new();

    for d in disks.list() {
        let mount = d.mount_point().to_string_lossy().into_owned();
        let fs = d.file_system().to_string_lossy().into_owned();
        if !keep(&mount, &fs) {
            continue;
        }
        let kind = if d.is_removable() {
            "removable"
        } else {
            match d.kind() {
                sysinfo::DiskKind::SSD => "ssd",
                sysinfo::DiskKind::HDD => "hdd",
                _ => "disk",
            }
        };
        out.push(Device {
            label: label_for(&mount, &d.name().to_string_lossy()),
            mount_path: mount,
            kind,
            total: d.total_space(),
            free: d.available_space(),
        });
    }

    // Bind mounts can report the same mount point twice.
    out.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
    out.dedup_by(|a, b| a.mount_path == b.mount_path);
    out
}

/// Free and total space of the filesystem holding a local path.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub label: String,
    pub free: u64,
    pub total: u64,
}

/// Free and total space for the disk that contains `path`, off the main thread
/// like `list_devices`.
#[tauri::command]
pub async fn disk_usage(path: String) -> Result<DiskUsage, String> {
    tauri::async_runtime::spawn_blocking(move || disk_usage_sync(&path))
        .await
        .map_err(|e| format!("disk usage task failed: {e}"))?
}

fn disk_usage_sync(path: &str) -> Result<DiskUsage, String> {
    // An unknown home leaves an empty path, which then matches no disk.
    let target = resolve_local(path).unwrap_or_default();
    // Resolve symlinks first so the path is matched against its real disk.
    //
    // Mount points go through the same resolution, because the two are only
    // comparable in the same form: on Windows a resolved path reads
    // `\\?\C:\Users\…`, which never starts with a raw `C:\`.
    let resolved = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let target = resolved(&target);
    let disks = Disks::new_with_refreshed_list();

    // The deepest mount point containing the path wins.
    let mut best: Option<(usize, DiskUsage)> = None;
    for d in disks.list() {
        let raw = d.mount_point();
        let mp = resolved(raw);
        if !target.starts_with(&mp) {
            continue;
        }
        let len = mp.as_os_str().len();
        if best.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
            best = Some((
                len,
                DiskUsage {
                    // The label comes from the mount point as the system writes
                    // it, not from the resolved form the comparison needs.
                    label: label_for(&raw.to_string_lossy(), &d.name().to_string_lossy()),
                    free: d.available_space(),
                    total: d.total_space(),
                },
            ));
        }
    }
    best.map(|(_, u)| u)
        .ok_or_else(|| "no filesystem for path".to_string())
}

/// The home directory as a native absolute path, for display only.
///
/// Paths are stored and navigated as `~`; this lets the UI show the real path
/// where `~` means nothing, such as on Windows.
#[tauri::command]
pub fn local_home_dir() -> Result<String, String> {
    std::env::home_dir()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.display().to_string())
        .ok_or_else(|| "home directory not found".to_string())
}

#[cfg(test)]
mod disk_usage_tests {
    /// The status bar asks for free space every time a local pane moves, so a
    /// path that exists must always find the filesystem it is on.
    #[test]
    fn home_and_tilde_find_their_filesystem() {
        let home = std::env::home_dir().expect("home directory");
        for p in [home.to_string_lossy().into_owned(), "~".to_string()] {
            assert!(super::disk_usage_sync(&p).is_ok(), "{p:?}");
        }
    }
}
