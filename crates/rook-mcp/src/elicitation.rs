//! Invocation-owned server requests; user data never resolves a client waiter.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub const MAX_ELICITATION_BYTES: usize = 32768;
const MAX_RESPONSE_BYTES: usize = 16384;
static CONNECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct ElicitationOrigin {
    pub server: String,
    pub connection: u64,
    pub request: u64,
    pub method: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Serialize)]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}
impl ElicitationResponse {
    pub fn cancel() -> Self {
        Self { action: ElicitationAction::Cancel, content: None }
    }
}

#[async_trait]
pub trait Elicitor: Send + Sync {
    /// Called only for the invocation which owns the transport request. Errors
    /// must be generic and must not contain user-entered values.
    async fn elicit(
        &self,
        origin: &ElicitationOrigin,
        params: &Value,
    ) -> std::result::Result<ElicitationResponse, String>;
}

pub struct Interaction {
    pub elicitor: Arc<dyn Elicitor>,
    pub trace: Option<rook_llm::TraceContext>,
}

pub(crate) struct Invocation {
    pub origin: ElicitationOrigin,
    pub handler: Arc<dyn Elicitor>,
    pub alive: AtomicBool,
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
}
impl Invocation {
    pub fn new(
        server: &str,
        connection: u64,
        request: u64,
        method: &str,
        handler: Arc<dyn Elicitor>,
    ) -> Arc<Self> {
        Arc::new(Self {
            origin: ElicitationOrigin { server: server.into(), connection, request, method: method.into() },
            handler,
            alive: AtomicBool::new(true),
            tasks: Mutex::new(Vec::new()),
        })
    }
    pub fn track(&self, task: tokio::task::AbortHandle) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|t| !t.is_finished());
        if self.alive.load(Ordering::Acquire) {
            tasks.push(task);
        } else {
            task.abort();
        }
    }
    pub fn close(&self) {
        self.alive.store(false, Ordering::Release);
        for task in self.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            task.abort();
        }
    }
}

#[derive(Default)]
pub(crate) struct Seen(Mutex<BTreeSet<String>>);
impl Seen {
    pub fn admit(&self, id: &Value) -> bool {
        if !valid_id(id) {
            return false;
        }
        let Ok(id) = serde_json::to_string(id) else { return false };
        let mut ids = self.0.lock().unwrap_or_else(|e| e.into_inner());
        ids.len() < 4096 && ids.insert(id)
    }
}
pub(crate) fn connection() -> u64 {
    CONNECTION.fetch_add(1, Ordering::Relaxed)
}

fn valid_id(id: &Value) -> bool {
    match id {
        Value::String(value) => value.len() <= 128 && !value.chars().any(char::is_control),
        Value::Number(value) => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

pub fn within_elicitation(value: &impl Serialize, limit: usize) -> bool {
    struct Budget(usize);
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::other("elicitation exceeds its byte limit"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(limit), value).is_ok()
}

pub(crate) async fn answer(owner: Option<Arc<Invocation>>, message: crate::protocol::Incoming) -> Value {
    let id = message.id.unwrap_or(Value::Null);
    let error = |code, message| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
    if message.method.as_deref() != Some("elicitation/create") {
        return error(-32601, "unsupported server request");
    }
    if message.result.is_some() || message.error.is_some() {
        return error(-32600, "invalid server request");
    }
    let params = message.params.unwrap_or(Value::Null);
    if !within_elicitation(&params, MAX_ELICITATION_BYTES) {
        return error(-32602, "elicitation exceeds its byte limit");
    }
    let result = match owner {
        Some(owner) if owner.alive.load(Ordering::Acquire) => {
            owner.handler.elicit(&owner.origin, &params).await
        }
        _ => Ok(ElicitationResponse::cancel()),
    };
    let result = result.map(|mut result| {
        if result.action != ElicitationAction::Accept {
            result.content = None;
        }
        result
    });
    match result {
        Ok(result) if within_elicitation(&result, MAX_RESPONSE_BYTES) => {
            json!({"jsonrpc":"2.0","id":id,"result":result})
        }
        Ok(_) => error(-32603, "elicitation response exceeds its byte limit"),
        // A handler is extensible. Do not trust its error to be value-free.
        Err(_) => error(-32602, "unsupported or invalid elicitation"),
    }
}
