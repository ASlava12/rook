//! A side question is separate from prompt admission and turn control.
use serde::{Deserialize, Serialize};

pub const MAX_QUESTION_BYTES: usize = 8192;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub question: String,
    #[serde(default)]
    pub model: Option<String>,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.question.trim().is_empty() || self.question.len() > MAX_QUESTION_BYTES {
            return Err("a side question needs 1..8192 bytes of text");
        }
        if self.model.as_ref().is_some_and(|name| name.trim().is_empty() || name.len() > 256) {
            return Err("a side question model needs 1..256 bytes");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Complete { answer: String },
    Failed { error: String },
}
