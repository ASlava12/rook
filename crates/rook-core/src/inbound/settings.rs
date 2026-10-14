//! Operator-defined event handlers. Delivery bodies never configure a task.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub routes: BTreeMap<String, Route>,
    pub max_routes: usize,
    pub body_bytes: usize,
    pub state_bytes: usize,
    pub max_pending: usize,
    pub max_receipts: usize,
    pub timestamp_window_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            routes: BTreeMap::new(),
            max_routes: 8,
            body_bytes: 64 * 1024,
            state_bytes: 16 * 1024 * 1024,
            max_pending: 32,
            max_receipts: 256,
            timestamp_window_secs: 300,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub workspace: String,
    /// Name in the existing vault, never the credential itself.
    pub secret: String,
    /// Literal operator instructions; the payload is attached as separate data.
    pub task: String,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default = "iterations")]
    pub max_iterations: u32,
    #[serde(default = "tokens")]
    pub max_tokens: u64,
    #[serde(default = "seconds")]
    pub max_seconds: u64,
}
fn iterations() -> u32 {
    8
}
fn tokens() -> u64 {
    65536
}
fn seconds() -> u64 {
    1800
}
pub(super) fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
impl Settings {
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (field, value, low, high) in [
            ("max_routes", self.max_routes, 1, 32),
            ("body_bytes", self.body_bytes, 1024, 1024 * 1024),
            ("state_bytes", self.state_bytes, 4096, 64 * 1024 * 1024),
            ("max_pending", self.max_pending, 1, 256),
            ("max_receipts", self.max_receipts, 1, 4096),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("inbound.{field}: expected {low}..={high}"));
            }
        }
        if !(30..=3600).contains(&self.timestamp_window_secs) {
            errors.push("inbound.timestamp_window_secs: expected 30..=3600".into());
        }
        if self.max_pending > self.max_receipts {
            errors.push("inbound.max_receipts must cover max_pending".into());
        }
        if self.routes.len() > self.max_routes {
            errors.push("inbound.routes exceeds max_routes".into());
        }
        for (id, route) in &self.routes {
            if !name(id) || !name(&route.secret) {
                errors.push(format!("inbound.routes.{id}: route and secret names need 1..64 ASCII letters, digits, underscores or hyphens"));
            }
            if route.workspace.len() > 4096
                || !Path::new(&route.workspace).is_absolute()
                || route.workspace.chars().any(char::is_control)
            {
                errors.push(format!("inbound.routes.{id}.workspace: expected a bounded absolute path"));
            }
            if route.task.trim().is_empty() || route.task.len() > 16 * 1024 {
                errors.push(format!(
                    "inbound.routes.{id}.task: expected 1..16384 bytes of literal operator instructions"
                ));
            }
            if !(1..=256).contains(&route.max_iterations)
                || !(1..=5_000_000).contains(&route.max_tokens)
                || !(1..=86400).contains(&route.max_seconds)
            {
                errors.push(format!(
                    "inbound.routes.{id}: iteration, token and time ceilings must be finite and positive"
                ));
            }
        }
        errors
    }
}
