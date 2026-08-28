//! Rook offered as an MCP server, over stdio.
//!
//! The other direction from [`rook_mcp`], which calls somebody else's tools.
//! Here Rook's own are offered to whatever speaks the protocol — an editor, a
//! local model host, another agent — so the file tools, the search and the
//! command runner are reachable without a Rook conversation around them.
//!
//! Two things make it more than a wrapper. The approval policy is in front of
//! every call, so a client cannot reach past it; and with nobody to ask, the
//! unattended approver refuses and says what would make it possible, rather than
//! deciding on the user's behalf.
//!
//! stdout is the protocol. Nothing else may print there, which is why every
//! diagnostic in here goes to stderr.

use std::sync::Arc;

use rook_mcp::protocol::{self, RpcError, ToolDescriptor};
use rook_tools::policy::{Approval, Approver, Decision, Policy, Risk};
use rook_tools::{ToolBox, ToolContext};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::error::Result;

/// Same cap as the client side: a line is a message, and how long one gets is
/// decided by the party on the other end of the pipe.
const MAX_LINE_BYTES: u64 = 8 << 20;

pub struct Offered {
    pub tools: ToolBox,
    pub ctx: ToolContext,
    pub policy: Arc<Policy>,
    pub approver: Arc<dyn Approver>,
}

/// Serve until stdin closes.
pub async fn serve(offered: Offered) -> Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut out = tokio::io::stdout();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = (&mut input).take(MAX_LINE_BYTES).read_until(b'\n', &mut line).await;
        match read {
            Ok(0) => return Ok(()),
            Ok(_) if line.last() != Some(&b'\n') => {
                eprintln!("rook mcp: a line passed {MAX_LINE_BYTES} bytes; closing");
                return Ok(());
            }
            Err(e) => {
                eprintln!("rook mcp: {e}");
                return Ok(());
            }
            Ok(_) => {}
        }

        let Ok(call) = serde_json::from_slice::<protocol::Incoming>(&line) else {
            eprintln!("rook mcp: unparsable line");
            continue;
        };
        // No id is a notification: `notifications/initialized` is the one that
        // matters and it wants no answer. Answering one is a protocol error.
        let Some(id) = call.id else { continue };
        let Some(method) = call.method.as_deref() else { continue };

        let answer = match answer_to(&offered, method, call.params.unwrap_or(json!({}))).await {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let mut text = serde_json::to_vec(&answer)?;
        text.push(b'\n');
        if out.write_all(&text).await.is_err() || out.flush().await.is_err() {
            return Ok(());
        }
    }
}

async fn answer_to(
    offered: &Offered,
    method: &str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": protocol::PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "rook", "version": crate::AGENT_VERSION },
        })),
        "tools/list" => Ok(json!({ "tools": descriptors(&offered.tools) })),
        "tools/call" => call_tool(offered, params).await,
        "ping" => Ok(json!({})),
        other => Err(RpcError { code: -32601, message: format!("{other} is not implemented"), data: None }),
    }
}

fn descriptors(tools: &ToolBox) -> Vec<ToolDescriptor> {
    tools
        .specs()
        .into_iter()
        .map(|spec| ToolDescriptor {
            name: spec.name,
            description: spec.description,
            input_schema: spec.parameters,
            annotations: Default::default(),
        })
        .collect()
}

async fn call_tool(
    offered: &Offered,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, RpcError> {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let Some(tool) = offered.tools.get(&name) else {
        return Err(RpcError { code: -32602, message: format!("no tool called {name:?}"), data: None });
    };

    // The same gate a turn goes through. A client reaching in from outside is
    // not more trusted than the model inside, and the policy is the only thing
    // that ever decided this.
    let risk = tool.risk(&args);
    if let Some(refusal) = refused(offered, &name, risk).await {
        return Ok(as_result(&refusal, true));
    }

    match offered.tools.call(&offered.ctx, &name, &args).await {
        Ok(outcome) => Ok(as_result(&outcome.content, outcome.is_error)),
        Err(e) => Ok(as_result(&e.to_string(), true)),
    }
}

/// Why the call may not happen, or `None` when it may.
async fn refused(offered: &Offered, name: &str, risk: Risk) -> Option<String> {
    match offered.policy.decide(&risk) {
        Decision::Allow => None,
        Decision::Deny(why) => Some(format!("refused: {why}")),
        Decision::Ask => match offered.approver.ask(name, &risk).await {
            Approval::Once => None,
            Approval::ForRun => {
                offered.policy.grant_for_run(&risk.subject());
                None
            }
            Approval::Deny(why) => Some(format!("refused: {why}")),
        },
    }
}

/// A tool result in the protocol's shape. Errors travel as a result with
/// `isError`, not as an RPC error: the call happened and the client wants to
/// show what it said, which a transport-level failure would hide.
fn as_result(text: &str, is_error: bool) -> serde_json::Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}
