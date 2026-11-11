use super::{AgentLoop, effects::Shown};
use crate::{
    CoreError, Result,
    remote_agents::{Operation, TOOL},
};
use serde_json::{Value, json};
#[derive(Default)]
pub(super) struct Tasks {
    started: std::collections::BTreeSet<(u128, String, String)>,
    live: std::collections::VecDeque<(u128, String, String)>,
}

impl AgentLoop<'_> {
    pub(super) fn remote_live_poll(&self, call: &rook_llm::ToolCall) -> bool {
        if call.name != TOOL || call.arguments.get("action").and_then(Value::as_str) != Some("status") {
            return false;
        }
        let Some(peer) = call.arguments.get("peer").and_then(Value::as_str) else {
            return false;
        };
        let Some(id) = call.arguments.get("id").and_then(Value::as_str) else {
            return false;
        };
        self.remote_tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .live
            .iter()
            .any(|(session, p, r)| *session == self.session && p == peer && r == id)
    }
    pub(super) async fn remote_agent(&self, args: &Value) -> Result<Value> {
        if !self.rook.config.remote_agents.enabled
            || !self.rook.config.remote_agents.errors().is_empty()
            || self.rook.config.sandbox.network_policy.enabled
        {
            return Err(CoreError::Other(
                "remote agents are disabled, invalid, or excluded by domain network policy".into(),
            ));
        }
        let fields =
            args.as_object().ok_or_else(|| CoreError::Other("remote_agent expects an object".into()))?;
        let mut operation = fields.clone();
        let peer = operation.remove("peer");
        let request = operation.remove("request");
        if operation.get("action").and_then(Value::as_str) == Some("peers") {
            if operation.len() != 1 || peer.is_some() || request.is_some() {
                return Err(CoreError::Other("peers accepts only action".into()));
            }
            return Ok(self.rook.config.remote_agents.peers());
        }
        let operation: Operation = serde_json::from_value(json!(operation))?;
        operation.validate()?;
        let peer = peer
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Other("remote operation requires peer".into()))?;
        let request = request
            .as_ref()
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| crate::remote_agents::name(name))
                    .ok_or_else(|| CoreError::Other("request must be a stable bounded name".into()))
            })
            .transpose()?;
        if operation.writes() && request.is_none() {
            return Err(CoreError::Other("submit, message and cancel require a stable request name".into()));
        }
        let target = self
            .rook
            .config
            .remote_agents
            .peers
            .get(peer)
            .ok_or_else(|| CoreError::Other("unknown configured remote agent peer".into()))?;
        let endpoint = target.endpoint()?;
        let risk = rook_tools::policy::Risk::External {
            name: format!("Rook peer {peer} at {endpoint}"),
            claims_read_only: !operation.writes(),
        };
        if let Some(reason)=self.gate_risk(TOOL,args,risk,Shown::Text("Contact this configured Rook peer. Remote tasks run under that computer's configured workspace, permissions and budget; their costs are separate from this local turn.")).await {
            return Err(CoreError::Other(reason));
        }
        if matches!(operation, Operation::Submit { .. }) {
            let key = (self.session, peer.to_string(), request.expect("validated remote write").to_string());
            let mut tasks = self.remote_tasks.lock().unwrap_or_else(|e| e.into_inner());
            if !tasks.started.contains(&key) {
                if tasks.started.len() >= self.rook.config.remote_agents.max_tracked_tasks {
                    return Err(CoreError::Other("remote task allowance for this turn family is exhausted; retries keep their original request name".into()));
                }
                tasks.started.insert(key);
            }
        }
        let status_id = if let Operation::Status { id } = &operation { Some(id.clone()) } else { None };
        let pending = self.rook.remote_agent_request(&self.vault, self.session, peer, request, operation);
        let response = if let Some(by) = self.by {
            tokio::time::timeout_at(tokio::time::Instant::from_std(by),pending).await
                .unwrap_or_else(|_|Err(CoreError::Other("local turn deadline reached while contacting the remote peer; a write may already be accepted, so retry only the original request name and arguments".into())))
        } else {
            pending.await
        };
        if let Some(id) = status_id {
            let live = response.as_ref().is_ok_and(|value| {
                let response = &value["response"];
                response["state"] == "pending"
                    || serde_json::from_value::<rook_proto::work::Status>(response["work"]["status"].clone())
                        .is_ok_and(|s| s.runnable())
            });
            let mut tasks = self.remote_tasks.lock().unwrap_or_else(|e| e.into_inner());
            tasks.live.retain(|(session, p, r)| *session != self.session || p != peer || r != &id);
            if live {
                while tasks.live.len() >= self.rook.config.remote_agents.max_tracked_tasks {
                    tasks.live.pop_front();
                }
                tasks.live.push_back((self.session, peer.into(), id));
            }
        }
        response
    }
}
