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
//!
//! The same reading serves an endpoint that does carry tools, because a small
//! model handed them natively still answers with the object some of the time:
//! `qwen2.5-coder:3b` wrote `{"name": "read_file", "arguments": {...}}` as its
//! whole reply in every smoke scenario, and the turn ended with nothing called.
//! There the object is adopted only when it names a tool that was offered — a
//! reply that is JSON because JSON was asked for is not a call.

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

/// The shapes a model reaches for: ours, OpenAI's `name`/`arguments`, and
/// Anthropic's `name`/`input`.
#[derive(serde::Deserialize)]
struct Called {
    #[serde(alias = "name")]
    tool: String,
    #[serde(default, alias = "input", alias = "parameters")]
    arguments: serde_json::Value,
}

/// Every call in `text`, in order, and what the model said around them.
///
/// Every `{` is tried in turn and the parser decides: a brace in prose fails to
/// deserialize and costs nothing, and a fenced block needs no special case
/// because the fence is simply text either side of the object. All of them,
/// not the first: a small model writes three calls in one reply, and given
/// one of them back it writes the other two again, every step. `known` says
/// which names are tools that were offered; an object naming anything else is
/// left in the text as the answer it is.
pub fn calls_in(text: &str, known: impl Fn(&str) -> bool) -> (Vec<ToolCall>, String) {
    let mut calls = Vec::new();
    let mut said = String::new();
    let mut kept_until = 0;
    for (at, _) in text.match_indices('{') {
        if at < kept_until {
            continue;
        }
        let mut objects = serde_json::Deserializer::from_str(&text[at..]).into_iter::<Called>();
        let Some(Ok(called)) = objects.next() else { continue };
        if !known(&called.tool) {
            continue;
        }
        said.push_str(&text[kept_until..at]);
        kept_until = at + objects.byte_offset();
        calls.push(ToolCall { id: format!("prompted-{at}"), name: called.tool, arguments: called.arguments });
    }
    said.push_str(&text[kept_until..]);
    // The fences the objects sat in are left behind on their own lines.
    let said: Vec<&str> = said.lines().filter(|line| !line.trim_start().starts_with("```")).collect();
    (calls, said.join("\n").trim().to_string())
}

/// Move the calls the model wrote into the fields native ones would have used,
/// so nothing above here has to know which kind of endpoint answered.
pub fn adopt(response: &mut Response, known: impl Fn(&str) -> bool) {
    if !response.message.tool_calls.is_empty() {
        return;
    }
    let (calls, said) = calls_in(&response.message.content, known);
    if calls.is_empty() {
        return;
    }
    response.message.content = said;
    response.message.tool_calls = calls;
    response.stop_reason = StopReason::ToolUse;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn called(text: &str) -> (ToolCall, String) {
        let (mut calls, said) = calls_in(text, |_| true);
        assert_eq!(calls.len(), 1, "one call expected in {text:?}: {calls:?}");
        (calls.remove(0), said)
    }

    /// Three calls in one reply are three calls, in the order written, with
    /// the prose between them kept and the fences gone.
    #[test]
    fn every_call_in_a_reply_is_read_in_order() {
        let text = "First:\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a\"}}\n\
                    then\n```json\n{\"name\": \"list_dir\", \"arguments\": {}}\n```\n\
                    and {\"name\": \"widget\", \"arguments\": {}} is not a tool.";
        let (calls, said) = calls_in(text, |name| name != "widget");
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["read_file", "list_dir"]);
        assert_eq!(calls[0].arguments["path"], "a");
        assert!(said.contains("First:") && said.contains("then") && said.contains("is not a tool"), "{said}");
        assert!(said.contains(r#"{"name": "widget""#), "an object naming no tool stays: {said}");
        assert!(!said.contains("```"), "{said}");
        assert_ne!(calls[0].id, calls[1].id);
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

        // The shapes the providers taught it.
        let openai = called(r#"{"name": "read_file", "arguments": {"path": "config.rs"}}"#);
        assert_eq!((openai.0.name.as_str(), &openai.0.arguments["path"]), ("read_file", &"config.rs".into()));
        let anthropic = called(r#"{"name": "read_file", "input": {"path": "config.rs"}}"#);
        assert_eq!(anthropic.0.arguments["path"], "config.rs");
    }

    /// A reply that is JSON because JSON was asked for is not a call, and the
    /// way to tell is whether the name is a tool that was offered.
    #[test]
    fn an_object_naming_no_offered_tool_is_left_as_text() {
        let text = r#"{"name": "widget", "arguments": {"size": 3}}"#;
        let mut response = Response {
            message: crate::Message::assistant(text),
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: "test".into(),
        };
        adopt(&mut response, |name| name == "read_file");
        assert!(response.message.tool_calls.is_empty(), "{:?}", response.message.tool_calls);
        assert_eq!(response.message.content, text);

        adopt(&mut response, |name| name == "widget");
        assert_eq!(response.message.tool_calls.len(), 1);
        assert!(matches!(response.stop_reason, StopReason::ToolUse));
    }

    /// A scanner for `"tool":` finds one in the tool list, in a quoted example,
    /// and in the model explaining itself. A parser finds an object or fails.
    #[test]
    fn prose_that_talks_about_a_call_is_not_one() {
        assert!(calls_in("I would call {tool} but the path is unclear", |_| true).0.is_empty());
        assert!(calls_in(r#"The "tool" field takes a name, e.g. { "tool": 3 }"#, |_| true).0.is_empty());
        assert!(calls_in("no braces here at all", |_| true).0.is_empty());
    }

    #[test]
    fn a_reply_that_calls_nothing_is_left_alone() {
        let mut answered = Response {
            message: crate::Message::assistant("the file has 40 lines"),
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: "m".into(),
        };
        adopt(&mut answered, |_| true);
        assert_eq!(answered.stop_reason, StopReason::EndTurn);
        assert_eq!(answered.message.content, "the file has 40 lines");
    }
}
