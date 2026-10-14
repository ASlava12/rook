//! One browser context per conversation, carried through ordinary tool dispatch.
//! Pages never choose tools or permissions. The configured local MCP executable
//! is trusted equipment; observations from it remain external source data.
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rook_tools::{Tool, ToolContext, ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const TOOL: &str = "browser";
const ACTIONS: &[(&str, &str)] = &[
    ("navigate", "browser_navigate"),
    ("snapshot", "browser_snapshot"),
    ("click", "browser_click"),
    ("type", "browser_type"),
    ("wait", "browser_wait_for"),
    ("screenshot", "browser_take_screenshot"),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub include_images: bool,
    pub command: String,
    pub args: Vec<String>,
    pub max_sessions: usize,
    pub max_calls: usize,
    pub idle_secs: u64,
    pub startup_timeout_secs: u64,
    pub call_timeout_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            include_images: false,
            command: "npx".into(),
            args: vec!["--yes".into(), "@playwright/mcp@0.0.83".into()],
            max_sessions: 4,
            max_calls: 128,
            idle_secs: 600,
            startup_timeout_secs: 30,
            call_timeout_secs: 45,
        }
    }
}

impl Settings {
    pub fn error(&self) -> Option<&'static str> {
        if self.command.is_empty() || self.command.len() > 4096 || self.command.contains('\0') {
            Some("browser.command: expected a local executable, 1..=4096 bytes")
        } else if self.args.len() > 16
            || self.args.iter().any(|a| a.len() > 4096 || a.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>() > 8192
        {
            Some("browser.args: at most 16 arguments, 4096 bytes each and 8192 total")
        } else if !(1..=16).contains(&self.max_sessions) {
            Some("browser.max_sessions: expected 1..=16")
        } else if !(1..=1024).contains(&self.max_calls) {
            Some("browser.max_calls: expected 1..=1024")
        } else if !(30..=3600).contains(&self.idle_secs) {
            Some("browser.idle_secs: expected 30..=3600")
        } else if !(1..=120).contains(&self.startup_timeout_secs) {
            Some("browser.startup_timeout_secs: expected 1..=120")
        } else if !(1..=120).contains(&self.call_timeout_secs) {
            Some("browser.call_timeout_secs: expected 1..=120")
        } else {
            None
        }
    }
}

struct Connected {
    server: rook_mcp::Server,
    // Screenshots and any upstream artifacts stay outside the user's workspace.
    _directory: tempfile::TempDir,
}

struct Slot {
    generation: u128,
    settings: Settings,
    connected: Mutex<Option<Arc<Connected>>>,
    busy: AtomicBool,
    uncertain: AtomicBool,
    calls: AtomicUsize,
    last: Mutex<Instant>,
}

impl Slot {
    fn new(settings: Settings, generation: u128) -> Self {
        Self {
            generation,
            settings,
            connected: Mutex::new(None),
            busy: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            last: Mutex::new(Instant::now()),
        }
    }
}

#[derive(Default)]
pub(crate) struct Pool {
    slots: Mutex<BTreeMap<u128, Arc<Slot>>>,
}

impl Pool {
    fn slot(
        &self,
        session: u128,
        settings: &Settings,
        create: bool,
        claim: bool,
    ) -> Result<Option<Arc<Slot>>, &'static str> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        // Reclamation never races an admitted call. A reclaimed context must be
        // explicitly navigated again; old refs cannot start a fresh browser.
        slots.retain(|_, slot| {
            slot.busy.load(Ordering::Acquire)
                || slot.last.lock().unwrap_or_else(|e| e.into_inner()).elapsed().as_secs()
                    < slot.settings.idle_secs
        });
        if let Some(slot) = slots.get(&session) {
            if claim && slot.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
            {
                return Err("a browser call is already in progress for this session");
            }
            return Ok(Some(slot.clone()));
        }
        if !create {
            return Ok(None);
        }
        if slots.len() >= settings.max_sessions {
            return Err(
                "browser capacity reached; close an idle conversation's browser or wait for idle expiry",
            );
        }
        // Unique across engine lifetimes too: saved refs from before a restart
        // must never address the first page opened by the resumed conversation.
        let generation = rook_store::new_session_id();
        let slot = Arc::new(Slot::new(settings.clone(), generation));
        slot.busy.store(claim, Ordering::Release);
        slots.insert(session, slot.clone());
        Ok(Some(slot))
    }
}

// This also covers cancellation of the whole turn. Once a call could have been
// delivered, a missing answer cannot authorize repeating it or reusing its page.
struct Calling {
    slot: Arc<Slot>,
    started: bool,
    confirmed: bool,
}
impl Drop for Calling {
    fn drop(&mut self) {
        if self.started && !self.confirmed {
            self.slot.uncertain.store(true, Ordering::Release);
            self.slot.connected.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        *self.slot.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.slot.busy.store(false, Ordering::Release);
    }
}

pub(crate) struct Browser {
    pub pool: Arc<Pool>,
    pub settings: Settings,
    pub session: u128,
}

pub(crate) fn register(tools: &mut rook_tools::ToolBox, rook: &crate::Rook, session: u128) {
    // ToolBox registration appends. A child inheriting the parent's toolbox
    // must replace that binding, otherwise the first (parent-owned) tool wins.
    *tools = tools.without(&[TOOL]);
    if rook.config.browser.enabled {
        tools.register(Arc::new(Browser {
            pool: rook.browser.clone(),
            settings: rook.config.browser.clone(),
            session,
        }));
    }
}

fn remote(action: &str) -> Option<&'static str> {
    ACTIONS.iter().find_map(|(name, remote)| (*name == action).then_some(*remote))
}

/// Reject invalid/oversized arguments before admitting a browser or spawning.
fn arguments(args: &Value) -> Result<(&str, Value), &'static str> {
    let object = args.as_object().ok_or("browser expects an object")?;
    if object.len() > 5
        || object.keys().any(|k| !matches!(k.as_str(), "action" | "url" | "ref" | "text" | "seconds"))
    {
        return Err("browser arguments: action, url, ref, text or seconds only");
    }
    let action = args.get("action").and_then(Value::as_str).ok_or("browser needs action")?;
    let allowed: &[&str] = match action {
        "navigate" => &["action", "url"],
        "click" => &["action", "ref"],
        "type" => &["action", "ref", "text"],
        "wait" => &["action", "text", "seconds"],
        "snapshot" | "screenshot" | "status" | "close" => &["action"],
        _ => return Err("unknown browser action"),
    };
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("argument does not apply to this browser action");
    }
    let string = |key: &str, max: usize| -> Result<&str, &'static str> {
        let value = args.get(key).and_then(Value::as_str).ok_or("missing browser string argument")?;
        if value.is_empty() || value.len() > max || value.contains('\0') {
            Err("browser string is empty or exceeds its byte limit")
        } else {
            Ok(value)
        }
    };
    let target = || -> Result<&str, &'static str> {
        let value = string("ref", 80)?;
        let value_ref = value.rsplit(':').next().ok_or("invalid browser ref")?;
        // Allow accessibility refs only, rather than arbitrary selectors or JS.
        if !value_ref.starts_with('e')
            || value_ref.len() < 2
            || !value_ref[1..].bytes().all(|b| b.is_ascii_digit())
        {
            return Err("ref must come from this session's latest browser snapshot");
        }
        Ok(value)
    };
    let mapped = match action {
        "navigate" => {
            let url = string("url", 4096)?;
            let parsed = reqwest::Url::parse(url).map_err(|_| "invalid browser URL")?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err("browser URL must be http(s) without embedded credentials");
            }
            json!({"url":url})
        }
        "click" => json!({"target":target()?}),
        "type" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| text.len() <= 16384 && !text.contains('\0'))
                .ok_or("type needs text of at most 16384 bytes")?;
            json!({"target":target()?,"text":text})
        }
        "wait" => match (args.get("text"), args.get("seconds")) {
            (Some(_), None) => json!({"text":string("text",1024)?}),
            (None, Some(value)) => {
                let seconds = value
                    .as_u64()
                    .filter(|n| (1..=30).contains(n))
                    .ok_or("wait seconds must be an integer in 1..=30")?;
                json!({"time":seconds})
            }
            _ => return Err("wait needs either text or seconds"),
        },
        "screenshot" => json!({"type":"png","fullPage":false}),
        _ => json!({}),
    };
    Ok((action, mapped))
}

async fn connect(settings: &Settings) -> Result<Arc<Connected>, String> {
    let directory =
        tempfile::tempdir().map_err(|e| format!("cannot create browser artifact directory: {e}"))?;
    let mut args = settings.args.clone();
    args.extend(
        [
            "--headless",
            "--isolated",
            "--browser",
            "chromium",
            "--viewport-size",
            "1280,720",
            "--idle-timeout",
            "0",
            "--no-webmcp",
            "--image-responses",
            "allow",
            "--output-max-size",
            "16777216",
            "--timeout-action",
            "5000",
            "--timeout-navigation",
            "30000",
            "--output-dir",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    args.push(directory.path().display().to_string());
    let config = rook_mcp::ServerConfig {
        name: "browser".into(),
        command: settings.command.clone(),
        args,
        cwd: Some(directory.path().display().to_string()),
        startup_timeout_secs: settings.startup_timeout_secs,
        call_timeout_secs: settings.call_timeout_secs,
        catalog_max_bytes: 256 * 1024,
        catalog_max_tools: 64,
        catalog_max_pages: 4,
        catalog_timeout_secs: settings.startup_timeout_secs,
        ..Default::default()
    };
    let connected = Arc::new(Connected {
        server: rook_mcp::Server::connect(&config, &rook_llm::Proxy::Direct)
            .await
            .map_err(|e| e.to_string())?,
        _directory: directory,
    });
    let tools = connected.server.list_tools().await.map_err(|e| e.to_string())?;
    if ACTIONS.iter().any(|(_, name)| !tools.iter().any(|tool| tool.name == *name)) {
        return Err(
            "configured browser server lacks required Playwright tools; check pinned package and arguments"
                .into(),
        );
    }
    Ok(connected)
}

fn namespaced(text: &str, prefix: &str, budget: usize) -> (String, usize) {
    let Ok(refs) = regex::Regex::new(r"\[ref=(e[0-9]+)\]") else { return (String::new(), text.len()) };
    let mut output = String::with_capacity(budget.min(text.len()));
    let mut total = 0usize;
    let mut full = false;
    let mut append = |value: &str| {
        total = total.saturating_add(value.len());
        if full {
            return;
        }
        full = value.len() > budget.saturating_sub(output.len());
        let mut end = value.len().min(budget.saturating_sub(output.len()));
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&value[..end]);
    };
    let mut previous = 0;
    for capture in refs.captures_iter(text) {
        let Some(whole) = capture.get(0) else { continue };
        append(&text[previous..whole.start()]);
        append("[ref=");
        append(prefix);
        append(&capture[1]);
        append("]");
        previous = whole.end();
    }
    append(&text[previous..]);
    (output, total)
}

#[async_trait]
impl Tool for Browser {
    fn name(&self) -> &str {
        TOOL
    }
    fn spec(&self) -> rook_llm::ToolSpec {
        rook_llm::ToolSpec {
            name: TOOL.into(), description: "Control this session's isolated browser. Navigate, then snapshot for page text and exact click/type refs; navigation may return only an artifact link. Close after an uncertain call; effects are never retried automatically.".into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["navigate","snapshot","click","type","wait","screenshot","status","close"]},
                "url":{"type":"string","maxLength":4096}, "ref":{"type":"string","maxLength":80},
                "text":{"type":"string","maxLength":16384}, "seconds":{"type":"integer","minimum":1,"maximum":30}
            },"required":["action"],"additionalProperties":false}),
        }
    }
    fn risk(&self, args: &Value) -> rook_tools::policy::Risk {
        if args.get("action").and_then(Value::as_str) == Some("status") {
            rook_tools::policy::Risk::ReadOnly
        } else {
            rook_tools::policy::Risk::External { name: TOOL.into(), claims_read_only: false }
        }
    }
    fn mcp_source(&self, args: &Value) -> Option<(&str, &str)> {
        remote(args.get("action")?.as_str()?).map(|name| (TOOL, name))
    }
    async fn call(&self, ctx: &ToolContext, args: &Value) -> rook_tools::Result<ToolOutcome> {
        let error = |why: &str| {
            ToolOutcome::error(why).with("browser_session", rook_store::format_session_id(self.session))
        };
        if !self.settings.enabled {
            return Ok(error("browser is disabled"));
        }
        if let Some(why) = self.settings.error() {
            return Ok(error(why));
        }
        let (action, mut mapped) = match arguments(args) {
            Ok(value) => value,
            Err(why) => return Ok(error(why)),
        };
        let slot =
            match self.pool.slot(self.session, &self.settings, action == "navigate", action != "status") {
                Ok(Some(slot)) => slot,
                Ok(None) if matches!(action, "status" | "close") => {
                    return Ok(ToolOutcome::ok(
                        "browser is closed; navigate to start a fresh isolated context",
                    ));
                }
                Ok(None) => return Ok(error("browser is closed; navigate first; old page refs are invalid")),
                Err(why) => return Ok(error(why)),
            };
        if action == "status" {
            return Ok(ToolOutcome::ok(
                json!({"session":rook_store::format_session_id(self.session),
                "busy":slot.busy.load(Ordering::Acquire), "uncertain":slot.uncertain.load(Ordering::Acquire),
                "calls":slot.calls.load(Ordering::Relaxed), "limit":slot.settings.max_calls,
                "generation":rook_store::format_session_id(slot.generation),
                "started":slot.connected.lock().unwrap_or_else(|e| e.into_inner()).is_some() })
                .to_string(),
            ));
        }
        let mut calling = Calling { slot: slot.clone(), started: false, confirmed: false };
        if action == "close" {
            calling.started = true;
            let connected = slot.connected.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(connected) = connected {
                connected.server.shutdown().await;
            }
            self.pool.slots.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.session);
            calling.confirmed = true;
            return Ok(ToolOutcome::ok(
                "browser closed; its page refs are invalid; navigate for a new context",
            ));
        }
        if slot.uncertain.load(Ordering::Acquire) {
            return Ok(error(
                "previous browser outcome is unknown; inspect the recorded result and close before starting a new context; no action was replayed",
            ));
        }
        if slot.settings != self.settings {
            return Ok(error("browser configuration changed; close before using the new configuration"));
        }
        let prefix = format!(
            "{}:{}:",
            rook_store::format_session_id(self.session),
            rook_store::format_session_id(slot.generation)
        );
        if let Some(target) = mapped.get("target").and_then(Value::as_str) {
            let Some(target) = target.strip_prefix(&prefix) else {
                return Ok(error(
                    "browser ref belongs to another session or closed context; read this session's latest snapshot",
                ));
            };
            mapped["target"] = Value::String(target.to_owned());
        }
        if slot.calls.load(Ordering::Relaxed) >= self.settings.max_calls {
            return Ok(error("browser call limit reached; close before starting a new context"));
        }
        calling.started = true;
        slot.calls.fetch_add(1, Ordering::Relaxed);
        let previous = slot.connected.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let connected = match previous {
            Some(connected) => connected,
            None => {
                ctx.watching("starting an isolated browser for this conversation");
                match tokio::time::timeout(
                    Duration::from_secs(self.settings.startup_timeout_secs),
                    connect(&self.settings),
                )
                .await
                {
                    Ok(Ok(connected)) => {
                        *slot.connected.lock().unwrap_or_else(|e| e.into_inner()) = Some(connected.clone());
                        connected
                    }
                    Ok(Err(why)) => {
                        return Ok(error(&format!(
                            "browser setup failed: {why}; close and check browser.command, package and Chromium installation"
                        )));
                    }
                    Err(_) => {
                        return Ok(error(
                            "browser setup deadline reached; close and check the configured executable and Chromium installation",
                        ));
                    }
                }
            }
        };
        let Some(name) = remote(action) else {
            return Ok(error("unknown browser action"));
        };
        let result = match tokio::time::timeout(
            Duration::from_secs(self.settings.call_timeout_secs),
            connected.server.call_tool(name, &mapped),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(why)) => {
                return Ok(error(&format!(
                    "browser outcome is unknown and was not replayed: {why}; close before starting a new context"
                )));
            }
            Err(_) => {
                return Ok(error(
                    "browser call deadline reached; outcome is unknown and was not replayed; close before starting a new context",
                ));
            }
        };
        let mut outcome = rook_tools::mcp::outcome(&result, ctx, TOOL)
            .with("browser_session", rook_store::format_session_id(self.session))
            .with("browser_action", action)
            .with("browser_generation", rook_store::format_session_id(slot.generation));
        let (content, total) = namespaced(&outcome.content, &prefix, ctx.max_output_bytes);
        outcome.content = content;
        outcome.full_bytes = outcome.full_bytes.max(total);
        outcome.truncated |= total > ctx.max_output_bytes;
        // Error replies can follow partial effects too. Require an explicit
        // close even when the transport itself survived.
        calling.confirmed = !outcome.is_error;
        Ok(outcome)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn fixture(root: &std::path::Path, session: u128, pool: Arc<Pool>) -> Browser {
        Browser {
            pool,
            session,
            settings: Settings {
                enabled: true,
                command: "python3".into(),
                args: vec![
                    format!("{}/tests/fixtures/browser_mcp.py", env!("CARGO_MANIFEST_DIR")),
                    root.join("calls.jsonl").display().to_string(),
                ],
                ..Default::default()
            },
        }
    }
    fn calls(root: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|value: &Value| value.get("name").is_some())
            .collect()
    }
    fn context(root: &std::path::Path) -> ToolContext {
        ToolContext::new(root.to_path_buf())
    }
    fn first_ref(text: &str) -> String {
        text.split("[ref=").nth(1).unwrap().split(']').next().unwrap().to_owned()
    }
    #[test]
    fn browser_input_and_rendering_limits_precede_spawning_and_namespace_allocation() {
        for request in [
            json!({"action":"navigate","url":"file:///etc/passwd"}),
            json!({"action":"navigate","url":"https://user:password@example.org"}),
            json!({"action":"navigate","url":"x".repeat(4097)}),
            json!({"action":"wait","seconds":31}),
            json!({"action":"wait","seconds":1,"text":"ready"}),
            json!({"action":"click","ref":"body"}),
            json!({"action":"type","ref":"e1","text":"Ж".repeat(8193)}),
            json!({"action":"snapshot","filename":"outside"}),
        ] {
            assert!(arguments(&request).is_err(), "{request}");
        }
        let source = "[ref=e1]Ж".repeat(1000);
        assert!(source.len() > 1024);
        let (bounded, total) = namespaced(&source, &"a".repeat(48), 1024);
        assert!(total > source.len());
        assert!(bounded.len() <= 1024);
        assert!(bounded.is_char_boundary(bounded.len()));
        let settings = Settings { max_sessions: 17, ..Default::default() };
        assert!(settings.error().is_some());
        let settings = Settings { args: vec!["x".repeat(8193)], ..Default::default() };
        assert!(settings.error().is_some());
    }
    #[test]
    fn browser_capacity_never_evicts_an_admitted_call_and_expiry_invalidates_its_generation() {
        let pool = Pool::default();
        let settings = Settings { max_sessions: 1, ..Default::default() };
        let slot = pool.slot(1, &settings, true, true).unwrap().unwrap();
        *slot.last.lock().unwrap() = Instant::now() - Duration::from_secs(601);
        assert!(
            pool.slot(2, &settings, true, true).is_err(),
            "the first slot is active despite its old clock"
        );
        assert!(pool.slot(1, &settings, true, true).is_err(), "calls cannot queue for one context");
        let generation = slot.generation;
        drop(Calling { slot: slot.clone(), started: true, confirmed: false });
        assert!(slot.uncertain.load(Ordering::Acquire));
        *slot.last.lock().unwrap() = Instant::now() - Duration::from_secs(601);
        let fresh = pool.slot(1, &settings, true, false).unwrap().unwrap();
        assert!(fresh.generation > generation);
        assert!(!fresh.uncertain.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn contexts_and_refs_are_bound_to_their_conversation_and_generation() {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let pool = Arc::new(Pool::default());
        let first = fixture(root.path(), 1, pool.clone());
        let second = fixture(root.path(), 2, pool);
        let a = first.call(&ctx, &json!({"action":"navigate","url":"http://localhost/first"})).await.unwrap();
        assert!(!a.is_error, "{}", a.content);
        let old_ref = first_ref(&a.content);
        let b =
            second.call(&ctx, &json!({"action":"navigate","url":"http://localhost/second"})).await.unwrap();
        assert!(!b.is_error, "{}", b.content);
        assert_ne!(old_ref, first_ref(&b.content));
        let before = calls(root.path()).len();
        assert!(second.call(&ctx, &json!({"action":"click","ref":old_ref})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), before);
        let snapshot = first.call(&ctx, &json!({"action":"snapshot"})).await.unwrap();
        assert!(snapshot.content.contains("/first"));
        assert!(!snapshot.content.contains("/second"));
        first.call(&ctx, &json!({"action":"close"})).await.unwrap();
        assert!(first.call(&ctx, &json!({"action":"snapshot"})).await.unwrap().is_error);
        first.call(&ctx, &json!({"action":"navigate","url":"http://localhost/fresh"})).await.unwrap();
        let before = calls(root.path()).len();
        assert!(first.call(&ctx, &json!({"action":"click","ref":old_ref})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), before);
        first.call(&ctx, &json!({"action":"close"})).await.unwrap();
        second.call(&ctx, &json!({"action":"close"})).await.unwrap();
        let resumed = fixture(root.path(), 1, Arc::new(Pool::default()));
        let opened =
            resumed.call(&ctx, &json!({"action":"navigate","url":"http://localhost/resumed"})).await.unwrap();
        assert!(!opened.is_error, "{}", opened.content);
        assert_ne!(old_ref, first_ref(&opened.content), "restarting the engine cannot revive a saved ref");
        let before = calls(root.path()).len();
        assert!(resumed.call(&ctx, &json!({"action":"click","ref":old_ref})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), before);
        resumed.call(&ctx, &json!({"action":"close"})).await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_and_transport_loss_poison_the_context_without_replaying_a_call() {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let browser = Arc::new(fixture(root.path(), 1, Arc::new(Pool::default())));
        let opened =
            browser.call(&ctx, &json!({"action":"navigate","url":"http://localhost/ready"})).await.unwrap();
        assert!(!opened.is_error, "{}", opened.content);
        let task = tokio::spawn({
            let browser = browser.clone();
            let ctx = ctx.clone();
            async move { browser.call(&ctx, &json!({"action":"wait","seconds":30})).await }
        });
        tokio::time::timeout(Duration::from_secs(30), async {
            while !calls(root.path()).iter().any(|c| c["name"] == "browser_wait_for") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let status = browser.call(&ctx, &json!({"action":"status"})).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&status.content).unwrap()["uncertain"], true);
        let before = calls(root.path()).len();
        assert!(
            browser
                .call(&ctx, &json!({"action":"navigate","url":"http://localhost/replay"}))
                .await
                .unwrap()
                .is_error
        );
        assert_eq!(calls(root.path()).len(), before);
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
        let failed =
            browser.call(&ctx, &json!({"action":"navigate","url":"http://localhost/crash"})).await.unwrap();
        assert!(failed.is_error, "{}", failed.content);
        assert!(
            browser
                .call(&ctx, &json!({"action":"navigate","url":"http://localhost/crash"}))
                .await
                .unwrap()
                .is_error
        );
        assert_eq!(
            calls(root.path()).iter().filter(|c| c["arguments"]["url"] == "http://localhost/crash").count(),
            1
        );
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
    }
    #[tokio::test]
    async fn page_call_cap_refuses_before_dispatch_and_close_releases_capacity() {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let mut browser = fixture(root.path(), 1, Arc::new(Pool::default()));
        browser.settings.max_calls = 2;
        assert!(
            !browser
                .call(&ctx, &json!({"action":"navigate","url":"http://localhost/one"}))
                .await
                .unwrap()
                .is_error
        );
        assert!(!browser.call(&ctx, &json!({"action":"snapshot"})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), 2);
        assert!(browser.call(&ctx, &json!({"action":"snapshot"})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), 2);
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
        assert!(
            !browser
                .call(&ctx, &json!({"action":"navigate","url":"http://localhost/new"}))
                .await
                .unwrap()
                .is_error
        );
        assert_eq!(calls(root.path()).len(), 3);
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
    }
    #[tokio::test]
    async fn closing_a_browser_ends_the_mcp_launchers_child_processes() {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let browser = fixture(root.path(), 1, Arc::new(Pool::default()));
        let result =
            browser.call(&ctx, &json!({"action":"navigate","url":"http://localhost/tree"})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        let rows = std::fs::read_to_string(root.path().join("calls.jsonl")).unwrap();
        let child = rows
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find_map(|v| v["child"].as_u64())
            .unwrap();
        let running = || {
            let output = rook_contain::quietly(&mut std::process::Command::new("ps"))
                .args(["-p", &child.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().starts_with('Z')
        };
        assert!(running(), "the descendant must be alive before the cleanup assertion");
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while running() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancelling_close_retains_uncertainty_and_invalidates_old_refs_before_reopening() {
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let pool = Arc::new(Pool::default());
        let browser = Arc::new(fixture(root.path(), 3, pool.clone()));
        let opened =
            browser.call(&ctx, &json!({"action":"navigate","url":"http://localhost/closing"})).await.unwrap();
        assert!(!opened.is_error, "{}", opened.content);
        let old_ref = first_ref(&opened.content);
        let slot = pool.slot(3, &browser.settings, false, false).unwrap().unwrap();
        let close = tokio::spawn({
            let browser = browser.clone();
            let ctx = ctx.clone();
            async move { browser.call(&ctx, &json!({"action":"close"})).await }
        });
        tokio::time::timeout(Duration::from_secs(30), async {
            while slot.connected.lock().unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!close.is_finished(), "must cancel during actual driver cleanup");
        close.abort();
        assert!(close.await.unwrap_err().is_cancelled());
        assert!(slot.uncertain.load(Ordering::Acquire));
        assert!(
            browser
                .call(&ctx, &json!({"action":"navigate","url":"http://localhost/replay"}))
                .await
                .unwrap()
                .is_error
        );
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
        browser.call(&ctx, &json!({"action":"navigate","url":"http://localhost/new"})).await.unwrap();
        let before = calls(root.path()).len();
        assert!(browser.call(&ctx, &json!({"action":"click","ref":old_ref})).await.unwrap().is_error);
        assert_eq!(calls(root.path()).len(), before);
        browser.call(&ctx, &json!({"action":"close"})).await.unwrap();
    }

    // Playwright detaches Chromium into another process group. A protocol
    // fixture that merely spawns a child cannot prove this cleanup contract.
    #[tokio::test]
    #[ignore = "requires pinned Playwright MCP and installed Chromium; run explicitly"]
    async fn real_chromium_cancellation_and_engine_drop_leave_no_owned_browser() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).await;
                let body = "<!doctype html><title>Owned page</title>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        let root = tempfile::tempdir().unwrap();
        let ctx = context(root.path());
        let mut settings = Settings { enabled: true, startup_timeout_secs: 120, ..Default::default() };
        if let Ok(path) = std::env::var("ROOK_BROWSER_MCP_CLI") {
            settings.command = "node".into();
            settings.args = vec![path];
        }
        for disposition in ["call", "close", "engine"] {
            let pool = Arc::new(Pool::default());
            let browser = Arc::new(Browser { pool: pool.clone(), settings: settings.clone(), session: 17 });
            let opened = browser
                .call(&ctx, &json!({"action":"navigate","url":format!("http://{address}/")}))
                .await
                .unwrap();
            assert!(!opened.is_error, "{}", opened.content);
            let slot = pool.slot(17, &settings, false, false).unwrap().unwrap();
            let connected = slot.connected.lock().unwrap().as_ref().unwrap().clone();
            let driver = connected.server.child_pid().await.unwrap();
            let output = rook_contain::quietly(&mut std::process::Command::new("ps"))
                .args(["-axo", "pid=,ppid=,command="])
                .output()
                .unwrap();
            let rows: Vec<(u32, u32, String)> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    Some((
                        parts.next()?.parse().ok()?,
                        parts.next()?.parse().ok()?,
                        parts.collect::<Vec<_>>().join(" "),
                    ))
                })
                .collect();
            let mut family = vec![driver];
            for _ in 0..16 {
                let children: Vec<u32> = rows
                    .iter()
                    .filter(|(pid, parent, _)| family.contains(parent) && !family.contains(pid))
                    .map(|(pid, _, _)| *pid)
                    .collect();
                if children.is_empty() {
                    break;
                }
                family.extend(children);
            }
            let browsers: Vec<u32> = rows
                .iter()
                .filter(|(pid, _, command)| {
                    family.contains(pid)
                        && command.contains("--remote-debugging-pipe")
                        && !command.contains("--type=")
                })
                .map(|(pid, _, _)| *pid)
                .collect();
            assert_eq!(browsers.len(), 1, "must identify a live Chromium owned by this exact driver");
            let running = |pid: u32| {
                let output = rook_contain::quietly(&mut std::process::Command::new("ps"))
                    .args(["-p", &pid.to_string(), "-o", "stat="])
                    .output()
                    .unwrap();
                output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().starts_with('Z')
            };
            assert!(running(browsers[0]));
            if disposition != "engine" {
                let task = tokio::spawn({
                    let browser = browser.clone();
                    let ctx = ctx.clone();
                    async move {
                        let action = if disposition == "close" {
                            json!({"action":"close"})
                        } else {
                            json!({"action":"wait","seconds":30})
                        };
                        browser.call(&ctx, &action).await
                    }
                });
                tokio::time::timeout(Duration::from_secs(30), async {
                    while if disposition == "close" {
                        slot.connected.lock().unwrap().is_some()
                    } else {
                        connected.server.active_requests() == 0
                    } {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(!task.is_finished(), "must cancel during the actual {disposition} wait");
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
                assert!(slot.uncertain.load(Ordering::Acquire));
                assert!(slot.connected.lock().unwrap().is_none());
                assert!(browser.call(&ctx, &json!({"action":"snapshot"})).await.unwrap().is_error);
            }
            // No local observation handle may keep the driver alive past the
            // engine lifetime being tested.
            drop(connected);
            drop(slot);
            drop(browser);
            drop(pool);
            tokio::time::timeout(Duration::from_secs(30), async {
                while running(browsers[0]) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("owned detached Chromium must close after cancellation or engine drop");
        }
        app.abort();
        let _ = app.await;
    }
}
