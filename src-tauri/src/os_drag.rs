//! Dragging files from the app window out to the OS file manager (Linux only).
//!
//! Prototype: the command exists but no part of the UI calls it yet.

use drag::{DragItem, Image, Options};
use std::path::PathBuf;

use crate::local_path::resolve_local;

/// Starts a native drag of the given local files from the app window.
#[tauri::command]
pub fn start_os_drag(window: tauri::WebviewWindow, paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Err("empty file list".to_string());
    }
    let files: Vec<PathBuf> = paths.iter().map(|p| resolve_local(p).unwrap_or_default()).collect();
    // drag-rs requires a preview image; the first file itself is used.
    let icon = Image::File(files[0].clone());
    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
    drag::start_drag(
        &gtk_window,
        DragItem::Files(files),
        icon,
        |_result, _cursor_pos| {},
        Options::default(),
    )
    .map_err(|e| e.to_string())
}
