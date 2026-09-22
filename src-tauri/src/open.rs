//! Opens local files, folders and links in the operating system's default app.
//!
//! Remote objects are opened elsewhere (`transfers::open_remote_start`): they
//! are downloaded to a cache first, then opened from there.

use crate::local_path::resolve_local;

/// Opens a local file or folder in its default application.
///
/// Errors are short codes the frontend translates: `remoteNotSupported`,
/// `notFound`, otherwise the OS error text.
#[tauri::command]
pub fn open_path(side: String, path: String) -> Result<(), String> {
    if side != "local" {
        return Err("remoteNotSupported".into());
    }
    let target = resolve_local(&path)?;
    if !target.exists() {
        return Err("notFound".into());
    }
    // Returns as soon as the app is launched, so the UI is not blocked.
    opener::open(&target).map_err(|e| e.to_string())
}

/// Opens a link in the default browser or mail client.
///
/// Only `https:`, `http:` and `mailto:` are accepted.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    let ok = ["https://", "http://", "mailto:"]
        .iter()
        .any(|p| url.starts_with(p));
    if !ok {
        return Err("unsupportedScheme".into());
    }
    opener::open(&url).map_err(|e| e.to_string())
}
