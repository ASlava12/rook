//! Configuration only: the engine owns browser interaction and private storage.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthConfig {
    /// A public, pre-registered client, or an HTTPS client metadata document.
    /// Empty requests dynamic registration when the issuer supports it.
    pub client_id: String,
    /// Select one of the resource's advertised issuers. Empty selects the first.
    pub issuer: String,
    /// An explicit scope set when the resource's challenge supplies none.
    pub scopes: Vec<String>,
    /// Zero lets the OS choose the native browser callback port.
    pub callback_port: u16,
    pub max_response_bytes: usize,
    pub timeout_secs: u64,
    pub login_timeout_secs: u64,
}
impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            issuer: String::new(),
            scopes: Vec::new(),
            callback_port: 0,
            max_response_bytes: 256 * 1024,
            timeout_secs: 30,
            login_timeout_secs: 600,
        }
    }
}
impl OAuthConfig {
    pub fn error(&self) -> Option<&'static str> {
        if !(4096..=1048576).contains(&self.max_response_bytes) {
            return Some("oauth.max_response_bytes: expected 4096..=1048576");
        }
        if !(1..=120).contains(&self.timeout_secs) {
            return Some("oauth.timeout_secs: expected 1..=120");
        }
        if !(30..=1800).contains(&self.login_timeout_secs) {
            return Some("oauth.login_timeout_secs: expected 30..=1800");
        }
        if self.client_id.len() > 2048
            || self.issuer.len() > 2048
            || self.scopes.len() > 64
            || self.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 256
                    || !scope
                        .bytes()
                        .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
            })
        {
            return Some(
                "oauth: client/issuer must be at most 2048 bytes; use at most 64 valid scopes of at most 256 bytes",
            );
        }
        None
    }
}

/// Resolved immediately before an HTTP request, so an hours-long turn does not
/// retain an expired access token. Implementations must never return raw errors.
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self) -> std::result::Result<String, &'static str>;
}
