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
    use std::time::{Duration, Instant};
    use percent_encoding::{percent_encode, AsciiSet, CONTROLS};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use wl_clipboard_rs::copy::{self, MimeSource, MimeType as CopyMimeType, Options, Source};
    use wl_clipboard_rs::paste::{get_contents, get_mime_types, ClipboardType, MimeType, Seat};

    /// How long the owner gets to hand over the file list, and how large it may
    /// be: about 150 000 paths.
    const LIST_TIMEOUT: Duration = Duration::from_secs(2);
    const LIST_CAP: usize = 16 * 1024 * 1024;

    /// What came out of the owner's pipe.
    #[derive(Debug, PartialEq)]
    pub(super) enum Piped {
        /// Everything, up to the owner closing the pipe.
        Whole(Vec<u8>),
        /// More than the cap; the rest was read and dropped.
        TooLarge,
        /// The owner did not finish by the deadline.
        NoAnswer,
    }

    /// Reads what the owner writes into the pipe until it closes it, giving up
    /// at `deadline`, and keeping no more than `cap` bytes: an owner that never
    /// answers, or never stops, cannot hold the reader or fill memory. Past the
    /// cap the pipe is still read to its end, because an owner whose reader
    /// leaves early may give up its offer altogether.
    pub(super) fn read_pipe<R: std::io::Read + std::os::fd::AsRawFd>(
        mut pipe: R,
        deadline: Instant,
        cap: usize,
    ) -> Piped {
        let fd = pipe.as_raw_fd();
        // SAFETY: plain flag calls on a descriptor this function owns through `pipe`.
        let nonblocking = unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
        };
        if !nonblocking {
            return Piped::NoAnswer;
        }
        let mut out = Vec::new();
        let mut too_large = false;
        let mut buf = [0u8; 8192];
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return if too_large { Piped::TooLarge } else { Piped::NoAnswer };
            };
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid `pollfd`, and the count says so.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Piped::NoAnswer;
            }
            if ready == 0 {
                continue; // the deadline is checked at the top
            }
            match pipe.read(&mut buf) {
                Ok(0) if too_large => return Piped::TooLarge,
                Ok(0) => return Piped::Whole(out),
                Ok(_) if too_large => {}
                Ok(n) if out.len() + n > cap => {
                    too_large = true;
                    out = Vec::new();
                }
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Piped::NoAnswer,
            }
        }
    }

    /// The local paths in a `text/uri-list` body: one URI a line, CRLF or LF,
    /// `#` starts a comment, and only a `file:` URI of this machine is a local
    /// path. A path that is not UTF-8 is left out, as arboard did.
    pub(super) fn parse_uri_list(body: &[u8]) -> Vec<PathBuf> {
        body.split(|&b| b == b'\n')
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
            .filter_map(|line| {
                let rest = line.strip_prefix(b"file://")?;
                // `file:///path`, or `file://localhost/path`; another host is not this machine.
                let path = if rest.starts_with(b"/") { rest } else { rest.strip_prefix(b"localhost")? };
                if !path.starts_with(b"/") {
                    return None;
                }
                let bytes: Vec<u8> = percent_encoding::percent_decode(path).collect();
                String::from_utf8(bytes).ok().map(PathBuf::from)
            })
            .collect()
    }

    /// Why the Wayland reader could not be used: the compositor offers no
    /// data-control, or there is no Wayland to talk to.
    pub(super) struct NoDataControl;

    /// The file list on the clipboard, read with a deadline and a size cap.
    /// An empty clipboard and one holding something else read as an empty
    /// list; `None` when the owner did not answer in time or the list was over
    /// the cap, which says nothing about what the clipboard holds.
    pub(super) fn file_list() -> Result<Option<Vec<PathBuf>>, NoDataControl> {
        use wl_clipboard_rs::paste::Error;
        let started = Instant::now();
        let got = with_timeout(LIST_TIMEOUT, || {
            get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Specific("text/uri-list"))
        });
        match got {
            Some(Ok((pipe, _))) => match read_pipe(pipe, started + LIST_TIMEOUT, LIST_CAP) {
                Piped::Whole(body) => Ok(Some(parse_uri_list(&body))),
                Piped::TooLarge | Piped::NoAnswer => Ok(None),
            },
            Some(Err(Error::ClipboardEmpty | Error::NoMimeType | Error::NoSeats)) => Ok(Some(Vec::new())),
            None => Ok(None),
            Some(Err(_)) => Err(NoDataControl),
        }
    }

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
            let Some(Ok((pipe, _))) = with_timeout(TIMEOUT, || {
                get_contents(
                    ClipboardType::Regular,
                    Seat::Unspecified,
                    MimeType::Specific(GNOME_FILES_MARKER),
                )
            }) else {
                return false;
            };
            // Read with the same deadline and cap as the file list: an owner
            // that never answers, or never stops, reads as "copy".
            let Piped::Whole(buf) = read_pipe(pipe, Instant::now() + TIMEOUT, LIST_CAP) else {
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
    /// `false` when the clipboard could not be read in time: an owner that did
    /// not answer, a list over the size limit, another read still running.
    /// What it holds is then unknown, not empty.
    pub known: bool,
}

impl OsClipboardState {
    const UNKNOWN: Self = OsClipboardState { paths: Vec::new(), cut: false, known: false };
}

/// Runs `f` on a throwaway thread and gives up after `timeout`.
///
/// A clipboard request can be answered by the window that asks it: when we own
/// the clipboard, the poll waits on ourselves and never returns. The timeout
/// bounds that to one late poll instead of a frozen app.
///
/// One helper runs at a time. While one still waits on an owner that never
/// answers, the next call gives up at once instead of leaving another thread
/// behind; "no answer" already reads as a copy.
#[cfg(target_os = "linux")]
fn with_timeout<T: Send + 'static>(
    timeout: std::time::Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    // Clears the flag when the helper ends, a panic included.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }

    if BUSY.swap(true, Ordering::AcqRel) {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().spawn(move || {
        // The flag clears before the answer goes out, so a caller holding the
        // answer can take the next step at once.
        let out = {
            let _done = Done;
            f()
        };
        let _ = tx.send(out);
    });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
        return None;
    }
    rx.recv_timeout(timeout).ok()
}

/// Returns the file paths sitting on the OS clipboard and whether they were cut.
///
/// An empty clipboard and one holding something else (text, an image) are an
/// empty list rather than an error: the caller reads it as "nothing to paste".
/// A clipboard that could not be read (no clipboard service, an owner that did
/// not answer, another program holding it) comes back with `known: false`.
///
/// Clipboard work happens on a worker thread. Handled synchronously it would run
/// on the GTK main thread, and a clipboard we own ourselves would deadlock the
/// whole app there.
#[tauri::command]
pub async fn read_os_clipboard_files() -> OsClipboardState {
    tauri::async_runtime::spawn_blocking(read_os_clipboard_files_sync)
        .await
        .unwrap_or(OsClipboardState::UNKNOWN)
}

/// The file list on the OS clipboard, or `None` when it could not be read in
/// time. In a Wayland session with data-control it is read directly, with a
/// deadline and a size cap; elsewhere through `arboard`, whose X11 reader has
/// its own timeout (`arboard_answer`).
fn file_list() -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && let Ok(list) = wayland::file_list()
    {
        return list;
    }
    let read = || {
        let started = std::time::Instant::now();
        let list = arboard::Clipboard::new().and_then(|mut cb| cb.get().file_list());
        (list, started.elapsed())
    };
    let (mut list, mut took) = read();
    // A quick "no file list" is asked once more: on X11 a large list arrives in
    // pieces, and a piece that comes late reads the same.
    if matches!(list, Err(arboard::Error::ContentNotAvailable)) && took < ARBOARD_GAVE_UP {
        (list, took) = read();
    }
    arboard_answer(list, took)
}

/// An arboard read that fails only after this long has run into its own 4 s
/// timeout on X11: the owner did not answer.
const ARBOARD_GAVE_UP: std::time::Duration = std::time::Duration::from_millis(3500);

/// What an arboard read says about the clipboard: its file list, an empty
/// clipboard, or nothing (`None`). Only "no file list" means empty, and only
/// when it comes at once: arboard reports running out of time with the same
/// error. Any other failure, another program holding the clipboard among them,
/// says nothing about what the clipboard holds.
fn arboard_answer(
    read: Result<Vec<PathBuf>, arboard::Error>,
    took: std::time::Duration,
) -> Option<Vec<PathBuf>> {
    match read {
        Ok(list) => Some(list),
        Err(arboard::Error::ContentNotAvailable) if took < ARBOARD_GAVE_UP => Some(Vec::new()),
        Err(_) => None,
    }
}

fn read_os_clipboard_files_sync() -> OsClipboardState {
    // One read at a time, whoever asks: the cut check makes several requests in
    // a row and must not meet another reader's helper thread halfway.
    static READING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_one) = READING.try_lock() else {
        return OsClipboardState::UNKNOWN;
    };
    let Some(list) = file_list() else {
        return OsClipboardState::UNKNOWN;
    };
    let paths: Vec<String> = list
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
    OsClipboardState { paths, cut, known: true }
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

#[cfg(all(test, target_os = "linux"))]
mod timeout_tests {
    use super::with_timeout;
    use std::sync::mpsc;
    use std::time::Duration;

    // An owner that never answers holds one helper thread; the calls that come
    // meanwhile give up at once instead of leaving more threads behind. All in
    // one test, because the gate is shared by the whole process.
    #[test]
    fn one_helper_thread_at_a_time() {
        let (entered_tx, entered) = mpsc::channel::<()>();
        let (release, stuck) = mpsc::channel::<()>();
        let first = with_timeout(Duration::from_millis(50), move || {
            let _ = entered_tx.send(());
            let _ = stuck.recv();
        });
        assert!(first.is_none(), "the stuck call times out");
        entered.recv_timeout(Duration::from_secs(5)).expect("the first helper ran");

        // While it is stuck, even a call that would answer at once gives up.
        for i in 0..5 {
            assert_eq!(with_timeout(Duration::from_secs(5), move || i), None, "no second helper");
        }

        // Once the owner answers, calls run again, each right after the last:
        // the gate is open by the time an answer arrives.
        release.send(()).unwrap();
        let back = (0..500).find_map(|_| {
            std::thread::sleep(Duration::from_millis(10));
            with_timeout(Duration::from_secs(5), || 7)
        });
        assert_eq!(back, Some(7));
        for i in 0..1000 {
            assert_eq!(with_timeout(Duration::from_secs(5), move || i), Some(i), "call {i}");
        }

        // A helper that panics opens the gate too.
        assert_eq!(with_timeout(Duration::from_secs(5), || -> u8 { panic!("owner gone") }), None);
        assert_eq!(with_timeout(Duration::from_secs(5), || 1), Some(1));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod wayland_read_tests {
    use super::wayland::{Piped, parse_uri_list, read_pipe};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Runs the read on its own thread, so a read that never returns fails the
    /// test instead of hanging it.
    fn read_within(
        pipe: std::io::PipeReader,
        deadline: Duration,
        cap: usize,
        wait: Duration,
    ) -> Result<Piped, &'static str> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_pipe(pipe, Instant::now() + deadline, cap));
        });
        rx.recv_timeout(wait).map_err(|_| "the read is still waiting")
    }

    #[test]
    fn an_owner_that_never_answers_is_given_up() {
        let (r, w) = std::io::pipe().unwrap();
        let started = Instant::now();
        let got = read_within(r, Duration::from_millis(100), 1024, Duration::from_secs(3));
        assert_eq!(got, Ok(Piped::NoAnswer), "given up at the deadline");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(w);
    }

    // An answer over the cap is read to its end and dropped, without waiting
    // for the deadline; one that never ends is dropped at the deadline.
    #[test]
    fn an_answer_over_the_cap_is_dropped() {
        // More than the pipe holds, so a reader that stopped at the cap would
        // leave the writer with a broken pipe.
        let (r, mut w) = std::io::pipe().unwrap();
        let (sent_tx, sent) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent_tx.send(w.write_all(&vec![b'x'; 200 * 1024]).is_ok());
        });
        let started = Instant::now();
        let got = read_within(r, Duration::from_secs(5), 64 * 1024, Duration::from_secs(8));
        assert_eq!(got, Ok(Piped::TooLarge));
        assert!(started.elapsed() < Duration::from_secs(2), "read to its end, not to the deadline");
        assert_eq!(sent.recv_timeout(Duration::from_secs(5)), Ok(true), "the owner could send it all");

        let (r, mut w) = std::io::pipe().unwrap();
        std::thread::spawn(move || while w.write_all(&[b'x'; 4096]).is_ok() {});
        let got = read_within(r, Duration::from_millis(300), 64 * 1024, Duration::from_secs(5));
        assert_eq!(got, Ok(Piped::TooLarge), "an owner that never stops");
    }

    #[test]
    fn a_normal_answer_is_read_whole() {
        let (r, mut w) = std::io::pipe().unwrap();
        w.write_all(b"file:///tmp/a\r\n").unwrap();
        drop(w);
        let got = read_within(r, Duration::from_secs(5), 1024, Duration::from_secs(8));
        assert_eq!(got, Ok(Piped::Whole(b"file:///tmp/a\r\n".to_vec())));
    }

    #[test]
    fn a_uri_list_gives_its_local_paths() {
        let body = b"# a comment\r\nfile:///home/u/a%20b.txt\r\nfile://localhost/tmp/c\r\nhttps://example.com/x\r\nfile://otherhost/d\r\n\r\n";
        assert_eq!(
            parse_uri_list(body),
            vec![PathBuf::from("/home/u/a b.txt"), PathBuf::from("/tmp/c")]
        );
        assert_eq!(parse_uri_list(b"file:///tmp/last"), vec![PathBuf::from("/tmp/last")]);
    }

    // Only `file:///` and `file://localhost/` name this machine; a path that is
    // not UTF-8 is left out, as a transfer leaves out such a name (N18).
    #[test]
    fn a_uri_list_leaves_out_other_hosts_and_non_utf8_paths() {
        assert!(parse_uri_list(b"file://localhostfoo/x").is_empty());
        assert!(parse_uri_list(b"file://localhost.example/x").is_empty());
        assert!(parse_uri_list(b"file:///tmp/bad%FF.txt").is_empty());
    }
}

#[cfg(test)]
mod arboard_answer_tests {
    use super::arboard_answer;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn a_read_that_ran_out_of_time_is_unknown() {
        let nothing = || Err(arboard::Error::ContentNotAvailable);
        assert_eq!(arboard_answer(nothing(), Duration::from_millis(20)), Some(Vec::new()), "no file list");
        assert_eq!(arboard_answer(nothing(), Duration::from_secs(4)), None, "no answer in time");
        let list = vec![PathBuf::from("/a")];
        assert_eq!(arboard_answer(Ok(list.clone()), Duration::from_secs(4)), Some(list));
    }

    // Only "no file list" means an empty clipboard. Another program holding
    // the clipboard (Windows answers that within ~25 ms) says nothing about it.
    #[test]
    fn a_busy_clipboard_is_unknown() {
        let quick = Duration::from_millis(25);
        assert_eq!(arboard_answer(Err(arboard::Error::ClipboardOccupied), quick), None);
        assert_eq!(arboard_answer(Err(arboard::Error::ClipboardNotSupported), quick), None);
    }
}
