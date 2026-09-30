//! A session's ordinary and goal receipts in one bounded view.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    pub after: Option<String>,
    pub include_finished: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Opaque reference, including the goal generation where applicable.
    pub reference: String,
    pub receipt: crate::work::Steering,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Entry>,
    pub next: Option<String>,
    pub total: usize,
    pub max_message_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Edit { reference: String, revision: u64, text: String },
    Withdraw { reference: String, revision: u64 },
}
