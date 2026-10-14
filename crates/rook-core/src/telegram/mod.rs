//! Durable bot routing, authorized inputs and explicitly owned outbound effects.
mod settings;
mod transport;
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use settings::Settings;
pub use transport::{Client, Response};
static CHANGES: std::sync::Mutex<()> = std::sync::Mutex::new(());
const KEY: &str = "telegram/v1";
fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub chat: i64,
    pub topic: i64,
    pub session: String,
    pub owner: i64,
    pub result_seq: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub update: i64,
    pub chat: i64,
    pub topic: i64,
    pub user: i64,
    pub session: String,
    pub text: String,
    pub callback: Option<String>,
    pub callback_query: Option<String>,
    pub state: String,
    pub error: Option<String>,
    pub digest: String,
    pub mode: Option<String>,
    pub target: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outbound {
    pub id: String,
    pub chat: i64,
    pub topic: i64,
    pub session: String,
    pub text: String,
    pub markup: Option<Value>,
    pub state: String,
    pub attempts: u32,
    pub next_at: u64,
    pub message: Option<i64>,
    pub error: Option<String>,
    pub note: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Callback {
    pub token: String,
    pub chat: i64,
    pub topic: i64,
    pub user: i64,
    pub session: String,
    pub request: String,
    pub action: String,
    pub epoch: String,
    pub expires: u64,
    pub state: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    version: u32,
    #[serde(default)]
    adapter_error: Option<String>,
    bot: i64,
    offset: i64,
    routes: Vec<Route>,
    inputs: Vec<Input>,
    outbox: Vec<Outbound>,
    callbacks: Vec<Callback>,
}
fn read(rook: &Rook) -> Result<(Option<Vec<u8>>, Book)> {
    let raw = rook.store.kv_get_limited(KEY, rook.config.telegram.state_bytes)?;
    let book = raw.as_ref().map(|v| serde_json::from_slice::<Book>(v)).transpose()?.unwrap_or(Book {
        version: 1,
        adapter_error: None,
        bot: 0,
        offset: 0,
        routes: Vec::new(),
        inputs: Vec::new(),
        outbox: Vec::new(),
        callbacks: Vec::new(),
    });
    if book.version != 1
        || book.routes.len() > rook.config.telegram.max_chats
        || book.inputs.len() > rook.config.telegram.max_updates * 4
        || book.outbox.len() > rook.config.telegram.max_outbox
        || book.callbacks.len() > rook.config.telegram.max_outbox
    {
        return Err(bad(
            "Telegram retained state exceeds its version/configured bounds; inspect it before changing limits",
        ));
    }
    Ok((raw, book))
}
fn encode(rook: &Rook, book: &Book) -> Result<Vec<u8>> {
    crate::persistence::encode_with_limit(book, rook.config.telegram.state_bytes)
}
fn save(rook: &Rook, old: Option<&[u8]>, book: &Book) -> Result<()> {
    let next = encode(rook, book)?;
    if !rook.store.kv_update_checked(KEY, old, &[(KEY, &next)], None)? {
        return Err(bad("Telegram state changed concurrently; inspect and retry"));
    }
    Ok(())
}
impl Rook {
    pub fn telegram_bot(&self, bot: i64) -> Result<i64> {
        if bot <= 0 {
            return Err(bad("Bot API returned an invalid identity"));
        }
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        if book.bot != 0 && book.bot != bot {
            return Err(bad(
                "configured bot identity changed; retain its routing and delivery history before explicitly resetting the adapter",
            ));
        }
        if book.bot == 0 {
            book.bot = bot;
            save(self, old.as_deref(), &book)?;
        }
        Ok(book.offset)
    }
    /// Persist handling before a later getUpdates request can confirm its offset.
    pub fn telegram_update(&self, update: &Value) -> Result<Option<Input>> {
        let settings = &self.config.telegram;
        if !settings.enabled || !settings.errors().is_empty() {
            return Err(bad("Telegram adapter is disabled or invalid"));
        }
        let id = update["update_id"]
            .as_i64()
            .filter(|v| *v >= 0)
            .ok_or_else(|| bad("invalid Telegram update identity"))?;
        let next = id.checked_add(1).ok_or_else(|| bad("Telegram update identity exhausted"))?;
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        if book.bot == 0 {
            return Err(bad("Verify the configured bot identity before accepting updates"));
        }
        let source = crate::persistence::encode_with_limit(update, settings.response_bytes)?;
        use sha2::Digest;
        let digest = format!("{:x}", sha2::Sha256::digest(&source));
        if let Some(input) = book.inputs.iter().find(|i| i.update == id) {
            if input.digest != digest {
                return Err(bad("Telegram update ID was reused with different content"));
            }
            return Ok(Some(input.clone()));
        }
        if id < book.offset {
            return Ok(None);
        }
        let callback = update.get("callback_query");
        let message = callback.map(|q| &q["message"]).or_else(|| update.get("message"));
        let from = callback.map(|q| &q["from"]).or_else(|| message.map(|m| &m["from"]));
        let user = from.and_then(|f| f["id"].as_i64()).unwrap_or(0);
        let chat = message.and_then(|m| m["chat"]["id"].as_i64()).unwrap_or(0);
        let topic = message.and_then(|m| m["message_thread_id"].as_i64()).unwrap_or(0);
        if !settings.allowed_users.contains(&user)
            || !settings.allowed_chats.contains(&chat)
            || from.is_none_or(|f| f["is_bot"] != false)
            || topic < 0
            || message.is_some_and(|m| m.get("forward_origin").is_some())
        {
            book.offset = next;
            save(self, old.as_deref(), &book)?;
            return Ok(None);
        }
        let text = message.and_then(|m| m["text"].as_str()).unwrap_or("");
        let data = callback.and_then(|q| q["data"].as_str());
        let query = callback.and_then(|q| q["id"].as_str());
        if callback.is_none() && text.is_empty() {
            book.offset = next;
            save(self, old.as_deref(), &book)?;
            return Ok(None);
        }
        if text.len() > settings.message_bytes
            || data.is_some_and(|v| v.is_empty() || v.len() > 64)
            || query.is_some_and(|v| v.is_empty() || v.len() > 256)
            || callback.is_some() && (data.is_none() || query.is_none())
        {
            return Err(bad(
                "Telegram input exceeds supported text/callback bounds; offset remains unconfirmed",
            ));
        }
        if book.inputs.len() >= settings.max_updates * 4
            && let Some(at) =
                book.inputs.iter().position(|i| matches!(i.state.as_str(), "handled" | "failed"))
        {
            book.inputs.remove(at);
        }
        if book.inputs.len() >= settings.max_updates * 4 {
            return Err(bad(
                "Telegram input capacity reached; resolve pending/uncertain updates before polling ahead",
            ));
        }
        let route = book.routes.iter().find(|r| r.chat == chat && r.topic == topic).cloned();
        let fresh = route.is_none();
        if fresh && book.routes.len() >= settings.max_chats {
            return Err(bad("Telegram chat/topic routing capacity reached"));
        }
        if fresh && callback.is_some() {
            book.offset = next;
            save(self, old.as_deref(), &book)?;
            return Ok(None);
        }
        let route = route.unwrap_or_else(|| Route {
            chat,
            topic,
            session: rook_store::format_session_id(rook_store::new_session_id()),
            owner: user,
            result_seq: 0,
        });
        let input = Input {
            update: id,
            chat,
            topic,
            user,
            session: route.session.clone(),
            text: text.into(),
            callback: data.map(str::to_owned),
            callback_query: query.map(str::to_owned),
            state: "pending".into(),
            error: None,
            digest,
            mode: None,
            target: None,
        };
        if fresh {
            book.routes.push(route.clone());
        }
        book.inputs.push(input.clone());
        book.offset = next;
        if fresh {
            let workspace = std::path::Path::new(&settings.workspace)
                .canonicalize()
                .map_err(|e| bad(format!("Telegram workspace unavailable: {e}")))?;
            let session = rook_store::parse_session_id(&route.session)
                .ok_or_else(|| bad("invalid Telegram routing session"))?;
            let mut meta = rook_store::SessionMeta::new(
                session,
                format!("Telegram {chat}/{topic}"),
                workspace.display().to_string(),
                rook_store::now_unix(),
            );
            meta.tags = vec!["rook:telegram".into(), "rook:work".into()];
            let source = crate::persistence::encode_with_limit(
                &json!({"bot":book.bot,"chat":chat,"topic":topic,"user":user,"update":id}),
                4096,
            )?;
            let bytes = encode(self, &book)?;
            if !self.store.create_session_with_companion_checked(
                &meta,
                rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &source)
                    .label("telegram routing"),
                KEY,
                old.as_deref(),
                &bytes,
                settings.state_bytes,
            )? {
                return Err(bad("Telegram routing changed before admission; retry this update"));
            }
        } else {
            save(self, old.as_deref(), &book)?;
        }
        Ok(Some(input))
    }
    pub fn telegram_pending(&self) -> Result<Vec<Input>> {
        Ok(read(self)?.1.inputs.into_iter().filter(|i| i.state == "pending").collect())
    }
    pub fn telegram_routes(&self) -> Result<Vec<Route>> {
        Ok(read(self)?.1.routes)
    }
    pub fn telegram_input_done(
        &self,
        id: i64,
        route: &Route,
        text: &str,
        error: Option<String>,
    ) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let input = book
            .inputs
            .iter_mut()
            .find(|i| i.update == id)
            .ok_or_else(|| bad("Telegram input receipt missing"))?;
        input.state = if error.is_some() { "failed" } else { "handled" }.into();
        input.error = error.map(|e| e.chars().take(512).collect());
        enqueue(self, &mut book, &format!("input-{id}"), route, text, None)?;
        save(self, old.as_deref(), &book)
    }
    /// Capture the dispatch decision before a turn, steering message or stop can have effects.
    pub fn telegram_dispatch(&self, id: i64, mode: &str, target: Option<&str>) -> Result<Input> {
        if !matches!(mode, "prompt" | "steer" | "stop" | "callback" | "status")
            || target.is_some_and(|t| t.len() > 128)
        {
            return Err(bad("Invalid Telegram dispatch target"));
        }
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let input = book
            .inputs
            .iter_mut()
            .find(|i| i.update == id && i.state == "pending")
            .ok_or_else(|| bad("Telegram pending input missing"))?;
        if let Some(chosen) = &input.mode {
            if chosen != mode || input.target.as_deref() != target {
                return Err(bad("Telegram dispatch already belongs to another turn/mode"));
            }
        } else {
            input.mode = Some(mode.into());
            input.target = target.map(str::to_owned);
        }
        let result = input.clone();
        save(self, old.as_deref(), &book)?;
        Ok(result)
    }
    /// Resume observation after process loss, never repeat an in-flight send.
    pub fn telegram_recover(&self) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let mut changed = false;
        for item in &mut book.outbox {
            if item.state == "sending" {
                item.state = "uncertain".into();
                item.error =
                    Some("The sender stopped before retaining a reply; inspect before explicit retry".into());
                changed = true;
            }
        }
        if changed {
            save(self, old.as_deref(), &book)?;
        }
        Ok(())
    }
    pub fn telegram_reply(&self, id: &str, route: &Route, text: &str) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        enqueue(self, &mut book, id, route, text, None)?;
        save(self, old.as_deref(), &book)
    }
    /// The cursor and every bounded output part commit together.
    pub fn telegram_publish(&self, route: &Route, seq: u64, text: &str) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let saved = book
            .routes
            .iter()
            .find(|r| r.session == route.session)
            .ok_or_else(|| bad("Telegram routing missing"))?;
        if seq <= saved.result_seq {
            return Ok(());
        }
        for (part, text) in parts(text).into_iter().enumerate() {
            enqueue(self, &mut book, &format!("result-{}-{seq}-{part}", route.session), route, text, None)?;
        }
        book.routes
            .iter_mut()
            .find(|r| r.session == route.session)
            .ok_or_else(|| bad("Telegram routing missing"))?
            .result_seq = seq;
        save(self, old.as_deref(), &book)
    }
    pub fn telegram_outbox(&self) -> Result<Vec<Outbound>> {
        Ok(read(self)?.1.outbox)
    }
    pub fn telegram_begin_send(&self, id: &str) -> Result<Outbound> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let item = book
            .outbox
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| bad("Telegram outbound item missing"))?;
        if item.state != "pending" || item.next_at > crate::work::managed::now() {
            return Err(bad("Telegram delivery is not due or already has uncertain/sent effects"));
        }
        if item.attempts >= self.config.telegram.max_attempts {
            return Err(bad("Telegram delivery retry budget reached"));
        }
        item.state = "sending".into();
        item.attempts += 1;
        let result = item.clone();
        save(self, old.as_deref(), &book)?;
        Ok(result)
    }
    pub fn telegram_finish_send(&self, id: &str, result: Response) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let item = book
            .outbox
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| bad("Telegram outbound item missing"))?;
        if item.state != "sending" {
            return Err(bad("Telegram delivery changed during its attempt"));
        }
        match result {
            Response::Sent { message } => {
                item.state = "sent".into();
                item.message = Some(message);
                item.error = None;
            }
            Response::Rejected { reason, retry_after } => {
                item.error = Some(reason);
                if let Some(delay) = retry_after.filter(|_| item.attempts < self.config.telegram.max_attempts)
                {
                    item.state = "pending".into();
                    item.next_at = crate::work::managed::now().saturating_add(delay);
                } else {
                    item.state = "failed".into();
                }
            }
            Response::Uncertain => {
                item.state = "uncertain".into();
                item.error = Some(
                    "Bot API response was lost; inspect before explicit retry to avoid duplicate messages"
                        .into(),
                );
            }
        }
        save(self, old.as_deref(), &book)
    }
    /// A live request, its buttons and its delivery are one publication boundary.
    pub fn telegram_notice(
        &self,
        route: &Route,
        request: &str,
        epoch: &str,
        text: &str,
        actions: &[(&str, &str)],
    ) -> Result<()> {
        if request.is_empty()
            || request.len() > 128
            || epoch.is_empty()
            || epoch.len() > 64
            || actions.is_empty()
            || actions.len() > 16
            || actions.iter().any(|(label, action)| label.len() > 128 || action.len() > 256)
        {
            return Err(bad("Telegram callback request exceeds its bound"));
        }
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let now = crate::work::managed::now();
        book.callbacks.retain(|c| c.expires > now);
        if book.outbox.iter().any(|i| i.id == format!("notice-{epoch}-{request}")) {
            return Ok(());
        }
        if book
            .callbacks
            .iter()
            .any(|c| c.session == route.session && c.request == request && c.epoch == epoch)
        {
            return Ok(());
        }
        if book.callbacks.len() + actions.len() > self.config.telegram.max_outbox {
            return Err(bad("Telegram callback capacity reached"));
        }
        let mut buttons = Vec::new();
        for (label, action) in actions {
            let token = rook_store::format_session_id(rook_store::new_session_id());
            buttons.push(json!([{"text":label,"callback_data":token}]));
            book.callbacks.push(Callback {
                token,
                chat: route.chat,
                topic: route.topic,
                user: route.owner,
                session: route.session.clone(),
                request: request.into(),
                action: (*action).into(),
                epoch: epoch.into(),
                expires: now + self.config.telegram.callback_secs,
                state: "pending".into(),
            });
        }
        enqueue(
            self,
            &mut book,
            &format!("notice-{epoch}-{request}"),
            route,
            text,
            Some(json!({"inline_keyboard":buttons})),
        )?;
        save(self, old.as_deref(), &book)
    }
    pub fn telegram_take_callback(&self, input: &Input, epoch: &str) -> Result<Callback> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let token = input.callback.as_deref().ok_or_else(|| bad("Telegram input is not a callback"))?;
        let callback = book
            .callbacks
            .iter_mut()
            .find(|c| c.token == token)
            .ok_or_else(|| bad("Telegram callback is unknown/expired"))?;
        if callback.state != "pending"
            || callback.expires <= crate::work::managed::now()
            || callback.epoch != epoch
            || callback.chat != input.chat
            || callback.topic != input.topic
            || callback.user != input.user
            || callback.session != input.session
        {
            return Err(bad(
                "Telegram callback does not belong to this sender/chat/topic/current request epoch",
            ));
        }
        callback.state = "answered".into();
        let result = callback.clone();
        save(self, old.as_deref(), &book)?;
        Ok(result)
    }
    pub fn telegram_health(&self, error: Option<&str>) -> Result<()> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let next = error.map(|s| s.chars().take(512).collect::<String>());
        if book.adapter_error != next {
            book.adapter_error = next;
            save(self, old.as_deref(), &book)?;
        }
        Ok(())
    }
    pub fn telegram_request(&self, request: Request) -> Result<Value> {
        let _lock = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let (old, mut book) = read(self)?;
        let retry = matches!(&request, Request::Retry { .. });
        match request {
            Request::Status => Ok(
                json!({"enabled":self.config.telegram.enabled,"bot":book.bot,"offset":book.offset,"adapter_error":book.adapter_error,"routes":book.routes.len(),"inputs":book.inputs.len(),"outbox":book.outbox.len(),"uncertain":book.outbox.iter().filter(|i|i.state=="uncertain"||i.state=="sending").count()}),
            ),
            Request::Routes { after } => {
                let mut rows: Vec<_> =
                    book.routes.iter().filter(|r| after.as_ref().is_none_or(|a| r.session > *a)).collect();
                rows.sort_by(|a, b| a.session.cmp(&b.session));
                Ok(
                    json!({"routes":rows.iter().take(16).collect::<Vec<_>>(),"after":rows.get(16).and_then(|_|rows.get(15)).map(|r|&r.session)}),
                )
            }
            Request::Inputs { after } => {
                let rows: Vec<_> =
                    book.inputs.iter().filter(|i| after.is_none_or(|a| i.update > a)).take(17).collect();
                Ok(
                    json!({"inputs":rows.iter().take(16).map(|i|json!({"update":i.update,"session":i.session,"state":i.state,"mode":i.mode,"error":i.error})).collect::<Vec<_>>(),"after":rows.get(16).and_then(|_|rows.get(15)).map(|i|i.update)}),
                )
            }
            Request::Outbox { after } => {
                let mut rows: Vec<_> =
                    book.outbox.iter().filter(|i| after.as_ref().is_none_or(|a| i.id > *a)).collect();
                rows.sort_by(|a, b| a.id.cmp(&b.id));
                let page:Vec<_>=rows.iter().take(16).map(|i|json!({"id":i.id,"session":i.session,"chat":i.chat,"topic":i.topic,"state":i.state,"attempts":i.attempts,"next_at":i.next_at,"message":i.message,"error":i.error,"note":i.note})).collect();
                Ok(json!({"outbox":page,"after":rows.get(16).and_then(|_|rows.get(15)).map(|i|&i.id)}))
            }
            Request::Input { update } => {
                let input = book
                    .inputs
                    .iter()
                    .find(|i| i.update == update)
                    .ok_or_else(|| bad("Telegram input receipt was evicted or is missing"))?;
                Ok(
                    json!({"update":input.update,"chat":input.chat,"topic":input.topic,"user":input.user,"session":input.session,"text":input.text,"state":input.state,"error":input.error,"mode":input.mode,"target":input.target,"digest":input.digest}),
                )
            }
            Request::Inspect { id } => {
                let item = book
                    .outbox
                    .iter()
                    .find(|i| i.id == id)
                    .ok_or_else(|| bad("Telegram outbound item missing"))?;
                Ok(
                    json!({"id":item.id,"session":item.session,"chat":item.chat,"topic":item.topic,"text":item.text,"state":item.state,"attempts":item.attempts,"next_at":item.next_at,"message":item.message,"error":item.error,"note":item.note}),
                )
            }
            Request::Retry { id, attempts, note } | Request::Discard { id, attempts, note } => {
                if note.trim().is_empty() || note.len() > 512 {
                    return Err(bad("Explain the reviewed Telegram delivery decision in 1–512 bytes"));
                }
                let item = book
                    .outbox
                    .iter_mut()
                    .find(|i| i.id == id)
                    .ok_or_else(|| bad("Telegram outbound item missing"))?;
                if item.attempts != attempts
                    || !matches!(item.state.as_str(), "uncertain" | "failed" | "pending")
                {
                    return Err(bad("Telegram delivery changed or is sending/sent; inspect before deciding"));
                }
                if retry && item.state == "pending" {
                    return Err(bad(
                        "Telegram delivery is already queued; respect its retained flood-wait deadline",
                    ));
                }
                if retry && item.attempts >= self.config.telegram.max_attempts {
                    return Err(bad(
                        "Telegram retry budget exhausted; discard or increase the trusted finite budget",
                    ));
                }
                item.state = if retry { "pending" } else { "discarded" }.into();
                item.note = Some(note);
                item.next_at = 0;
                let reply = json!({"id":id,"state":item.state,"attempts":item.attempts});
                save(self, old.as_deref(), &book)?;
                Ok(reply)
            }
            Request::Forget { id, attempts } => {
                let item = book
                    .outbox
                    .iter()
                    .find(|i| i.id == id)
                    .ok_or_else(|| bad("Telegram outbound item missing"))?;
                if item.attempts != attempts || !matches!(item.state.as_str(), "sent" | "discarded") {
                    return Err(bad("Resolve the inspected delivery before forgetting it"));
                }
                book.outbox.retain(|i| i.id != id);
                save(self, old.as_deref(), &book)?;
                Ok(json!({"id":id,"state":"forgotten"}))
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Input { update: i64 },
    Routes { after: Option<String> },
    Inputs { after: Option<i64> },
    Outbox { after: Option<String> },
    Inspect { id: String },
    Retry { id: String, attempts: u32, note: String },
    Discard { id: String, attempts: u32, note: String },
    Forget { id: String, attempts: u32 },
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 8192 {
        return Err(bad("Telegram request exceeds 8 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::Status);
    }
    Ok(serde_json::from_str(text)?)
}
fn parts(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (index, c) in text.char_indices() {
        if units + c.len_utf16() > 4000 {
            result.push(&text[start..index]);
            start = index;
            units = 0;
        }
        units += c.len_utf16();
    }
    result.push(&text[start..]);
    result
}
fn enqueue(
    rook: &Rook,
    book: &mut Book,
    id: &str,
    route: &Route,
    text: &str,
    markup: Option<Value>,
) -> Result<()> {
    if id.is_empty()
        || id.len() > 256
        || text.is_empty()
        || text.len() > rook.config.telegram.message_bytes
        || text.encode_utf16().count() > 4096
    {
        return Err(bad(
            "Telegram outbound identity/text exceeds its bound; split the response before sending",
        ));
    }
    if let Some(item) = book.outbox.iter().find(|i| i.id == id) {
        if item.text != text
            || item.chat != route.chat
            || item.topic != route.topic
            || item.session != route.session
            || item.markup != markup
        {
            return Err(bad("Telegram outbound identity belongs to different content"));
        }
        return Ok(());
    }
    if book.outbox.len() >= rook.config.telegram.max_outbox {
        return Err(bad("Telegram outbox capacity reached; inspect and forget resolved deliveries"));
    }
    if let Some(value) = &markup {
        crate::persistence::encode_with_limit(value, 8192)?;
    }
    book.outbox.push(Outbound {
        id: id.into(),
        chat: route.chat,
        topic: route.topic,
        session: route.session.clone(),
        text: text.into(),
        markup,
        state: "pending".into(),
        attempts: 0,
        next_at: 0,
        message: None,
        error: None,
        note: None,
    });
    Ok(())
}
