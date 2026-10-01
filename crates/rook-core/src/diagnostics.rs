//! Local, bounded support reports. Conversation payloads are excluded by default.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use rook_store::{EventKind, Kind, NewEvent, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CoreError, Result, Rook};

const TIMING_LABEL: &str = "rook:timing:v1";
const MAX_REPORT: usize = 1024 * 1024;
const MAX_TIMING: u64 = 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    ModelRequest,
    CheckRequest,
    CompactionRequest,
    ToolDispatch,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Completed,
    Failed,
    Cancelled,
}
#[derive(Debug, Serialize, Deserialize)]
struct Timing {
    phase: Phase,
    status: Status,
    duration_ms: u64,
    result_seq: Option<u64>,
}

/// Cancellation drops the future before ordinary completion code can run.
/// A drop receipt says cancelled, never that the operation itself was undone.
pub(crate) struct Timer {
    store: Arc<Store>,
    session: u128,
    phase: Phase,
    started: tokio::time::Instant,
    finished: bool,
}
impl Timer {
    pub(crate) fn start(rook: &Rook, session: u128, phase: Phase) -> Self {
        Self {
            store: rook.store.clone(),
            session,
            phase,
            started: tokio::time::Instant::now(),
            finished: false,
        }
    }
    pub(crate) fn finish(&mut self, status: Status, result_seq: Option<u64>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let sample = Timing {
            phase: self.phase,
            status,
            duration_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            result_seq,
        };
        if let Ok(body) = serde_json::to_vec(&sample)
            && let Err(error) = self.store.append_event(
                self.session,
                NewEvent::new(EventKind::Note, Kind::Message, &body).label(TIMING_LABEL),
            )
        {
            tracing::warn!("timing could not be recorded: {error}");
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        self.finish(Status::Cancelled, None);
    }
}

/// Read existing v1 timing notes without another storage format or index.
/// Concurrent input receipts can appear between the result and its timing;
/// inspect at most sixteen following records, and match the exact result seq.
/// Missing, malformed, cancelled or out-of-window measurements stay unknown.
pub(crate) fn tool_measurement(
    rook: &Rook,
    result: &rook_store::Event,
) -> Result<Option<crate::transcript::ToolMeasurement>> {
    if result.record.kind != EventKind::ToolResult {
        return Ok(None);
    }
    let Some(from) = result.seq.checked_add(1) else { return Ok(None) };
    for event in rook.store.events(result.session, from, 16)? {
        if event.record.kind != EventKind::Note || event.record.label != TIMING_LABEL {
            continue;
        }
        let size = rook.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
        if size > MAX_TIMING {
            continue;
        }
        let bytes = rook.store.get_range(&event.record.body, 0, MAX_TIMING as usize)?;
        let Ok(sample) = serde_json::from_slice::<Timing>(&bytes) else { continue };
        if !matches!(sample.phase, Phase::ToolDispatch) || sample.result_seq != Some(result.seq) {
            continue;
        }
        let failed = match sample.status {
            Status::Completed => false,
            Status::Failed => true,
            Status::Cancelled => continue,
        };
        return Ok(Some(crate::transcript::ToolMeasurement {
            failed,
            duration_ms: sample.duration_ms,
            timing_seq: event.seq,
        }));
    }
    Ok(None)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub agent_version: String,
    pub platform: String,
    pub architecture: String,
    pub generated_at: i64,
    pub session: Value,
    pub execution: Value,
    pub settings: Value,
    pub events: Vec<Value>,
    pub timings: Vec<Value>,
    pub logs: Vec<Value>,
    pub notices: Vec<String>,
}

impl Report {
    /// Also used before serving over HTTP: escaping must not bypass the cap.
    pub fn json(&self) -> Result<String> {
        struct Bounded(Vec<u8>);
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > MAX_REPORT.saturating_sub(self.0.len()) {
                    return Err(std::io::Error::other(
                        "diagnostic report exceeds 1 MiB; reduce telemetry.diagnostic_events or telemetry.diagnostic_log_bytes",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Bounded(Vec::new());
        serde_json::to_writer_pretty(&mut writer, self)?;
        String::from_utf8(writer.0).map_err(|e| CoreError::Other(e.to_string()))
    }

    /// Export is explicit and never overwrites an existing file or symlink.
    pub fn save(&self, path: &Path) -> Result<PathBuf> {
        let body = self.json()?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|source| CoreError::Io { path: path.into(), source })?;
        temporary
            .write_all(body.as_bytes())
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|source| CoreError::Io { path: path.into(), source })?;
        temporary
            .persist_noclobber(path)
            .map_err(|error| CoreError::Io { path: path.into(), source: error.error })?;
        Ok(path.into())
    }
}

impl Rook {
    /// Safe fields are selected explicitly; session titles, prompts, model URLs,
    /// configuration secrets and tool arguments never enter the default report.
    pub fn diagnostics(&self, session: u128, include_logs: bool) -> Result<Report> {
        let meta = self
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        let limit = self.config.telemetry.diagnostic_events.clamp(1, 2048);
        let from = meta.next_seq.saturating_sub(limit as u64);
        let mut report = Report {
            version: 1,
            agent_version: crate::AGENT_VERSION.into(),
            platform: std::env::consts::OS.into(),
            architecture: std::env::consts::ARCH.into(),
            generated_at: rook_store::now_unix(),
            session: json!({"id":rook_store::format_session_id(session), "created_at":meta.created_at,
                "updated_at":meta.updated_at,"events":meta.event_count,"next_seq":meta.next_seq,
                "tokens_in":meta.tokens_in,"tokens_out":meta.tokens_out,"tail_from":from}),
            execution: Value::Null,
            settings: json!({"context_window_override":self.config.agent.context_window,
                "compact_at":self.config.agent.compact_at,"max_steps":self.config.agent.max_steps,
                "stream_idle_timeout_secs":self.config.agent.stream_idle().as_secs()}),
            events: Vec::new(),
            timings: Vec::new(),
            logs: Vec::new(),
            notices: Vec::new(),
        };
        match crate::execution::diagnostic_state(&self.store, session) {
            Ok(state) => report.execution = state,
            Err(_) => report.notices.push("execution receipt could not be read".into()),
        }
        if from > 0 {
            report.notices.push("only the bounded recent event tail is included".into());
        }
        for event in self.store.events(session, from, limit)? {
            let size = self.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
            report.events.push(json!({"seq":event.seq,"at":event.record.ts,
                "kind":event.record.kind.as_str(),"bytes":size}));
            if event.record.kind == EventKind::Note && event.record.label == TIMING_LABEL {
                let sample = if size <= MAX_TIMING {
                    self.store
                        .get(&event.record.body)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Timing>(&bytes).ok())
                } else {
                    None
                };
                if let Some(sample) = sample {
                    report.timings.push(json!({"seq":event.seq,"at":event.record.ts,"measurement":sample}));
                } else if !report.notices.iter().any(|n| n == "some timing records could not be read") {
                    report.notices.push("some timing records could not be read".into());
                }
            }
        }
        if report.timings.is_empty() {
            report.notices.push("no measured timings in the selected tail".into());
        }
        if include_logs {
            report.notices.push("log tails are redacted heuristically and can contain private application text; inspect before sharing".into());
            let cap = self.config.telemetry.diagnostic_log_bytes.min(64 * 1024);
            for name in ["rook.log", crate::telemetry::PANICS] {
                report.logs.push(log_tail(&crate::paths::logs_dir(), name, cap, Path::new(&meta.workspace)));
            }
        }
        report.json()?;
        Ok(report)
    }
}

fn log_tail(root: &Path, name: &str, cap: usize, workspace: &Path) -> Value {
    let read = || -> std::io::Result<(String, usize, bool)> {
        let mut file = rook_contain::files::open(root, Path::new(name))?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let from = meta.len().saturating_sub(cap as u64);
        file.seek(SeekFrom::Start(from))?;
        let mut bytes = Vec::new();
        file.take(cap as u64).read_to_end(&mut bytes)?;
        let read = bytes.len();
        let start =
            if from > 0 { bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| n + 1) } else { 0 };
        Ok((redact(&String::from_utf8_lossy(&bytes[start..]), workspace), read, from > 0))
    };
    match read() {
        Ok((text, bytes, truncated)) => {
            json!({"name":name,"text":text,"bytes_read":bytes,"truncated":truncated})
        }
        Err(_) => json!({"name":name,"unavailable":true}),
    }
}

fn redact(text: &str, workspace: &Path) -> String {
    static FIELDS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r#"(?im)((?:[a-z0-9_-]*(?:token|secret|password|api[_-]?key)|authorization)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\r\n,;}]+)"#).expect("built-in diagnostic redaction pattern is valid")
    });
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(?:bearer\s+[^\s\x22',;}]+|(?:sk-|gh[pousr]_|github_pat_)[a-z0-9_-]+)")
            .expect("built-in diagnostic redaction pattern is valid")
    });
    static URLS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(?:https?|socks5h?)://[^\s"'<>]+"#)
            .expect("built-in diagnostic redaction pattern is valid")
    });
    let text = FIELDS.replace_all(text, "${1}<redacted>");
    let text = TOKENS.replace_all(&text, "<redacted>");
    let mut text = URLS.replace_all(&text, "<url>").into_owned();
    for path in [workspace.to_path_buf(), crate::paths::home(), crate::paths::user_home()] {
        let raw = path.to_string_lossy();
        if raw.len() > 1 {
            text = text
                .replace(raw.as_ref(), "<local-path>")
                .replace(&raw.replace('\\', "\\\\"), "<local-path>");
        }
    }
    text
}
