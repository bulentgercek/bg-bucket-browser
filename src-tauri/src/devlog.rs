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
//! A third file, `feedback.log`, exists only while the user logs a problem
//! for a feedback report (Settings → Feedback). For that time every verbose
//! line is written there too, whatever the verbose setting is. It opens with
//! `=== log start ... ===` and closes with `=== log end ... ===`;
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
// The user reads this file before sending it, and its folder is documented:
// the name and the markers say "log", the word the interface uses.
const RECORDING_FILE: &str = "feedback.log";
/// The first words of the line that opens a recording.
pub const RECORDING_START: &str = "=== log start ";
/// The first words of the line that closes a recording.
pub const RECORDING_END: &str = "=== log end ";
// The file's name up to 1.2.0.
const LEGACY_RECORDING_FILE: &str = "recording.log";

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
    scrub(&text.replace("\r\n", " | ").replace(['\n', '\r'], " | "))
}

/// What a credential is introduced by in a signature header, a signed URL or
/// an S3 error body; the word after one of these is never written.
const KEY_MARKERS: &[&str] = &[
    "Credential=",
    "Signature=",
    "<AWSAccessKeyId>",
    "AWSAccessKeyId=",
];

/// Takes what looks like a credential out of a line before it is written: the
/// word after a marker above, and any word shaped like an access key.
///
/// The app never logs a key itself. This is for text it does not write: an
/// error message is the server's, and a server may repeat back what it was
/// sent.
fn scrub(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(word) {
        let (before, from) = rest.split_at(start);
        let end = from.find(|c| !word(c)).unwrap_or(from.len());
        let (run, after) = from.split_at(end);
        out.push_str(before);
        let marked = KEY_MARKERS.iter().any(|m| out.ends_with(m));
        out.push_str(if marked || key_shaped(run) { "***" } else { run });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// True for a word shaped like an access key: an AWS key id, or one of
/// RunPod's prefixed keys.
fn key_shaped(word: &str) -> bool {
    let alnum = |s: &str, min: usize| s.len() >= min && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if let Some(tail) = word.strip_prefix("AKIA").or_else(|| word.strip_prefix("ASIA")) {
        return tail.len() == 16 && tail.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    }
    ["rps_", "rpa_", "user_"].iter().any(|p| word.strip_prefix(p).is_some_and(|tail| alnum(tail, 20)))
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
    adopt_legacy_recording(dir);
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
        r.raw(&format!("{RECORDING_END}{} ===\n", now_full()));
    }
    RECORDING_ON.store(false, Ordering::Relaxed);
}

/// Starts a feedback recording, replacing any earlier one that was not sent.
pub fn recording_start() -> Result<(), String> {
    let mut result = Err("the log is not available".to_string());
    with_logger(|l| result = start_recording(l));
    result
}

fn start_recording(l: &mut Logger) -> Result<(), String> {
    end_recording(l);
    let path = recording_path(l);
    let _ = fs::remove_file(&path);
    let mut sink = Sink::open(path);
    if sink.file.is_none() {
        return Err("could not open the feedback log".into());
    }
    sink.raw(&format!(
        "{RECORDING_START}{} v{} {} ===\n",
        now_full(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    ));
    l.recording = Some((sink, std::time::Instant::now()));
    RECORDING_ON.store(true, Ordering::Relaxed);
    Ok(())
}

/// Takes over a recording left on disk under the name the file had up to
/// 1.2.0, so it is still offered for sending and still goes away with its
/// report. Where a recording already exists under today's name, or the old
/// name is not a plain file, the old one is only removed.
fn adopt_legacy_recording(dir: &Path) {
    let old = dir.join(LEGACY_RECORDING_FILE);
    let Ok(meta) = fs::symlink_metadata(&old) else { return };
    let new = dir.join(RECORDING_FILE);
    let free = fs::symlink_metadata(&new).is_err();
    if !(meta.is_file() && free && fs::rename(&old, &new).is_ok()) {
        let _ = fs::remove_file(&old);
    }
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
    fn what_looks_like_a_credential_is_not_written() {
        let line = "InvalidAccessKeyId: <AWSAccessKeyId>myKey123</AWSAccessKeyId> not found";
        assert_eq!(scrub(line), "InvalidAccessKeyId: <AWSAccessKeyId>***</AWSAccessKeyId> not found");
        let line = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261002/eu-ro-1/s3/aws4_request, SignedHeaders=host, Signature=0123abcd";
        assert_eq!(
            scrub(line),
            "AWS4-HMAC-SHA256 Credential=***/20261002/eu-ro-1/s3/aws4_request, SignedHeaders=host, Signature=***"
        );
        assert_eq!(scrub("key AKIAIOSFODNN7EXAMPLE was refused"), "key *** was refused");
        assert_eq!(scrub("token rps_ABCDEFGHIJKLMNOPQRSTUVWX1234 echoed"), "token *** echoed");
        assert_eq!(scrub("user_2aBcDeFgHiJkLmNoPqRsTuVw and rpa_ABCDEFGHIJ0123456789XYZ"), "*** and ***");
    }

    #[test]
    fn ordinary_names_are_left_alone() {
        for line in [
            "list models/user_presets/rps_notes.txt",
            "[cmd] ← list_remote ok 203ms",
            "user_data.json and AKIA.txt",
            "upload 0123456789abcdef0123456789abcdef.safetensors",
        ] {
            assert_eq!(scrub(line), line);
        }
    }

    #[test]
    fn one_line_joins_newlines() {
        assert_eq!(one_line("a\nb\r\nc"), "a | b | c");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bgbb-devlog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The file the user reads before sending calls itself a log, in its name
    /// and in the lines that open and close it.
    #[test]
    fn a_feedback_log_is_named_and_marked_as_a_log() {
        let dir = temp_dir("marks");
        let mut l = Logger {
            dir: dir.clone(),
            toast: Sink::open(dir.join("toast.log")),
            verbose: None,
            recording: None,
        };
        start_recording(&mut l).unwrap();
        write_verbose(&mut l, "cmd", "→ list_local");
        end_recording(&mut l);
        let text = fs::read_to_string(dir.join("feedback.log"));
        fs::remove_dir_all(&dir).unwrap();
        let text = text.expect("the log is written to feedback.log");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("=== log start "), "{text}");
        assert!(lines[2].starts_with("=== log end "), "{text}");
        assert!(!text.to_lowercase().contains("record"), "{text}");
    }

    /// A log left by a version that named the file `recording.log` is taken
    /// over, so it is offered again and goes away with its report.
    #[test]
    fn a_log_under_the_old_name_is_taken_over() {
        let dir = temp_dir("legacy");
        fs::write(dir.join("recording.log"), "old\n").unwrap();
        adopt_legacy_recording(&dir);
        let old_left = dir.join("recording.log").exists();
        let taken = fs::read_to_string(dir.join("feedback.log")).ok();
        fs::remove_dir_all(&dir).unwrap();
        assert!(!old_left, "nothing stays under the old name");
        assert_eq!(taken.as_deref(), Some("old\n"));
    }

    /// With a log under both names the newer one stays as it is.
    #[test]
    fn a_newer_log_wins_over_one_under_the_old_name() {
        let dir = temp_dir("legacy-both");
        fs::write(dir.join("recording.log"), "old\n").unwrap();
        fs::write(dir.join("feedback.log"), "new\n").unwrap();
        adopt_legacy_recording(&dir);
        let old_left = dir.join("recording.log").exists();
        let kept = fs::read_to_string(dir.join("feedback.log")).ok();
        fs::remove_dir_all(&dir).unwrap();
        assert!(!old_left, "nothing stays under the old name");
        assert_eq!(kept.as_deref(), Some("new\n"));
    }

    /// A link under the old name is removed, never followed: its target is
    /// neither taken over as a log nor deleted.
    #[cfg(unix)]
    #[test]
    fn a_link_under_the_old_name_is_removed_not_followed() {
        let dir = temp_dir("legacy-link");
        let target = dir.join("elsewhere.txt");
        fs::write(&target, "private\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("recording.log")).unwrap();
        adopt_legacy_recording(&dir);
        let link_left = fs::symlink_metadata(dir.join("recording.log")).is_ok();
        let taken = fs::symlink_metadata(dir.join("feedback.log")).is_ok();
        let target_text = fs::read_to_string(&target).ok();
        fs::remove_dir_all(&dir).unwrap();
        assert!(!link_left, "the link itself goes");
        assert!(!taken, "a link is not taken over as a log");
        assert_eq!(target_text.as_deref(), Some("private\n"), "its target is untouched");
    }
}
