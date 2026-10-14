//! Trusted, explicitly named ACP subprocesses. The model never supplies a command.
use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub profiles: BTreeMap<String, Profile>,
    pub frame_bytes: usize,
    pub result_bytes: usize,
    pub file_bytes: usize,
    pub max_messages: usize,
    pub idle_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: BTreeMap::new(),
            frame_bytes: 262_144,
            result_bytes: 131_072,
            file_bytes: 1024 * 1024,
            max_messages: 4096,
            idle_secs: 120,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub command: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Explicit opt-in: an external executable can perform operations independently
    /// of client callbacks. ACP is a transport, not an operating-system sandbox.
    pub trusted: bool,
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !(4096..=1024 * 1024).contains(&self.frame_bytes)
            || !(1024..=1024 * 1024).contains(&self.result_bytes)
            || !(4096..=16 * 1024 * 1024).contains(&self.file_bytes)
            || !(16..=65_536).contains(&self.max_messages)
            || !(1..=3600).contains(&self.idle_secs)
            || self.profiles.len() > 32
        {
            errors.push("acp_executor: expected frame_bytes 4096..1048576, result_bytes 1024..1048576, file_bytes 4096..16777216, max_messages 16..65536, idle_secs 1..3600 and at most 32 profiles".into());
        }
        for (name, p) in &self.profiles {
            if name.is_empty()
                || name.len() > 64
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || p.command.trim().is_empty()
                || p.command.len() > 4096
                || p.command.contains('\0')
                || p.args.len() > 128
                || p.args.iter().any(|a| a.len() > 16384 || a.contains('\0'))
                || p.env.len() > 128
                || p.env.iter().any(|(name, value)| {
                    name.is_empty()
                        || name.len() > 256
                        || value.len() > 16384
                        || value.contains('\0')
                        || !name.bytes().enumerate().all(|(i, b)| {
                            b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                        })
                })
                || p.env.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 65536
            {
                errors.push("acp_executor.profiles: use bounded names/executable, at most 128 arguments and 128 environment entries; environment names must be portable identifiers and the declared environment must fit 64 KiB".into());
            }
        }
        errors
    }
}
fn error(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}

pub(crate) struct Client<'a> {
    child: tokio::process::Child,
    group: rook_contain::Group,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    settings: &'a Settings,
    next: u64,
    received: usize,
    closed: bool,
    diagnostic: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr: tokio::task::JoinHandle<()>,
}
impl<'a> Client<'a> {
    pub(crate) fn start(
        profile: &Profile,
        settings: &'a Settings,
        cwd: &std::path::Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        if !profile.trusted {
            return Err(error("ACP executor requires trusted = true in operator configuration"));
        }
        let mut command = std::process::Command::new(&profile.command);
        command
            .args(&profile.args)
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        rook_contain::quietly(&mut command);
        rook_contain::on_its_own(&mut command);
        let mut child = tokio::process::Command::from(command).kill_on_drop(true).spawn().map_err(|_| {
            error("Cannot start configured ACP executable; check its command and installation")
        })?;
        let group = rook_contain::Group::holding(child.id());
        let input = child.stdin.take().ok_or_else(|| error("ACP subprocess stdin is unavailable"))?;
        let output = child.stdout.take().ok_or_else(|| error("ACP subprocess stdout is unavailable"))?;
        let mut stderr = child.stderr.take().ok_or_else(|| error("ACP subprocess stderr is unavailable"))?;
        let diagnostic = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = diagnostic.clone();
        let limit = settings.frame_bytes.min(16 * 1024);
        let stderr = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            loop {
                let count = match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                let mut tail = captured.lock().unwrap_or_else(|e| e.into_inner());
                let keep = count.min(limit);
                let excess = tail.len().saturating_add(keep).saturating_sub(limit);
                tail.drain(..excess);
                tail.extend_from_slice(&chunk[count - keep..count]);
            }
        });
        Ok(Self {
            child,
            group,
            input,
            output: BufReader::new(output),
            settings,
            next: 1,
            received: 0,
            closed: false,
            diagnostic,
            stderr,
        })
    }
    pub(crate) async fn send(&mut self, value: &Value) -> Result<()> {
        let bytes = crate::persistence::encode_with_limit(value, self.settings.frame_bytes)?;
        tokio::time::timeout(Duration::from_secs(self.settings.idle_secs), async {
            self.input.write_all(&bytes).await?;
            self.input.write_all(b"\n").await?;
            self.input.flush().await
        })
        .await
        .map_err(|_| error("ACP writer stopped accepting input; inspect the child session before retrying"))?
        .map_err(|_| error("ACP subprocess input closed; the external result may be unknown"))
    }
    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or_else(|| error("ACP request IDs exhausted"))?;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await?;
        Ok(id)
    }
    pub(crate) async fn receive(&mut self) -> Result<Value> {
        if self.received >= self.settings.max_messages {
            return Err(error("ACP message ceiling reached; inspect the retained child history"));
        }
        let mut bytes = Vec::new();
        loop {
            let available =
                tokio::time::timeout(Duration::from_secs(self.settings.idle_secs), self.output.fill_buf())
                    .await
                    .map_err(|_| {
                        error("ACP subprocess became silent; inspect its child session before retrying")
                    })?
                    .map_err(|_| error("ACP subprocess output failed; the external result may be unknown"))?;
            if available.is_empty() {
                return Err(error(
                    "ACP subprocess disconnected before completing its request; inspect the child session",
                ));
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let count = newline.map_or(available.len(), |at| at + 1);
            if bytes.len().saturating_add(count) > self.settings.frame_bytes {
                return Err(error(
                    "ACP frame exceeds acp_executor.frame_bytes; no partial message was accepted",
                ));
            }
            bytes.extend_from_slice(&available[..count]);
            self.output.consume(count);
            if newline.is_some() {
                break;
            }
        }
        self.received += 1;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error("ACP subprocess sent malformed JSON"))?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(error("ACP subprocess sent an unsupported JSON-RPC envelope"));
        }
        Ok(value)
    }
    pub(crate) async fn handshake(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.request(method, params).await?;
        let response = self.receive().await?;
        if response.get("id").and_then(Value::as_u64) != Some(id) || response.get("method").is_some() {
            return Err(error(
                "ACP setup response is out of order or requested unsupported setup interaction",
            ));
        }
        if response.get("error").is_some() {
            return Err(error(format!(
                "ACP {method} failed; check external agent authentication/configuration"
            )));
        }
        response.get("result").cloned().ok_or_else(|| error("ACP setup response has no result"))
    }
    pub(crate) fn diagnostic(&self) -> String {
        let bytes = self.diagnostic.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }
    pub(crate) async fn close(&mut self) {
        self.group.end();
        self.closed = true;
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        if tokio::time::timeout(Duration::from_secs(1), &mut self.stderr).await.is_err() {
            self.stderr.abort();
        }
    }
}
impl Drop for Client<'_> {
    fn drop(&mut self) {
        self.stderr.abort();
        if !self.closed {
            self.group.end();
            let _ = self.child.start_kill();
        }
    }
}
