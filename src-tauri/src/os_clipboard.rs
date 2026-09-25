//! Reads the operating system clipboard as a list of file paths, and writes the
//! app's own Copy/Cut back to it.
//!
//! `arboard` carries the file list on every platform. What it does not carry is
//! the cut/copy distinction: that is read and written through Wayland's
//! data-control protocol, so on X11, Windows and macOS everything is a copy.

use serde::Serialize;
use std::path::PathBuf;

// The cut marker travels as extra MIME types next to the file list, and only
// the Wayland data-control protocol exposes them.
#[cfg(target_os = "linux")]
mod wayland {
    use super::with_timeout;
    use std::time::Duration;
    use percent_encoding::{percent_encode, AsciiSet, CONTROLS};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use wl_clipboard_rs::copy::{self, MimeSource, MimeType as CopyMimeType, Options, Source};
    use wl_clipboard_rs::paste::{get_contents, get_mime_types, ClipboardType, MimeType, Seat};

    const KDE_CUT_MARKER: &str = "application/x-kde-cutselection";
    const GNOME_FILES_MARKER: &str = "x-special/gnome-copied-files";

    /// Reports whether the clipboard carries a cut (Move) rather than a copy.
    ///
    /// KDE marks a cut by offering its MIME type at all, so the payload is never
    /// read; GNOME always offers its type and puts the verb in the first line.
    /// Anything unreadable answers "copy", which is the safe default.
    pub(super) fn is_cut() -> bool {
        const TIMEOUT: Duration = Duration::from_millis(300);
        let Some(Ok(types)) =
            with_timeout(TIMEOUT, || get_mime_types(ClipboardType::Regular, Seat::Unspecified))
        else {
            return false;
        };
        if types.contains(KDE_CUT_MARKER) {
            return true;
        }
        if types.contains(GNOME_FILES_MARKER) {
            let Some(Ok((mut pipe, _))) = with_timeout(TIMEOUT, || {
                get_contents(
                    ClipboardType::Regular,
                    Seat::Unspecified,
                    MimeType::Specific(GNOME_FILES_MARKER),
                )
            }) else {
                return false;
            };
            let Some(Ok(buf)) = with_timeout(TIMEOUT, move || {
                let mut buf = Vec::new();
                use std::io::Read;
                pipe.read_to_end(&mut buf).map(|_| buf)
            }) else {
                return false;
            };
            return buf.starts_with(b"cut");
        }
        false
    }

    /// Characters a `text/uri-list` entry must percent-encode.
    const URI_ENCODE_SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');

    fn paths_to_uri_list(paths: &[String], resolve_home: impl Fn(&str) -> PathBuf) -> String {
        paths
            .iter()
            .map(|p| resolve_home(p))
            .map(|p| format!("file://{}", percent_encode(p.as_os_str().as_bytes(), URI_ENCODE_SET)))
            .collect::<Vec<_>>()
            // Entries are separated by CRLF, as the format requires.
            .join("\r\n")
    }

    /// Offers the file list and, for a cut, both desktops' markers from a single
    /// clipboard source.
    ///
    /// Fails when the compositor has no data-control support; the caller then
    /// falls back to `arboard`.
    pub(super) fn copy(
        paths: &[String],
        cut: bool,
        resolve_home: impl Fn(&str) -> PathBuf,
    ) -> Result<(), copy::Error> {
        let uri_list = paths_to_uri_list(paths, resolve_home);
        let mut sources = vec![MimeSource {
            source: Source::Bytes(uri_list.clone().into_bytes().into_boxed_slice()),
            mime_type: CopyMimeType::Specific("text/uri-list".to_string()),
        }];
        if cut {
            // KDE reads the marker's presence, GNOME reads its content, so both go out.
            sources.push(MimeSource {
                source: Source::Bytes(Box::new([])),
                mime_type: CopyMimeType::Specific(KDE_CUT_MARKER.to_string()),
            });
            sources.push(MimeSource {
                source: Source::Bytes(format!("cut\n{uri_list}").into_bytes().into_boxed_slice()),
                mime_type: CopyMimeType::Specific(GNOME_FILES_MARKER.to_string()),
            });
        }
        Options::new().copy_multi(sources)
    }
}

// Elsewhere the cut marker can be neither read nor written, so the answer is
// always "copy" and the file list travels through `arboard` alone.
#[cfg(target_os = "linux")]
fn detect_cut() -> bool {
    wayland::is_cut()
}
#[cfg(not(target_os = "linux"))]
fn detect_cut() -> bool {
    false
}

// The Wayland path is tried only in a Wayland session; an X11 session goes
// straight to the fallback.
#[cfg(target_os = "linux")]
fn try_wayland_copy(paths: &[String], cut: bool) -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        && wayland::copy(paths, cut, resolve_home).is_ok()
}
#[cfg(not(target_os = "linux"))]
fn try_wayland_copy(_paths: &[String], _cut: bool) -> bool {
    false
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsClipboardState {
    pub paths: Vec<String>,
    /// `true` only when a cut was actually detected. A copy and an undetectable
    /// clipboard look the same here.
    pub cut: bool,
}

/// Runs `f` on a throwaway thread and gives up after `timeout`.
///
/// A clipboard request can be answered by the window that asks it: when we own
/// the clipboard, the poll waits on ourselves and never returns. The timeout
/// bounds that to one late poll instead of a frozen app.
#[cfg(target_os = "linux")]
fn with_timeout<T: Send + 'static>(
    timeout: std::time::Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

/// Returns the file paths sitting on the OS clipboard and whether they were cut.
///
/// An empty clipboard, a clipboard holding something else (text, an image) and a
/// machine with no clipboard service at all are the same empty list rather than
/// an error: the caller reads it as "nothing to paste".
///
/// Clipboard work happens on a worker thread. Handled synchronously it would run
/// on the GTK main thread, and a clipboard we own ourselves would deadlock the
/// whole app there.
#[tauri::command]
pub async fn read_os_clipboard_files() -> OsClipboardState {
    tauri::async_runtime::spawn_blocking(read_os_clipboard_files_sync)
        .await
        .unwrap_or(OsClipboardState { paths: Vec::new(), cut: false })
}

fn read_os_clipboard_files_sync() -> OsClipboardState {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return OsClipboardState { paths: Vec::new(), cut: false };
    };
    let paths: Vec<String> = cb
        .get()
        .file_list()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            // arboard's Linux parser splits the uri-list on `\n` alone, leaving
            // a `\r` at the end of every path.
            let s = p
                .to_string_lossy()
                .trim_end_matches(['\r', '\n'])
                .to_string();
            (!s.is_empty()).then_some(s)
        })
        .collect();
    // Asking about cut costs a round trip, so it is skipped for an empty list.
    let cut = !paths.is_empty() && detect_cut();
    OsClipboardState { paths, cut }
}

// ── The other direction: the app's own Copy/Cut → the OS clipboard ─────────────

// Expands `~` against the home directory; other paths are used as given.
fn resolve_home(path: &str) -> PathBuf {
    let p = path;
    let home = || std::env::home_dir().unwrap_or_default();
    if p.is_empty() || p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}

/// Writes local paths to the OS clipboard so other file managers can paste them.
///
/// Returns whether the cut was honoured: a plain copy always was, a cut only
/// when a real marker could be offered. `false` means an outside paste will copy
/// instead of move; the in-app Move is unaffected either way.
#[tauri::command]
pub async fn write_os_clipboard_files(paths: Vec<String>, cut: bool) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || write_os_clipboard_files_sync(&paths, cut))
        .await
        .map_err(|e| e.to_string())?
}

fn write_os_clipboard_files_sync(paths: &[String], cut: bool) -> Result<bool, String> {
    if paths.is_empty() {
        return Err("empty file list".to_string());
    }
    // Wayland first: it is the only path that can carry a cut.
    let wayland = try_wayland_copy(paths, cut);
    if wayland {
        return Ok(true);
    }
    let resolved: Vec<PathBuf> = paths.iter().map(|p| resolve_home(p)).collect();
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set().file_list(&resolved).map_err(|e| e.to_string())?;
    // The fallback can only offer the file list, so a requested cut went out as a copy.
    Ok(!cut)
}
