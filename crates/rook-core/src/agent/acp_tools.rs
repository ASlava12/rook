//! Raw client file content is distinct from numbered model-facing file pages.
use crate::secrets::Vault;
use async_trait::async_trait;
use rook_tools::{Tool, ToolContext, ToolError, ToolOutcome};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct Read {
    pub(super) file_bytes: usize,
    pub(super) result_bytes: usize,
    pub(super) vault: Arc<Vault>,
}
#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "acp_read_text"
    }
    fn spec(&self) -> rook_llm::ToolSpec {
        rook_llm::ToolSpec {
            name: self.name().into(),
            description: "Read bounded raw client file content, preserving its original line endings.".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}},"required":["path"]}),
        }
    }
    async fn call(&self, ctx: &ToolContext, args: &Value) -> rook_tools::Result<ToolOutcome> {
        let invalid =
            |message: &str| ToolError::Invalid { tool: self.name().into(), message: message.into() };
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or_else(|| invalid("path must be a bounded nonempty string"))?;
        let number = |name: &str, default: usize| match args.get(name) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("line/limit must be positive integers")),
        };
        let line = number("line", 1)?;
        let limit = number("limit", usize::MAX)?;
        let path = ctx.resolve(path)?;
        let text = ctx.read_text_limited(&path, self.file_bytes).await?;
        let mut selected = String::new();
        for part in text.split_inclusive('\n').skip(line - 1).take(limit) {
            if part.len() > self.result_bytes.saturating_sub(selected.len()) {
                return Err(invalid(
                    "Selected raw file content exceeds the frame limit; request fewer lines or raise acp_executor.frame_bytes",
                ));
            }
            selected.push_str(part);
        }
        // Redact before JSON escaping so multiline/quoted credentials are covered.
        let content = self.vault.redact(&selected);
        #[derive(serde::Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        let bytes = crate::persistence::encode_with_limit(&Body { content: &content }, self.result_bytes)
            .map_err(|_| invalid("Encoded raw file content exceeds the frame limit; request fewer lines"))?;
        let result = String::from_utf8(bytes).map_err(|_| invalid("Raw text result is not UTF-8"))?;
        Ok(ToolOutcome::ok(result))
    }
    fn observed_paths(&self, args: &Value) -> Vec<String> {
        args.get("path").and_then(Value::as_str).map(|p| vec![p.into()]).unwrap_or_default()
    }
}

/// Job IDs never become ambient authority over another session's processes.
/// Pending launch ownership covers cancellation during post-tool hooks too.
pub(super) struct Terminals<'a> {
    pub(super) jobs: Arc<rook_tools::jobs::Jobs>,
    pub(super) active: std::collections::BTreeMap<String, usize>,
    pub(super) owned: Vec<String>,
    pub(super) secret_bytes: usize,
    pub(super) pending: &'a std::sync::Mutex<Option<String>>,
}
impl Drop for Terminals<'_> {
    fn drop(&mut self) {
        for id in &self.owned {
            self.jobs.stop(id);
        }
        if let Some(id) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).as_deref() {
            self.jobs.stop(id);
        }
    }
}

pub(super) fn command(params: &Value) -> rook_tools::Result<String> {
    let invalid =
        |message: &str| ToolError::Invalid { tool: "terminal/create".into(), message: message.into() };
    let executable = params
        .get("command")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(|| invalid("command must be a bounded nonempty executable"))?;
    let mut words = Vec::new();
    if let Some(env) = params.get("env") {
        let env = env
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or_else(|| invalid("env must contain at most 128 entries"))?;
        let mut seen = std::collections::HashSet::new();
        for entry in env {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or_else(|| invalid("environment names must be bounded strings"))?;
            let value = entry
                .get("value")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 16384)
                .ok_or_else(|| invalid("environment values must be bounded strings"))?;
            if !name
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
                || !seen.insert(name)
            {
                return Err(invalid("environment names must be unique shell identifiers"));
            }
            words.push(rook_contain::shell_assignment(name, value).map_err(|e| invalid(&e.to_string()))?);
        }
    }
    words.push(rook_contain::shell_argument(executable).map_err(|e| invalid(&e.to_string()))?);
    if let Some(args) = params.get("args") {
        let args = args
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or_else(|| invalid("args must contain at most 128 entries"))?;
        for arg in args {
            let arg = arg
                .as_str()
                .filter(|s| s.len() <= 16384)
                .ok_or_else(|| invalid("arguments must be bounded strings"))?;
            words.push(rook_contain::shell_argument(arg).map_err(|e| invalid(&e.to_string()))?);
        }
    }
    Ok(words.join(" "))
}

pub(super) struct Secrets {
    pub(super) parent: Arc<dyn rook_tools::Secrets>,
    pub(super) vault: Arc<Vault>,
}
impl rook_tools::Secrets for Secrets {
    fn value(&self, name: &str) -> Option<String> {
        self.parent.value(name)
    }
    fn redactions(&self) -> Vec<String> {
        let mut values = self.parent.redactions();
        for value in rook_tools::Secrets::redactions(self.vault.as_ref()) {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        values
    }
}

/// Policy evaluates the real invocation; frontend permission text never needs
/// to disclose a credential supplied by the external agent.
pub(super) struct Approver {
    pub(super) inner: Arc<dyn rook_tools::policy::Approver>,
    pub(super) vault: Arc<Vault>,
}
#[async_trait]
impl rook_tools::policy::Approver for Approver {
    fn for_session(&self, session: &str) -> Option<Arc<dyn rook_tools::policy::Approver>> {
        self.inner.for_session(session).map(|inner| {
            Arc::new(Self { inner, vault: self.vault.clone() }) as Arc<dyn rook_tools::policy::Approver>
        })
    }
    async fn ask(
        &self,
        tool: &str,
        risk: &rook_tools::policy::Risk,
        preview: Option<&str>,
    ) -> rook_tools::policy::Approval {
        use rook_tools::policy::Risk;
        let shown = match risk {
            Risk::Execute(command) => Risk::Execute(self.vault.redact(command)),
            Risk::Write(paths) => Risk::Write(paths.iter().map(|path| self.vault.redact(path)).collect()),
            Risk::Network(url) => Risk::Network(self.vault.redact(url)),
            Risk::External { name, claims_read_only } => {
                Risk::External { name: self.vault.redact(name), claims_read_only: *claims_read_only }
            }
            other => other.clone(),
        };
        let preview = preview.map(|text| self.vault.redact(text));
        self.inner.ask(tool, &shown, preview.as_deref()).await
    }
}
