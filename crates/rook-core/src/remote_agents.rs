//! Rook-to-Rook requests over authenticated HTTP and durable scoped ingress.
mod transport;
use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub(crate) use transport::Pool;
pub(crate) const TOOL: &str = "remote_agent";
pub(crate) const DOMAIN: &[u8] = b"rook-a2a-v1\0";
pub(crate) const REPLY_DOMAIN: &[u8] = b"rook-a2a-reply-v1\0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub peers: BTreeMap<String, Peer>,
    pub routes: std::collections::BTreeSet<String>,
    /// Explicit additional consent to bearer HTTP API and MCP access.
    pub api_routes: std::collections::BTreeSet<String>,
    pub max_peers: usize,
    pub max_tracked_tasks: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub timeout_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            peers: BTreeMap::new(),
            routes: Default::default(),
            api_routes: Default::default(),
            max_peers: 16,
            max_tracked_tasks: 32,
            request_bytes: 32768,
            response_bytes: 65536,
            timeout_secs: 30,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub url: String,
    pub route: String,
    pub secret: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allow_http: bool,
}
pub(crate) fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
impl Peer {
    pub(crate) fn endpoint(&self) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.url).map_err(|_| bad("invalid remote agent URL"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
        if self.url.len() > 4096
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.scheme() == "http" && !loopback && !self.allow_http)
            || !name(&self.route)
            || !name(&self.secret)
        {
            return Err(bad(
                "remote agent requires an HTTP(S) URL without credentials/query/fragment, bounded route/secret names, and HTTPS for non-loopback hosts unless allow_http is explicit",
            ));
        }
        let path = format!("{}/api/a2a/{}", url.path().trim_end_matches('/'), self.route);
        url.set_path(&path);
        Ok(url)
    }
}
impl Settings {
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (field, value, low, high) in [
            ("max_peers", self.max_peers, 1, 32),
            ("max_tracked_tasks", self.max_tracked_tasks, 1, 256),
            ("request_bytes", self.request_bytes, 1024, 65536),
            ("response_bytes", self.response_bytes, 32768, 1024 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("remote_agents.{field}: expected {low}..={high}"));
            }
        }
        if !(1..=120).contains(&self.timeout_secs) {
            errors.push("remote_agents.timeout_secs: expected 1..=120".into());
        }
        if self.peers.len() > self.max_peers {
            errors.push("remote_agents.peers exceeds max_peers".into());
        }
        if self.routes.len() > self.max_peers || self.routes.iter().any(|route| !name(route)) {
            errors.push(
                "remote_agents.routes must contain at most max_peers bounded inbound route names".into(),
            );
        }
        if !self.api_routes.is_subset(&self.routes) {
            errors.push(
                "remote_agents.api_routes must be a subset of explicitly enabled remote_agents.routes".into(),
            );
        }
        for (id, peer) in &self.peers {
            if !name(id)
                || peer.description.len() > 512
                || peer.description.chars().any(char::is_control)
                || peer.endpoint().is_err()
            {
                errors.push(format!("remote_agents.peers.{id}: invalid bounded peer configuration"));
            }
        }
        errors
    }
    pub(crate) fn peers(&self) -> Value {
        json!({"protocol":"rook-a2a-v1","peers":self.peers.iter().take(self.max_peers.min(32))
            .map(|(id,p)|json!({"peer":id,"url":p.url,"route":p.route,"description":p.description})).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Info,
    Submit {
        text: String,
    },
    Status {
        id: String,
    },
    Message {
        id: String,
        text: String,
    },
    Result {
        id: String,
        #[serde(default)]
        offset: usize,
        source: Option<String>,
    },
    Cancel {
        id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub version: u32,
    pub sender: String,
    pub operation: Operation,
}
impl Operation {
    pub(crate) fn writes(&self) -> bool {
        matches!(self, Self::Submit { .. } | Self::Message { .. } | Self::Cancel { .. })
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let (id, text) = match self {
            Self::Info => (None, None),
            Self::Submit { text } => (None, Some(text)),
            Self::Message { id, text } => (Some(id), Some(text)),
            Self::Status { id } | Self::Result { id, .. } | Self::Cancel { id } => (Some(id), None),
        };
        if id.is_some_and(|id| rook_store::parse_session_id(id).is_none())
            || text.is_some_and(|s| s.trim().is_empty() || s.len() > 16384)
        {
            return Err(bad("remote requests need a returned receipt ID and 1..16384 UTF-8 bytes of text"));
        }
        if let Self::Result { source: Some(source), .. } = self
            && (source.len() != 64 || !source.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(bad("result source must be the returned digest"));
        }
        Ok(())
    }

    fn admit_response(&self, value: &Value, route: &str, delivery: &str) -> Result<()> {
        let correct_id = |id: &str| value["id"].as_str() == Some(id);
        let valid = match self {
            Self::Info => {
                value["protocol"] == "rook-a2a-v1"
                    && value["route"] == route
                    && value["capabilities"].is_array()
            }
            Self::Submit { .. } => {
                value["id"].as_str().and_then(rook_store::parse_session_id).is_some()
                    && value["route"] == route
                    && value["delivery"] == delivery
                    && value["duplicate"].is_boolean()
            }
            Self::Status { id } => {
                correct_id(id)
                    && value["route"] == route
                    && (value["work"].is_null()
                        || serde_json::from_value::<rook_proto::work::Status>(
                            value["work"]["status"].clone(),
                        )
                        .is_ok())
            }
            Self::Message { id, .. } => {
                correct_id(id) && value["message"]["id"].is_string() && value["message"]["text"].is_string()
            }
            Self::Cancel { id } => correct_id(id) && value["control"].is_object(),
            Self::Result { id, offset, source } => {
                let text = value["text"].as_str();
                let total = value["total_bytes"].as_u64().and_then(|n| usize::try_from(n).ok());
                let digest = value["source"].as_str();
                correct_id(id)
                    && value["offset"] == json!(offset)
                    && digest.is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    && source.as_ref().is_none_or(|source| Some(source.as_str()) == digest)
                    && text.zip(total).is_some_and(|(text, total)| {
                        text.len() <= 4096
                            && offset.checked_add(text.len()).is_some_and(|end| {
                                end <= total
                                    && if end < total {
                                        value["next_offset"] == json!(end)
                                    } else {
                                        value["next_offset"].is_null()
                                    }
                            })
                    })
            }
        };
        if !valid {
            return Err(bad(
                "authenticated peer reply does not confirm the requested operation; inspect its receipt before proceeding",
            ));
        }
        Ok(())
    }
}
pub(crate) fn signature(
    domain: &[u8],
    route: &str,
    id: &str,
    timestamp: &str,
    body: &[u8],
    secret: &[u8],
) -> Result<String> {
    use hmac::{Hmac, Mac};
    if !(16..=4096).contains(&secret.len()) {
        return Err(bad("A2A credential must contain 16..4096 bytes"));
    }
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret).map_err(|_| bad("invalid A2A credential"))?;
    mac.update(domain);
    for field in [route, id, timestamp] {
        mac.update(field.as_bytes());
        mac.update(b"\0");
    }
    mac.update(body);
    Ok(format!("v1={}", hex::encode(mac.finalize().into_bytes())))
}
pub(crate) fn reply_mac(
    route: &str,
    id: &str,
    timestamp: &str,
    request_hash: &[u8],
    body: &[u8],
    secret: &[u8],
) -> Result<hmac::Hmac<sha2::Sha256>> {
    use hmac::Mac;
    if !(16..=4096).contains(&secret.len()) || request_hash.len() != 32 {
        return Err(bad("invalid A2A reply authentication inputs"));
    }
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret)
        .map_err(|_| bad("invalid A2A reply credential"))?;
    mac.update(REPLY_DOMAIN);
    for field in [route, id, timestamp] {
        mac.update(field.as_bytes());
        mac.update(b"\0");
    }
    mac.update(request_hash);
    mac.update(body);
    Ok(mac)
}
impl crate::Rook {
    pub(crate) async fn remote_agent_request(
        &self,
        vault: &crate::Vault,
        session: u128,
        peer_name: &str,
        request_id: Option<&str>,
        mut operation: Operation,
    ) -> Result<Value> {
        let settings = &self.config.remote_agents;
        if !settings.enabled || !settings.errors().is_empty() || self.config.sandbox.network_policy.enabled {
            return Err(bad("remote agents are disabled, invalid, or excluded by domain network policy"));
        }
        operation.validate()?;
        let peer = settings.peers.get(peer_name).ok_or_else(|| bad("unknown configured remote agent"))?;
        if operation.writes() && !request_id.is_some_and(name) {
            return Err(bad(
                "remote writes need a stable request name (1..64 ASCII letters, digits, underscores or hyphens); repeat the same name and arguments to retry",
            ));
        }
        // Resolve before redacting text: an env/password-manager credential
        // becomes a known redaction only when it has actually been obtained.
        let secret = vault
            .value(&peer.secret)
            .ok_or_else(|| bad("configured remote agent credential is unavailable"))?;
        if let Operation::Submit { text } | Operation::Message { text, .. } = &mut operation {
            *text = vault.redact(text);
        }
        operation.validate()?;
        let sender = rook_store::format_session_id(session);
        let body = crate::persistence::encode_with_limit(
            &Envelope { version: 1, sender: sender.clone(), operation: operation.clone() },
            settings.request_bytes,
        )?;
        let delivery = format!(
            "a2a:{sender}:{}",
            request_id.map(str::to_owned).unwrap_or_else(|| rook_store::ObjectId::of(&body).to_hex())
        );
        let timestamp = crate::work::managed::now().to_string();
        let request_hash = rook_store::ObjectId::of(&body);
        let signed = signature(DOMAIN, &peer.route, &delivery, &timestamp, &body, secret.as_bytes())?;
        let endpoint = peer.endpoint()?;
        let client =
            self.remote_agents.client(&endpoint, &self.config.proxy.for_mcp(), settings.max_peers)?;
        let request = client
            .post(endpoint)
            .header("content-type", "application/json")
            .header("x-rook-delivery", &delivery)
            .header("x-rook-timestamp", &timestamp)
            .header("x-rook-signature", signed)
            .timeout(std::time::Duration::from_secs(settings.timeout_secs))
            .body(body)
            .send();
        match transport::response(
            request,
            settings.response_bytes,
            &peer.route,
            &delivery,
            &timestamp,
            secret.as_bytes(),
            request_hash.as_bytes(),
        )
        .await
        {
            Ok(value) => {
                operation.admit_response(&value, &peer.route, &delivery)?;
                Ok(json!({"peer":peer_name,"delivery":delivery,"response":value}))
            }
            Err(error) => Err(bad(format!(
                "remote peer {peer_name}: {error}; delivery={delivery}. A write may already be accepted; retry only the same peer, request name and arguments, then inspect its status"
            ))),
        }
    }
}
