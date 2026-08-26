//! How a request reaches a server.
//!
//! Two shapes: a subprocess speaking newline-delimited JSON-RPC on its pipes,
//! and an HTTP endpoint. They differ in more than plumbing — stdio needs a
//! pending-request table because responses arrive on a shared pipe, while HTTP
//! answers each POST directly — so the trait is at the level of a whole
//! request rather than of writing a line.

use std::time::Duration;

use async_trait::async_trait;

use crate::Result;
use crate::protocol::Incoming;

#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Incoming>;

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()>;

    async fn shutdown(&self);
}
