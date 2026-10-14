use super::*;
use futures_util::StreamExt;
#[derive(Clone)]
pub struct Client {
    settings: Settings,
    token: String,
    http: reqwest::Client,
}
#[derive(Clone, Debug)]
pub enum Response {
    Sent { message: i64 },
    Rejected { reason: String, retry_after: Option<u64> },
    Uncertain,
}
impl Client {
    pub fn new(settings: Settings, vault: &crate::Vault) -> Result<Self> {
        if !settings.enabled || !settings.errors().is_empty() {
            return Err(bad("Telegram is disabled or has invalid trusted configuration"));
        }
        let token = vault
            .value(&settings.secret)
            .filter(|t| {
                !t.is_empty()
                    && t.len() <= 4096
                    && t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            })
            .ok_or_else(|| bad("Telegram bot credential is unavailable or malformed"))?;
        rook_llm::init_tls();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(settings.request_secs))
            .build()
            .map_err(|_| bad("Telegram client could not be prepared"))?;
        Ok(Self { settings, token, http })
    }
    async fn call(&self, method: &str, body: &Value) -> Result<Value> {
        crate::persistence::encode_with_limit(body, self.settings.response_bytes)?;
        let url = format!("{}/bot{}/{}", self.settings.api_url.trim_end_matches('/'), self.token, method);
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|_| bad("Telegram transport failed; no credential URL is retained"))?;
        let mut bytes = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| bad("Telegram response interrupted"))?;
            if chunk.len() > self.settings.response_bytes.saturating_sub(bytes.len()) {
                return Err(bad("Telegram response exceeded response_bytes while arriving"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| bad("Telegram returned an invalid bounded response"))
    }
    pub async fn identity(&self) -> Result<i64> {
        let reply = self.call("getMe", &json!({})).await?;
        if reply["ok"] != true {
            return Err(bad("Telegram getMe was rejected"));
        }
        reply["result"]["id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or_else(|| bad("Telegram identity response is invalid"))
    }
    pub async fn updates(&self, offset: i64) -> Result<Vec<Value>> {
        let reply=self.call("getUpdates",&json!({"offset":offset,"limit":self.settings.max_updates,"timeout":self.settings.poll_secs,"allowed_updates":["message","callback_query"]})).await?;
        if reply["ok"] != true {
            return Err(bad(
                "Telegram polling was rejected; check bot credentials and existing webhook ownership",
            ));
        }
        let mut updates = reply["result"]
            .as_array()
            .filter(|a| a.len() <= self.settings.max_updates)
            .ok_or_else(|| bad("Telegram update count exceeded its bound"))?
            .clone();
        if updates.iter().any(|u| u["update_id"].as_i64().is_none_or(|id| id < 0 || id == i64::MAX)) {
            return Err(bad("Telegram returned invalid update identities; no offset was confirmed"));
        }
        updates.sort_by_key(|u| u["update_id"].as_i64());
        Ok(updates)
    }
    pub async fn send(&self, item: &Outbound) -> Response {
        let mut body = json!({"chat_id":item.chat,"text":item.text});
        if item.topic != 0 {
            body["message_thread_id"] = json!(item.topic);
        }
        if let Some(markup) = &item.markup {
            body["reply_markup"] = markup.clone();
        }
        match self.call("sendMessage", &body).await {
            Ok(reply) if reply["ok"] == true => reply["result"]["message_id"]
                .as_i64()
                .map(|message| Response::Sent { message })
                .unwrap_or(Response::Uncertain),
            Ok(reply) if reply["ok"] == false => Response::Rejected {
                reason: "Bot API rejected the message".into(),
                retry_after: reply["parameters"]["retry_after"].as_u64().filter(|v| *v <= 3600),
            },
            _ => Response::Uncertain,
        }
    }
    pub async fn answer_callback(&self, id: &str, text: &str) -> Result<()> {
        let reply = self
            .call(
                "answerCallbackQuery",
                &json!({"callback_query_id":id,"text":text.chars().take(180).collect::<String>()}),
            )
            .await?;
        if reply["ok"] != true {
            return Err(bad("Telegram callback acknowledgement was rejected"));
        }
        Ok(())
    }
}
