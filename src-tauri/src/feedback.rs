//! In-app feedback: recording the detailed log for a report, and sending the
//! report to the feedback service.
//!
//! The service stores the report, then mails it to the developer; its
//! contract is described in the development notes. A report is sent from here
//! rather than from the webview, so the page's network policy stays as it is.

use serde::Serialize;
use tauri::AppHandle;

use crate::devlog;

const ENDPOINT: &str = "https://bulentgercek.com/feedback";

// The service accepts up to 1 MiB; the report is kept a little under it.
const BODY_LIMIT: usize = 1_000_000;

/// Development builds send as the service's `test` app: the request is checked
/// and answered like a real one, but nothing is stored or mailed.
const APP_ID: &str = if cfg!(bg_dev) { "test" } else { "bg-bucket-browser" };

/// What the Feedback screen shows of a recording.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub text: String,
    /// Log lines, not counting the start and end markers.
    pub lines: usize,
    pub duration_sec: u64,
    /// Still capturing.
    pub active: bool,
    /// The app went down while recording, so the end marker is missing.
    pub cut_short: bool,
}

/// Why a report was not accepted; the interface words it for the user.
#[derive(Serialize, Debug, PartialEq)]
pub struct FeedbackErr {
    /// `invalid`, `tooLarge`, `rateLimited`, `server` or `network`.
    pub kind: &'static str,
    /// For `invalid`: the field the service rejected, when it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl FeedbackErr {
    fn new(kind: &'static str) -> Self {
        FeedbackErr { kind, field: None }
    }
}

fn is_marker(line: &str) -> bool {
    line.starts_with("=== recording ")
}

/// The time a log line starts with, `YYYY-MM-DD HH:MM:SS.mmm`.
fn stamp_of(line: &str) -> Option<chrono::NaiveDateTime> {
    let body = line.strip_prefix("=== recording start ").unwrap_or(line);
    let body = body.strip_prefix("=== recording end ").unwrap_or(body);
    let s = body.get(..23)?;
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.3f").ok()
}

fn describe(text: String, active: bool) -> RecordingInfo {
    let lines = text.lines().filter(|l| !l.is_empty() && !is_marker(l)).count();
    let start = text.lines().next().and_then(stamp_of);
    let last = text.lines().rev().find_map(stamp_of);
    let duration_sec = match (start, last) {
        (Some(a), Some(b)) => (b - a).num_seconds().max(0) as u64,
        _ => 0,
    };
    let cut_short = !active && !text.lines().any(|l| l.starts_with("=== recording end "));
    RecordingInfo { text, lines, duration_sec, active, cut_short }
}

/// Keeps the newest lines of `text` that fit in `budget` bytes once escaped for
/// JSON; returns the text and how many log lines it holds. The first line (the
/// start marker) is always kept, followed by a note if lines were left out.
fn fit_recording(text: &str, budget: usize) -> (String, usize) {
    let escaped = |s: &str| serde_json::to_string(s).map_or(s.len(), |j| j.len() - 2);
    let all: Vec<&str> = text.lines().collect();
    let Some((head, rest)) = all.split_first() else {
        return (String::new(), 0);
    };
    let mut used = escaped(head) + 2;
    let mut kept: Vec<&str> = Vec::new();
    for line in rest.iter().rev() {
        let cost = escaped(line) + 2; // the line and its escaped newline
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(line);
    }
    kept.reverse();
    let dropped = rest.len() - kept.len();
    let mut out = String::with_capacity(used + 64);
    out.push_str(head);
    out.push('\n');
    if dropped > 0 {
        out.push_str(&format!("… {dropped} earlier lines left out to fit the size limit\n"));
    }
    for line in &kept {
        out.push_str(line);
        out.push('\n');
    }
    let count = kept.iter().filter(|l| !l.is_empty() && !is_marker(l)).count();
    (out, count)
}

/// Maps the service's answer to the result the interface expects.
fn read_answer(status: u16, body: &str) -> Result<String, FeedbackErr> {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let str_field = |k: &str| {
        json.as_ref()
            .and_then(|j| j.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    match status {
        200..=299 => str_field("id").ok_or_else(|| FeedbackErr::new("server")),
        400 => Err(FeedbackErr { kind: "invalid", field: str_field("field") }),
        413 => Err(FeedbackErr::new("tooLarge")),
        429 => Err(FeedbackErr::new("rateLimited")),
        _ => Err(FeedbackErr::new("server")),
    }
}

fn os_version() -> String {
    let v = sysinfo::System::long_os_version().unwrap_or_default();
    v.replace(['\n', '\r'], " ").chars().take(100).collect()
}

/// Starts recording the detailed log for a report.
#[tauri::command]
pub fn feedback_record_start() -> Result<(), String> {
    devlog::recording_start()?;
    devlog::verbose("feedback", "recording started");
    Ok(())
}

/// Stops the recording and returns it.
#[tauri::command]
pub fn feedback_record_stop() -> Option<RecordingInfo> {
    devlog::verbose("feedback", "recording stopped");
    devlog::recording_stop();
    devlog::recording_text().map(|t| describe(t, false))
}

/// The recording waiting to be sent, if there is one: still running, finished,
/// or cut short by a crash. Asked at startup.
#[tauri::command]
pub fn feedback_recording() -> Option<RecordingInfo> {
    devlog::recording_text().map(|t| describe(t, devlog::recording_active()))
}

/// True when reports go to the service's `test` channel.
#[tauri::command]
pub fn feedback_is_test() -> bool {
    APP_ID == "test"
}

/// Throws the recording away.
#[tauri::command]
pub fn feedback_discard_recording() {
    devlog::recording_discard();
}

/// Sends a report and returns the service's reference for it. With
/// `with_recording`, the recording goes along and is deleted once accepted.
#[tauri::command]
pub async fn feedback_send(
    app: AppHandle,
    message: String,
    contact: String,
    with_recording: bool,
) -> Result<String, FeedbackErr> {
    let mut report = serde_json::json!({
        "app": APP_ID,
        "version": app.package_info().version.to_string(),
        "platform": std::env::consts::OS,
        "osVersion": os_version(),
        "message": message,
        "contact": contact.trim(),
    });

    if with_recording {
        devlog::recording_stop();
        if let Some(info) = devlog::recording_text().map(|t| describe(t, false)) {
            report["crashRecovered"] = serde_json::Value::Bool(info.cut_short);
            let base = serde_json::to_vec(&report).map_or(0, |b| b.len());
            let budget = BODY_LIMIT.saturating_sub(base + 200);
            let (text, lines) = fit_recording(&info.text, budget);
            report["recording"] = serde_json::json!({
                "text": text,
                "durationSec": info.duration_sec.min(3600),
                "lines": lines,
            });
        }
    }

    let body = serde_json::to_vec(&report).map_err(|_| FeedbackErr::new("server"))?;
    if body.len() > BODY_LIMIT {
        return Err(FeedbackErr::new("tooLarge"));
    }
    devlog::verbose("feedback", format!("sending {} bytes as {APP_ID}", body.len()));

    // Bounded, so a stalled network cannot leave the Send button waiting forever.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| FeedbackErr::new("network"))?;
    let resp = client
        .post(ENDPOINT)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            devlog::verbose("feedback", format!("network error: {e}"));
            FeedbackErr::new("network")
        })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let answer = read_answer(status, &text);
    devlog::verbose("feedback", format!("answer {status} {answer:?}"));

    if answer.is_ok() && with_recording {
        devlog::recording_discard();
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "=== recording start 2026-09-25 10:00:00.000 v1.1.0 linux ===";

    #[test]
    fn a_recording_that_fits_is_sent_whole() {
        let text = format!("{START}\n2026-09-25 10:00:01.000 [cmd] a\n2026-09-25 10:00:02.000 [cmd] b\n");
        let (out, lines) = fit_recording(&text, 10_000);
        assert_eq!(out, text);
        assert_eq!(lines, 2);
    }

    #[test]
    fn a_long_recording_keeps_its_newest_lines() {
        let mut text = format!("{START}\n");
        for i in 0..1000 {
            text.push_str(&format!("2026-09-25 10:00:01.000 [cmd] line {i:04}\n"));
        }
        let (out, lines) = fit_recording(&text, 2_000);
        assert!(out.starts_with(START), "the start marker stays");
        assert!(out.contains("earlier lines left out"));
        assert!(out.trim_end().ends_with("line 0999"), "the newest line stays");
        assert!(!out.contains("line 0000"));
        assert!(serde_json::to_string(&out).unwrap().len() <= 2_000 + 200);
        assert_eq!(lines, out.lines().filter(|l| l.contains("[cmd]")).count());
    }

    #[test]
    fn duration_and_cut_short() {
        let done = format!(
            "{START}\n2026-09-25 10:01:00.000 [cmd] a\n=== recording end 2026-09-25 10:03:30.000 ===\n"
        );
        let info = describe(done, false);
        assert_eq!(info.duration_sec, 210);
        assert_eq!(info.lines, 1);
        assert!(!info.cut_short);

        let crashed = format!("{START}\n2026-09-25 10:00:40.000 [cmd] a\n");
        let info = describe(crashed, false);
        assert_eq!(info.duration_sec, 40);
        assert!(info.cut_short);

        let running = format!("{START}\n2026-09-25 10:00:40.000 [cmd] a\n");
        assert!(!describe(running, true).cut_short, "a running recording is not cut short");
    }

    #[test]
    fn answers_follow_the_status_code() {
        assert_eq!(read_answer(202, r#"{"id":"3f9a2c1b"}"#), Ok("3f9a2c1b".into()));
        assert_eq!(
            read_answer(400, r#"{"error":"invalid","field":"contact"}"#),
            Err(FeedbackErr { kind: "invalid", field: Some("contact".into()) })
        );
        assert_eq!(read_answer(413, "<html>"), Err(FeedbackErr::new("tooLarge")));
        assert_eq!(read_answer(429, "<html>"), Err(FeedbackErr::new("rateLimited")));
        assert_eq!(read_answer(429, r#"{"error":"rate-limited"}"#), Err(FeedbackErr::new("rateLimited")));
        assert_eq!(read_answer(502, "<html>"), Err(FeedbackErr::new("server")));
        assert_eq!(read_answer(202, "not json"), Err(FeedbackErr::new("server")));
    }

    #[test]
    fn the_build_channel_picks_the_app() {
        // A development checkout sends to the test channel; any other build is real.
        let expected = if cfg!(bg_dev) { "test" } else { "bg-bucket-browser" };
        assert_eq!(APP_ID, expected);
    }
}
