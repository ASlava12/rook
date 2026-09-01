//! Tool calls for an endpoint that has none.
//!
//! Some OpenAI-compatible servers — llama.cpp's, older LM Studio builds, a few
//! gateways — refuse a request that carries `tools` at all. A model behind one
//! can still use them if the tools are described in the prompt and its answer
//! is read back, which is what every agent did before the providers grew a
//! field for it.
//!
//! The encoding is the smallest thing a small model gets right: one JSON
//! object. Reading it back is a parse rather than a scan for markers and
//! quotes — a scanner finds "command": in prose and in the tool list itself.

use crate::{Response, StopReason, ToolCall, ToolSpec};

/// What the model is told it may call.
///
/// Goes in the system block, so it must be stable for a given tool list: the
/// front of a request that varies per turn invalidates the prompt cache behind
/// it.
pub fn describe(tools: &[ToolSpec]) -> String {
    let mut s = String::from(
        "\nThis endpoint cannot carry tool definitions, so a tool is called by replying with one \
         JSON object and nothing else:\n\
         {\"tool\": \"<name>\", \"arguments\": {...}}\n\
         One call per reply, and nothing after it: the result comes back in the next message. \
         Reply normally when you are not calling one.\n\nThe tools:\n",
    );
    for tool in tools {
        s.push_str(&format!("\n{}: {}\n{}\n", tool.name, tool.description, tool.parameters));
    }
    s
}

#[derive(serde::Deserialize)]
struct Called {
    tool: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

/// The first call in `text`, and what the model said around it.
///
/// Every `{` is tried in turn and the parser decides: a brace in prose fails to
/// deserialize and costs nothing, and a fenced block needs no special case
/// because the fence is simply text either side of the object.
pub fn call_in(text: &str) -> Option<(ToolCall, String)> {
    for (at, _) in text.match_indices('{') {
        let mut objects = serde_json::Deserializer::from_str(&text[at..]).into_iter::<Called>();
        let Some(Ok(called)) = objects.next() else { continue };
        let said = format!("{}{}", &text[..at], &text[at + objects.byte_offset()..]);
        // The fence the object sat in is left behind on its own lines.
        let said: Vec<&str> = said.lines().filter(|line| !line.trim_start().starts_with("```")).collect();
        let call = ToolCall { id: format!("prompted-{at}"), name: called.tool, arguments: called.arguments };
        return Some((call, said.join("\n").trim().to_string()));
    }
    None
}

/// Move a call the model wrote into the fields a native one would have used, so
/// nothing above here has to know which kind of endpoint answered.
pub fn adopt(response: &mut Response) {
    if !response.message.tool_calls.is_empty() {
        return;
    }
    let Some((call, said)) = call_in(&response.message.content) else { return };
    response.message.content = said;
    response.message.tool_calls = vec![call];
    response.stop_reason = StopReason::ToolUse;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn called(text: &str) -> (ToolCall, String) {
        call_in(text).unwrap_or_else(|| panic!("no call found in {text:?}"))
    }

    #[test]
    fn a_call_is_read_however_the_model_dressed_it() {
        let bare = called(r#"{"tool": "read_file", "arguments": {"path": "src/lib.rs"}}"#);
        assert_eq!(bare.0.name, "read_file");
        assert_eq!(bare.0.arguments["path"], "src/lib.rs");
        assert_eq!(bare.1, "", "the object itself is not also left in the reply");

        let fenced = called("Let me look.\n```json\n{\"tool\": \"list_dir\", \"arguments\": {}}\n```");
        assert_eq!(fenced.0.name, "list_dir");
        assert_eq!(fenced.1, "Let me look.", "and what it said around the call is kept");
    }

    /// A scanner for `"tool":` finds one in the tool list, in a quoted example,
    /// and in the model explaining itself. A parser finds an object or fails.
    #[test]
    fn prose_that_talks_about_a_call_is_not_one() {
        assert!(call_in("I would call {tool} but the path is unclear").is_none());
        assert!(call_in(r#"The "tool" field takes a name, e.g. { "tool": 3 }"#).is_none());
        assert!(call_in("no braces here at all").is_none());
    }

    #[test]
    fn a_reply_that_calls_nothing_is_left_alone() {
        let mut answered = Response {
            message: crate::Message::assistant("the file has 40 lines"),
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: "m".into(),
        };
        adopt(&mut answered);
        assert_eq!(answered.stop_reason, StopReason::EndTurn);
        assert_eq!(answered.message.content, "the file has 40 lines");
    }
}
