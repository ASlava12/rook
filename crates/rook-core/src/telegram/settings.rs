//! Explicit bot identity, access and transport bounds; no implicit allow-all.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub stance: String,
    pub max_turn_tokens: u64,
    pub max_turn_secs: u64,
    pub max_attempts: u32,
    pub secret: String,
    pub api_url: String,
    pub workspace: String,
    pub allowed_users: Vec<i64>,
    pub allowed_chats: Vec<i64>,
    pub max_chats: usize,
    pub max_updates: usize,
    pub max_outbox: usize,
    pub state_bytes: usize,
    pub response_bytes: usize,
    pub message_bytes: usize,
    pub poll_secs: u64,
    pub request_secs: u64,
    pub callback_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            stance: "assist".into(),
            max_turn_tokens: 65536,
            max_turn_secs: 1800,
            max_attempts: 3,
            secret: String::new(),
            api_url: "https://api.telegram.org".into(),
            workspace: String::new(),
            allowed_users: Vec::new(),
            allowed_chats: Vec::new(),
            max_chats: 128,
            max_updates: 32,
            max_outbox: 128,
            state_bytes: 8 * 1024 * 1024,
            response_bytes: 1024 * 1024,
            message_bytes: 32768,
            poll_secs: 20,
            request_secs: 30,
            callback_secs: 600,
        }
    }
}
impl Settings {
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if rook_tools::policy::Stance::parse(&self.stance).is_none()
            || !(1..=5_000_000).contains(&self.max_turn_tokens)
            || !(1..=86400).contains(&self.max_turn_secs)
            || !(1..=10).contains(&self.max_attempts)
        {
            errors.push(
                "telegram: expected a recognized stance and finite positive turn/delivery budgets".into(),
            );
        }
        for (name, value, low, high) in [
            ("max_chats", self.max_chats, 1, 1024),
            ("max_updates", self.max_updates, 1, 100),
            ("max_outbox", self.max_outbox, 1, 1024),
            ("state_bytes", self.state_bytes, 4096, 64 * 1024 * 1024),
            ("response_bytes", self.response_bytes, 4096, 8 * 1024 * 1024),
            ("message_bytes", self.message_bytes, 1024, 128 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("telegram.{name}: expected {low}..={high}"));
            }
        }
        if !(1..=50).contains(&self.poll_secs)
            || !(1..=120).contains(&self.request_secs)
            || self.request_secs <= self.poll_secs
        {
            errors.push(
                "telegram: poll_secs 1..50 and request_secs 1..120 must leave time for the polling response"
                    .into(),
            );
        }
        if !(30..=3600).contains(&self.callback_secs) {
            errors.push("telegram.callback_secs: expected 30..=3600".into());
        }
        if self.allowed_users.len() > 128
            || self.allowed_chats.len() > 128
            || self.allowed_users.iter().any(|id| *id <= 0)
            || self.allowed_chats.contains(&0)
        {
            errors.push("telegram: access lists hold at most 128 valid user/chat IDs".into());
        }
        let url = reqwest::Url::parse(&self.api_url).ok();
        if url.as_ref().is_none_or(|url| {
            let local = url.host_str().is_some_and(|host| {
                host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
            });
            !matches!(url.scheme(), "https" | "http")
                || (url.scheme() == "http" && !local)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
        }) {
            errors.push("telegram.api_url: expected HTTPS, or HTTP on loopback for protocol fixtures, without embedded credentials/query/fragment".into());
        }
        if self.enabled {
            if self.allowed_users.is_empty() || self.allowed_chats.is_empty() {
                errors.push(
                    "telegram: configure both authorized users and chats before enabling the bot".into(),
                );
            }
            if self.secret.is_empty()
                || self.secret.len() > 64
                || !self.secret.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                errors.push("telegram.secret: expected an existing vault name, never a bot token".into());
            }
            if !std::path::Path::new(&self.workspace).is_absolute()
                || self.workspace.len() > 4096
                || self.workspace.chars().any(char::is_control)
            {
                errors.push("telegram.workspace: expected a bounded absolute workspace path".into());
            }
        }
        errors
    }
}
