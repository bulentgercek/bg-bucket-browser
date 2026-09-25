//! Development log with two branches, never shown in the UI.
//!
//! - `toast.log`: every notification shown to the user. Always on. Each launch
//!   starts a new file and keeps the previous one as `toast.log.1`.
//! - `verbose.log`: command calls and internal detail. Rotates to
//!   `verbose.log.1` past a size cap.
//!
//! Each session opens with `=== session ... ===` and, on a normal exit, closes
//! with `=== session end ... ===`. A missing end marker means the process was
//! killed or crashed.
//!
//! Files live in the platform app log directory:
//! Linux `~/.local/share/com.bulentgercek.bgbucketbrowser/logs/`,
//! Windows `%LOCALAPPDATA%\com.bulentgercek.bgbucketbrowser\logs\`,
//! macOS `~/Library/Logs/com.bulentgercek.bgbucketbrowser/`.
//!
//! Verbose is on by default in development builds and off otherwise.
//! `BGBB_LOG=verbose` turns it on, `BGBB_LOG=quiet` turns it off.
//!
//! A third file, `recording.log`, exists only while the user records a problem
//! for a feedback report (Settings → Feedback). For that time every verbose
//! line is written there too, whatever the verbose setting is. It opens with
//! `=== recording start ... ===` and closes with `=== recording end ... ===`;
//! a missing end means the app went down while recording. It stays until the
//! report is sent or discarded.
//!
//! Never pass credentials or keys to this module.

use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};

// Verbose rotates past this size; one backup is kept.
const VERBOSE_MAX_BYTES: u64 = 5 * 1024 * 1024;

// A recording stops by itself after this long, or past this size.
pub const RECORDING_MAX_SECS: u64 = 5 * 60;
const RECORDING_MAX_BYTES: u64 = 16 * 1024 * 1024;
const RECORDING_FILE: &str = "recording.log";

// Checked on every verbose call without taking the lock.
static VERBOSE_ON: AtomicBool = AtomicBool::new(false);
// True while a feedback recording is capturing; verbose lines are then built even when the branch is off.
static RECORDING_ON: AtomicBool = AtomicBool::new(false);
// Set once at startup; until then every call is a no-op.
static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

/// One log file plus the state needed to collapse repeated lines.
struct Sink {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
    last: Option<String>,
    repeats: u32,
    first_repeat: Option<String>,
    last_repeat: Option<String>,
}

impl Sink {
    fn open(path: PathBuf) -> Self {
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        let bytes = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Sink { path, file, bytes, last: None, repeats: 0, first_repeat: None, last_repeat: None }
    }

    fn write(&mut self, stamp: &str, body: &str) {
        // Identical consecutive lines are counted instead of repeated, and the
        // count keeps the first and last time they happened: a burst and a slow
        // drip produce the same number otherwise.
        if self.last.as_deref() == Some(body) {
            self.repeats += 1;
            self.first_repeat.get_or_insert_with(|| stamp.to_owned());
            self.last_repeat = Some(stamp.to_owned());
            return;
        }
        self.flush_repeats();
        self.raw(&format!("{stamp} {body}\n"));
        self.last = Some(body.to_owned());
    }

    fn flush_repeats(&mut self) {
        if self.repeats > 0 {
            let n = self.repeats;
            self.repeats = 0;
            let first = self.first_repeat.take().unwrap_or_default();
            let last = self.last_repeat.take().unwrap_or_default();
            // A single repeat has no span worth printing; it happened once.
            let when = if n == 1 { last } else { format!("{first} - {last}") };
            self.raw(&format!("    (x{n} more, {when})\n"));
        }
    }

    // Write failures are ignored: logging must never break the app.
    fn raw(&mut self, line: &str) {
        if let Some(f) = self.file.as_mut()
            && f.write_all(line.as_bytes()).is_ok()
        {
            self.bytes += line.len() as u64;
        }
    }

    /// Moves the file to `<name>.1` and starts an empty one.
    fn rotate(&mut self) {
        self.file = None;
        let _ = fs::rename(&self.path, backup_path(&self.path));
        *self = Sink::open(self.path.clone());
    }
}

struct Logger {
    dir: PathBuf,
    toast: Sink,
    verbose: Option<Sink>,
    recording: Option<(Sink, std::time::Instant)>,
}

fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

fn now_time() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

fn now_full() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

fn one_line(text: &str) -> String {
    text.replace("\r\n", " | ").replace(['\n', '\r'], " | ")
}

fn channel_default() -> bool {
    cfg!(bg_dev)
}

// `BGBB_LOG` overrides the build default; unknown values are ignored.
fn verbose_from_env(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("verbose") => true,
        Some(v) if v.eq_ignore_ascii_case("quiet") => false,
        _ => default,
    }
}

/// Opens the log files. Called once from `setup`; records before this are dropped.
pub fn init(app: &AppHandle) {
    let Ok(dir) = app.path().app_log_dir() else { return };
    init_in(&dir, verbose_from_env(std::env::var("BGBB_LOG").ok().as_deref(), channel_default()));
}

fn init_in(dir: &Path, verbose: bool) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // toast.log holds only the current session; the previous one becomes `.1`.
    let toast_path = dir.join("toast.log");
    if toast_path.exists() {
        let _ = fs::rename(&toast_path, backup_path(&toast_path));
    }
    let mut toast = Sink::open(toast_path);

    let mut verbose_sink = verbose.then(|| Sink::open(dir.join("verbose.log")));
    if let Some(v) = verbose_sink.as_mut()
        && v.bytes > VERBOSE_MAX_BYTES
    {
        v.rotate();
    }

    // Session header: time, version, OS, channel and verbose state.
    let header = format!(
        "=== session {} v{} {} {} verbose={} ===",
        now_full(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        if cfg!(bg_dev) { "dev" } else { "user" },
        if verbose { "on" } else { "off" },
    );
    toast.raw(&format!("{header}\n"));
    if let Some(v) = verbose_sink.as_mut() {
        v.raw(&format!("\n{header}\n"));
    }

    VERBOSE_ON.store(verbose, Ordering::Relaxed);
    let _ = LOGGER.set(Mutex::new(Logger {
        dir: dir.to_path_buf(),
        toast,
        verbose: verbose_sink,
        recording: None,
    }));
}

/// True when the verbose branch is recording. Check it before building costly messages.
pub fn verbose_enabled() -> bool {
    VERBOSE_ON.load(Ordering::Relaxed) || RECORDING_ON.load(Ordering::Relaxed)
}

fn with_logger(f: impl FnOnce(&mut Logger)) {
    if let Some(m) = LOGGER.get()
        && let Ok(mut l) = m.lock()
    {
        f(&mut l);
    }
}

fn write_verbose(l: &mut Logger, area: &str, msg: &str) {
    let stamp = now_full();
    let line = format!("[{area}] {}", one_line(msg));
    if let Some(v) = l.verbose.as_mut() {
        if v.bytes > VERBOSE_MAX_BYTES {
            v.flush_repeats();
            v.rotate();
        }
        v.write(&stamp, &line);
    }
    let over = match l.recording.as_mut() {
        Some((r, started)) => {
            r.write(&stamp, &line);
            started.elapsed().as_secs() >= RECORDING_MAX_SECS || r.bytes > RECORDING_MAX_BYTES
        }
        None => false,
    };
    if over {
        end_recording(l);
    }
}

fn recording_path(l: &Logger) -> PathBuf {
    l.dir.join(RECORDING_FILE)
}

fn end_recording(l: &mut Logger) {
    if let Some((mut r, _)) = l.recording.take() {
        r.flush_repeats();
        r.raw(&format!("=== recording end {} ===\n", now_full()));
    }
    RECORDING_ON.store(false, Ordering::Relaxed);
}

/// Starts a feedback recording, replacing any earlier one that was not sent.
pub fn recording_start() -> Result<(), String> {
    let mut result = Err("the log is not available".to_string());
    with_logger(|l| {
        end_recording(l);
        let path = recording_path(l);
        let _ = fs::remove_file(&path);
        let mut sink = Sink::open(path);
        if sink.file.is_none() {
            result = Err("could not open the recording file".into());
            return;
        }
        sink.raw(&format!(
            "=== recording start {} v{} {} ===\n",
            now_full(),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
        ));
        l.recording = Some((sink, std::time::Instant::now()));
        RECORDING_ON.store(true, Ordering::Relaxed);
        result = Ok(());
    });
    result
}

/// Ends the running recording, if any. The file stays until it is sent or discarded.
pub fn recording_stop() {
    with_logger(end_recording);
}

/// True while a recording is capturing.
pub fn recording_active() -> bool {
    RECORDING_ON.load(Ordering::Relaxed)
}

/// The recording file's contents, running, finished or cut short; `None` when there is none.
pub fn recording_text() -> Option<String> {
    let mut out = None;
    with_logger(|l| {
        if let Some((r, _)) = l.recording.as_mut() {
            r.flush_repeats();
        }
        out = fs::read_to_string(recording_path(l)).ok();
    });
    out
}

/// Deletes the recording, ending it first if it is still running.
pub fn recording_discard() {
    with_logger(|l| {
        end_recording(l);
        let _ = fs::remove_file(recording_path(l));
    });
}

/// Records a notification shown to the user.
pub fn toast(tone: &str, text: &str) {
    let text = one_line(text);
    with_logger(|l| {
        l.toast.write(&now_time(), &format!("{tone:<5} {text}"));
        write_verbose(l, "toast", &format!("{tone} {text}"));
    });
}

/// Records internal detail in the verbose branch; does nothing when it is off.
pub fn verbose(area: &str, msg: impl Display) {
    if !verbose_enabled() {
        return;
    }
    let msg = msg.to_string();
    with_logger(|l| write_verbose(l, area, &msg));
}

/// Writes pending repeat counts and the `session end` marker. Call on a normal exit.
pub fn flush() {
    let footer = format!("=== session end {} ===\n", now_full());
    with_logger(|l| {
        l.toast.flush_repeats();
        l.toast.raw(&footer);
        if let Some(v) = l.verbose.as_mut() {
            v.flush_repeats();
            v.raw(&footer);
        }
        // Closing the app while recording ends the recording normally; it is
        // offered for sending on the next start.
        end_recording(l);
    });
}

/// Frontend entry for the toast branch.
#[tauri::command]
pub fn devlog_toast(tone: String, text: String) {
    toast(&tone, &text);
}

/// Frontend entry for the verbose branch.
#[tauri::command]
pub fn devlog_verbose(area: String, msg: String) {
    verbose(&area, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_channel_default() {
        assert!(verbose_from_env(Some("verbose"), false));
        assert!(verbose_from_env(Some(" VERBOSE "), false));
        assert!(!verbose_from_env(Some("quiet"), true));
        assert!(verbose_from_env(None, true));
        assert!(!verbose_from_env(Some("other"), false));
    }

    #[test]
    fn repeats_are_collapsed() {
        let dir = std::env::temp_dir().join(format!("bgbb-devlog-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        let mut s = Sink::open(path.clone());
        s.write("t1", "info a");
        s.write("t2", "info a");
        s.write("t3", "info a");
        s.write("t4", "info b");
        s.flush_repeats();
        let out = fs::read_to_string(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out, "t1 info a\n    (x2 more, t2 - t3)\nt4 info b\n");
    }

    /// A single repeat prints the one time it happened, not a span of itself.
    #[test]
    fn a_single_repeat_prints_one_time() {
        let dir = std::env::temp_dir().join(format!("bgbb-devlog-once-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        let mut s = Sink::open(path.clone());
        s.write("t1", "info a");
        s.write("t2", "info a");
        s.flush_repeats();
        let out = fs::read_to_string(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out, "t1 info a\n    (x1 more, t2)\n");
    }

    #[test]
    fn one_line_joins_newlines() {
        assert_eq!(one_line("a\nb\r\nc"), "a | b | c");
    }
}
