//! Credential-scoped agent automation shared by HTTP and MCP ingress.
//! Admission reuses the signed peer protocol and the existing durable supervisor.
use crate::remote_agents::{DOMAIN, Envelope, Operation, name, signature};
use crate::{CoreError, Result, Rook, Vault};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    request: Option<String>,
    operation: Operation,
}

fn auth_error() -> CoreError {
    CoreError::InboundAuthentication("agent API route or credential is unavailable".into())
}

impl Rook {
    /// Authenticate protocol discovery without requiring a potentially large
    /// worker description to fit a response envelope.
    pub fn authorize_agent_api(&self, route: &str, credential: &str, vault: &Vault) -> Result<()> {
        self.agent_api_credential(route, credential, vault).map(|_| ())
    }

    fn agent_api_credential(&self, route: &str, credential: &str, vault: &Vault) -> Result<String> {
        let settings = &self.config.remote_agents;
        if !settings.enabled
            || !settings.api_routes.contains(route)
            || !settings.errors().is_empty()
            || !self.config.inbound.enabled
            || !self.config.inbound.errors().is_empty()
        {
            return Err(auth_error());
        }
        let handler = self.config.inbound.routes.get(route).ok_or_else(auth_error)?;
        let secret = vault.value(&handler.secret).ok_or_else(auth_error)?;
        if !(16..=4096).contains(&secret.len()) || !(16..=4096).contains(&credential.len()) {
            return Err(auth_error());
        }
        // Compare fixed-length authenticators in constant time rather than
        // comparing secret strings or exposing a timing prefix oracle.
        let mut candidate =
            Hmac::<sha2::Sha256>::new_from_slice(credential.as_bytes()).map_err(|_| auth_error())?;
        let mut expected =
            Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| auth_error())?;
        candidate.update(b"rook-agent-bearer-v1");
        expected.update(b"rook-agent-bearer-v1");
        expected.verify_slice(&candidate.finalize().into_bytes()).map_err(|_| auth_error())?;
        Ok(secret)
    }

    /// A bearer credential grants only its explicitly enabled route. The payload
    /// cannot choose a workspace, policy, provider, autonomy or spending limit.
    pub fn receive_agent_api(
        &self,
        route: &str,
        credential: &str,
        body: &[u8],
        vault: &Vault,
    ) -> Result<Value> {
        let secret = self.agent_api_credential(route, credential, vault)?;
        let settings = &self.config.remote_agents;
        let limit = settings.request_bytes.min(self.config.inbound.body_bytes);
        if body.len() > limit {
            return Err(CoreError::Other("agent API request exceeds its configured byte limit".into()));
        }
        let request: Request = serde_json::from_slice(body)?;
        request.operation.validate()?;
        if request.operation.writes() && !request.request.as_deref().is_some_and(name) {
            return Err(CoreError::Other("agent writes require a stable request name of 1..64 ASCII letters, digits, underscores or hyphens; repeat the original name and arguments after an uncertain reply".into()));
        }
        if !request.operation.writes() && request.request.is_some() {
            return Err(CoreError::Other("read operations do not accept a request name".into()));
        }
        // This identity denotes the authenticated integration route, not an
        // external caller's claimed session. It is stable across transport retries.
        let hash = rook_store::ObjectId::of(format!("rook-agent-api-v1:{route}").as_bytes());
        let mut identity = [0; 16];
        identity.copy_from_slice(&hash.as_bytes()[..16]);
        let envelope = Envelope {
            version: 1,
            sender: rook_store::format_session_id(u128::from_be_bytes(identity)),
            operation: request.operation,
        };
        let canonical = crate::persistence::encode_with_limit(&envelope, limit)?;
        let delivery = format!(
            "api:{}",
            request.request.unwrap_or_else(|| rook_store::ObjectId::of(&canonical).to_hex())
        );
        let timestamp = crate::work::managed::now().to_string();
        let signed = signature(DOMAIN, route, &delivery, &timestamp, &canonical, secret.as_bytes())?;
        let mut response = self.receive_agent_request(
            crate::inbound::Delivery {
                route,
                id: &delivery,
                timestamp: &timestamp,
                signature: &signed,
                body: &canonical,
            },
            vault,
        )?;
        response["protocol"] = json!("rook-agent-api-v1");
        response["delivery"] = json!(delivery);
        Ok(response)
    }

    /// Enforce the whole HTTP or JSON-RPC envelope, including JSON escaping.
    pub fn encode_agent_api_response(&self, value: &Value) -> Result<Vec<u8>> {
        crate::persistence::encode_with_limit(value, self.config.remote_agents.response_bytes)
    }
}

/// Agent operations offered by the HTTP MCP server. Runtime admission still
/// validates each operation with the same typed parser as the native transport.
pub fn tools() -> Vec<Value> {
    [
        ("info","Read the worker's configured scope and capabilities.",false,false,false),
        ("submit","Submit a durable task. Keep the request name and arguments unchanged when retrying. Worker policy and budgets apply.",true,false,true),
        ("status","Inspect this route's task receipt. Pending means it has not yet been admitted by the supervisor.",false,true,false),
        ("message","Send source data to the task's original generation. This does not grant permissions or resume paused work.",true,true,true),
        ("result","Read a redacted result page. Continue with the returned offset and source digest; stopped status determines finality.",false,true,false),
        ("cancel","Cancel the task's original generation. Keep the request name unchanged on retry.",true,true,false),
    ].into_iter().map(|(action,description,writes,identified,text)|{
        let mut properties=serde_json::Map::new();let mut required=Vec::new();
        if writes {properties.insert("request".into(),json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9_-]+$","description":"Unique stable idempotency name. Reuse only with exactly the same operation and arguments."}));required.push("request");}
        if identified {properties.insert("id".into(),json!({"type":"string","maxLength":26,"description":"Receipt id returned by rook_submit."}));required.push("id");}
        if text {properties.insert("text".into(),json!({"type":"string","minLength":1,"maxLength":16384,"description":"At most 16384 UTF-8 bytes, subject to the escaped request byte limit."}));required.push("text");}
        if action=="result" {properties.insert("offset".into(),json!({"type":"integer","minimum":0}));properties.insert("source".into(),json!({"type":"string","minLength":64,"maxLength":64}));}
        json!({"name":format!("rook_{action}"),"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":!writes,"destructiveHint":writes,"idempotentHint":true,"openWorldHint":writes}})
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "agent-api-fixture-private-key";
    fn fixture() -> (tempfile::TempDir, Rook, Vault) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut config = crate::Config::default();
        config.remote_agents.enabled = true;
        config.remote_agents.routes = ["alpha".into(), "beta".into()].into();
        config.remote_agents.api_routes = config.remote_agents.routes.clone();
        config.inbound.enabled = true;
        let route = crate::inbound::Route {
            workspace: workspace.canonicalize().unwrap().display().to_string(),
            secret: "agent_key".into(),
            task: "Handle operation.text in this configured workspace.".into(),
            autonomous: false,
            max_iterations: 2,
            max_tokens: 10000,
            max_seconds: 120,
        };
        config.inbound.routes = [("alpha".into(), route.clone()), ("beta".into(), route)].into();
        let rook = Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "test"),
            rook_skills::SkillIndex::default(),
            workspace,
        );
        let mut vault = Vault::load_from(root.path().join("secrets.toml")).unwrap();
        vault.keep("agent_key", KEY).unwrap();
        (root, rook, vault)
    }
    fn call(rook: &Rook, vault: &Vault, route: &str, value: Value) -> Result<Value> {
        rook.receive_agent_api(route, KEY, &serde_json::to_vec(&value).unwrap(), vault)
    }

    #[test]
    fn bearer_automation_needs_explicit_opt_in_and_valid_authentication_before_any_admission() {
        let (_root, mut rook, vault) = fixture();
        let submit = json!({"request":"one","operation":{"action":"submit","text":"Inspect the project"}});
        let body = serde_json::to_vec(&submit).unwrap();
        for credential in ["wrong", "wrong-but-long-enough-credential"] {
            assert!(matches!(
                rook.receive_agent_api("alpha", credential, &body, &vault),
                Err(CoreError::InboundAuthentication(_))
            ));
        }
        assert!(rook.pending_events().unwrap().is_empty());
        rook.config.remote_agents.api_routes.clear();
        assert!(call(&rook, &vault, "alpha", submit.clone()).is_err());
        rook.config.remote_agents.api_routes.insert("alpha".into());
        assert!(call(&rook,&vault,"alpha",json!({"request":"one","workspace":"/elsewhere","operation":{"action":"submit","text":"spoof"}})).is_err());
        assert!(
            call(
                &rook,
                &vault,
                "alpha",
                json!({"request":"one","operation":{"action":"submit","text":"spoof","max_tokens":999999}})
            )
            .is_err()
        );
        assert!(
            call(&rook, &vault, "alpha", json!({"operation":{"action":"submit","text":"no request"}}))
                .is_err()
        );
        assert!(rook.pending_events().unwrap().is_empty());
        rook.config.remote_agents.request_bytes = 1024;
        let oversized = serde_json::to_vec(
            &json!({"request":"large","operation":{"action":"submit","text":"x".repeat(2000)}}),
        )
        .unwrap();
        assert!(oversized.len() > rook.config.remote_agents.request_bytes);
        assert!(rook.receive_agent_api("alpha", KEY, &oversized, &vault).is_err());
        assert!(rook.pending_events().unwrap().is_empty());
        rook.config.remote_agents.response_bytes = 32768;
        let large_reply = json!({"text":"\n".repeat(20000)});
        assert!(serde_json::to_vec(&large_reply).unwrap().len() > rook.config.remote_agents.response_bytes);
        assert!(rook.encode_agent_api_response(&large_reply).is_err());
    }

    #[test]
    fn api_retries_keep_one_durable_task_across_reopen_and_cannot_read_a_different_route() {
        let (root, rook, vault) = fixture();
        let submit = json!({"request":"one","operation":{"action":"submit","text":format!("Inspect {KEY}")}});
        let first = call(&rook, &vault, "alpha", submit.clone()).unwrap();
        assert_eq!(first["protocol"], "rook-agent-api-v1");
        assert_eq!(first["result"]["duplicate"], false);
        let id = first["result"]["id"].as_str().unwrap().to_string();
        assert!(!serde_json::to_string(&rook.pending_events().unwrap()).unwrap().contains(KEY));
        let config = rook.config.clone();
        let workspace = rook.workspace.clone();
        drop(rook);
        let reopened = Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "test"),
            rook_skills::SkillIndex::default(),
            workspace,
        );
        let second = call(&reopened, &vault, "alpha", submit).unwrap();
        assert_eq!(second["result"]["id"], id);
        assert_eq!(second["result"]["duplicate"], true);
        assert_eq!(reopened.pending_events().unwrap().len(), 1);
        assert!(
            call(
                &reopened,
                &vault,
                "alpha",
                json!({"request":"one","operation":{"action":"submit","text":"changed"}})
            )
            .is_err()
        );
        assert!(call(&reopened, &vault, "beta", json!({"operation":{"action":"status","id":id}})).is_err());
        let status =
            call(&reopened, &vault, "alpha", json!({"operation":{"action":"status","id":id}})).unwrap();
        assert_eq!(status["result"]["state"], "pending");
    }
}
