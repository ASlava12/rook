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

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    // The tag dialects first, and only then the object: a model writes one or
    // the other, and a `<parameter=…>` body may itself contain JSON that is an
    // argument rather than a call.
    let tagged = tagged_calls_in(text, &known);
    if !tagged.0.is_empty() {
        return tagged;
    }
    let mut calls = Vec::new();
    let mut said = String::new();
    let mut kept_until = 0;
    // Past the end of the last object that parsed, taken or not: an answer
    // that is JSON may carry a tool-shaped object inside it, and that is part
    // of the answer.
    let mut seen_until = 0;
    for (at, _) in text.match_indices('{') {
        if at < seen_until {
            continue;
        }
        let mut objects = serde_json::Deserializer::from_str(&text[at..]).into_iter::<Called>();
        let Some(Ok(called)) = objects.next() else { continue };
        seen_until = at + objects.byte_offset();
        if !known(&called.tool) {
            continue;
        }
        said.push_str(&text[kept_until..at]);
        kept_until = seen_until;
        // Unique for the process, not the reply: an id made from the offset
        // recurred across replies that each began with the object, and a
        // history replayed with the same id twice is one a dialect may refuse.
        let n = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        calls.push(ToolCall { id: format!("prompted-{n}"), name: called.tool, arguments: called.arguments });
    }
    said.push_str(&text[kept_until..]);
    (calls, cleaned(&said))
}

/// What a call was wrapped in, left behind on its own line once the call has
/// been taken out: fences, and the `<tool_call>` markers of the tag dialects.
fn cleaned(said: &str) -> String {
    let kept: Vec<&str> = said
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.starts_with("```") && !matches!(line, "<tool_call>" | "</tool_call>")
        })
        .collect();
    kept.join("\n").trim().to_string()
}

/// A call written in tags rather than as an object.
///
/// Qwen and Hermes chat templates emit `<tool_call><function=name>
/// <parameter=key>…</parameter></function></tool_call>`, and a model trained
/// on Claude's format writes `<invoke name="…"><parameter name="…">`. Both are
/// plainly a call and neither is JSON, so a reply carrying one ended the turn
/// with the work half done and nothing to run — which is what
/// `apodex-1.1-mini` did on the fourteenth step of a real task, in its
/// thinking, where nothing looked at all.
pub fn tagged_calls_in(text: &str, known: impl Fn(&str) -> bool) -> (Vec<ToolCall>, String) {
    let mut calls = Vec::new();
    let mut said = String::new();
    let mut rest = text;
    while let Some(found) = next_tagged(rest) {
        said.push_str(found.before);
        if known(&found.name) {
            let n = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            calls.push(ToolCall {
                id: format!("prompted-{n}"),
                name: found.name,
                arguments: parameters_in(found.body),
            });
        } else {
            // Not a tool that was offered: it is text, and stays text.
            said.push_str(found.whole);
        }
        rest = found.after;
    }
    said.push_str(rest);
    (calls, cleaned(&said))
}

struct Tagged<'a> {
    before: &'a str,
    /// The call as written, for putting back when it names nothing offered.
    whole: &'a str,
    name: String,
    body: &'a str,
    after: &'a str,
}

/// The two openers, each with what ends its name and what closes the call.
const DIALECTS: [(&str, &str, &str); 2] =
    [("<function=", ">", "</function>"), ("<invoke name=\"", "\">", "</invoke>")];

fn next_tagged(text: &str) -> Option<Tagged<'_>> {
    let (at, open, ends_name, close) = DIALECTS
        .iter()
        .filter_map(|(open, ends_name, close)| text.find(open).map(|at| (at, *open, *ends_name, *close)))
        .min_by_key(|(at, ..)| *at)?;
    let after_open = &text[at + open.len()..];
    let name_ends = after_open.find(ends_name)?;
    let name = after_open[..name_ends].trim().to_string();
    let body_from = &after_open[name_ends + ends_name.len()..];
    // A call that was never closed is not one to run: the loop asks a reply
    // cut off mid-call to go on, and half a command is worse than none.
    let body_ends = body_from.find(close)?;
    let past = at + open.len() + name_ends + ends_name.len() + body_ends + close.len();
    Some(Tagged {
        before: &text[..at],
        whole: &text[at..past],
        name,
        body: &body_from[..body_ends],
        after: &text[past..],
    })
}

/// The arguments of a tagged call, which are tagged too.
///
/// A value is read as JSON when it plainly is one — a number, a list, an
/// object — and as a string otherwise, because that is what a shell command
/// with quotes and pipes in it is.
fn parameters_in(body: &str) -> serde_json::Value {
    let mut arguments = serde_json::Map::new();
    for (open, ends_name) in [("<parameter=", ">"), ("<parameter name=\"", "\">")] {
        let mut rest = body;
        while let Some(at) = rest.find(open) {
            let after = &rest[at + open.len()..];
            let Some(name_ends) = after.find(ends_name) else { break };
            let key = after[..name_ends].trim().to_string();
            let value_from = &after[name_ends + ends_name.len()..];
            let Some(value_ends) = value_from.find("</parameter>") else { break };
            let value = value_from[..value_ends].trim();
            arguments.insert(key, as_json(value));
            rest = &value_from[value_ends..];
        }
    }
    serde_json::Value::Object(arguments)
}

fn as_json(value: &str) -> serde_json::Value {
    let looks_like_json = value.starts_with(['[', '{', '-'])
        || value.starts_with(|c: char| c.is_ascii_digit())
        || matches!(value, "true" | "false" | "null");
    match looks_like_json.then(|| serde_json::from_str(value).ok()).flatten() {
        Some(parsed) => parsed,
        None => serde_json::Value::String(value.to_string()),
    }
}

/// Whether the reply ends inside a tool call it did not finish writing.
///
/// A call written as text needs the whole object to be read as one, and a reply
/// cut mid-object is neither an answer nor a call: the loop asks such a reply
/// to go on, but only knew to when the provider said it had hit the output
/// limit. Ollama reports `stop` for a reply it truncated, so the tell has to be
/// the text — an object that names an offered tool and never closes.
pub fn cut_off_call(text: &str, known: impl Fn(&str) -> bool) -> bool {
    // The same walk `calls_in` does, for the same reason: an object that parses
    // is a call somebody else handles, and its innards must not be examined as
    // if they were a half-written one.
    let mut seen_until = 0;
    for (at, _) in text.match_indices('{') {
        if at < seen_until {
            continue;
        }
        let mut objects = serde_json::Deserializer::from_str(&text[at..]).into_iter::<Called>();
        if let Some(Ok(_)) = objects.next() {
            seen_until = at + objects.byte_offset();
            continue;
        }
        if named_in(&text[at..]).is_some_and(|name| known(&name)) {
            return true;
        }
    }
    false
}

/// The tool a half-written object names, by reading the one field that matters
/// rather than the object: there is no object yet, which is the point.
fn named_in(tail: &str) -> Option<String> {
    for key in ["\"tool\"", "\"name\""] {
        let Some(at) = tail.find(key) else { continue };
        let after = tail[at + key.len()..].trim_start();
        let Some(after) = after.strip_prefix(':') else { continue };
        let after = after.trim_start();
        let Some(rest) = after.strip_prefix('"') else { continue };
        // Only a value that was finished: a name cut in half is not one to
        // look up, and the call is cut anyway.
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// The same, for a call the model wrote while thinking rather than answering.
///
/// `apodex-1.1-mini` through LM Studio put a whole `<tool_call>` in
/// `reasoning_content` and left `content` empty, so a turn that was working
/// stopped at step fourteen with the call sitting in the transcript, unread.
/// Only the tag dialects, and only when nothing was said: an explicit
/// invocation tag is a call however it arrived, while an object sketched in
/// the middle of thinking out loud is a model considering one.
pub fn adopt_thought(response: &mut Response, thinking: &str, known: impl Fn(&str) -> bool) {
    if !response.message.tool_calls.is_empty() {
        return;
    }
    let (calls, _) = tagged_calls_in(thinking, known);
    if calls.is_empty() {
        return;
    }
    response.message.tool_calls = calls;
    response.stop_reason = StopReason::ToolUse;
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
        let (again, _) = calls_in(text, |name| name != "widget");
        assert!(
            !again.iter().any(|c| calls.iter().any(|earlier| earlier.id == c.id)),
            "ids do not recur across replies"
        );
    }

    /// An answer that is JSON may carry a tool-shaped object inside it. The
    /// outer object names no tool, so nothing inside it is a call either.
    #[test]
    fn an_object_inside_an_answer_is_not_a_call() {
        let text = r#"{"name": "widget", "arguments": {"name": "read_file", "arguments": {"path": "a"}}}"#;
        let (calls, said) = calls_in(text, |name| name == "read_file");
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(said, text);
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
    /// Verbatim from a real turn: `apodex-1.1-mini` writing its fourteenth
    /// step. The call was complete, in the transcript, and unread — the turn
    /// ended saying nothing, with the task half done.
    const AS_TAGS: &str = "Список файлов есть. Теперь поищу упоминания /tmp в коде.\n\n\
        <tool_call>\n<function=run_command>\n<parameter=command>\n\
        cd /Users/vart/pyportal && grep -rn \"/tmp\" --include=\"*.py\" . | head -50\n\
        </parameter>\n</function>\n</tool_call>";

    #[test]
    fn a_call_written_in_tags_is_read_like_one_written_as_json() {
        let (calls, said) = calls_in(AS_TAGS, |name| name == "run_command");

        assert_eq!(calls.len(), 1, "one call: {calls:?}");
        assert_eq!(calls[0].name, "run_command");
        assert_eq!(
            calls[0].arguments["command"],
            "cd /Users/vart/pyportal && grep -rn \"/tmp\" --include=\"*.py\" . | head -50",
            "the command survives its quotes and pipes whole"
        );
        assert_eq!(said, "Список файлов есть. Теперь поищу упоминания /tmp в коде.");
    }

    /// The same call, arriving where nothing looked: this model put it in
    /// `reasoning_content` and left the answer empty.
    #[test]
    fn a_call_the_model_wrote_while_thinking_is_still_a_call() {
        let mut response = Response {
            message: crate::Message::assistant(""),
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: String::new(),
        };
        adopt_thought(&mut response, AS_TAGS, |name| name == "run_command");

        assert_eq!(response.message.tool_calls.len(), 1, "the turn goes on: {response:?}");
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(response.message.content, "", "and the thinking stays where it was");
    }

    #[test]
    fn tags_naming_a_tool_that_was_not_offered_stay_as_text() {
        let (calls, said) = calls_in(AS_TAGS, |name| name == "something_else");

        assert!(calls.is_empty(), "not a call: {calls:?}");
        assert!(said.contains("<function=run_command>"), "and it is still there to read: {said}");
    }

    /// The other tag dialect, and typed arguments: a number written in a tag
    /// is a number, and a command with a pipe in it is not JSON at all.
    #[test]
    fn the_other_dialect_and_its_argument_types_are_read_too() {
        let text = "<invoke name=\"read_file\"><parameter name=\"path\">src/main.rs</parameter>\
            <parameter name=\"limit\">50</parameter></invoke>";
        let (calls, said) = calls_in(text, |name| name == "read_file");

        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].arguments["path"], "src/main.rs");
        assert_eq!(calls[0].arguments["limit"], 50, "a number stays a number: {:?}", calls[0].arguments);
        assert_eq!(said, "");
    }

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

    /// The tell has to be the text: Ollama reports `stop` for a reply it
    /// truncated, so a call cut in half arrives looking like an answer, and the
    /// model that wrote it goes on to repeat its previous call.
    #[test]
    fn a_call_the_model_did_not_finish_writing_is_recognised_as_cut_off() {
        let known = |name: &str| name == "find_skill";
        let cut = r#"{"name": "find_skill", "arguments": {"install":"config.rs","#;

        assert!(cut_off_call(cut, known), "an object that names a tool and never closes");
        assert!(calls_in(cut, known).0.is_empty(), "and it is not a call, because it is not whole");
    }

    #[test]
    fn a_finished_call_and_ordinary_prose_are_not_cut_off() {
        let known = |name: &str| name == "find_skill";

        assert!(!cut_off_call(r#"{"name": "find_skill", "arguments": {}}"#, known), "whole");
        assert!(!cut_off_call("I will use find_skill next.", known), "prose naming a tool");
        assert!(!cut_off_call(r#"{"name": "not_a_tool", "argu"#, known), "a tool nobody offered");
        assert!(!cut_off_call(r#"here is a map: {"a": 1,"#, known), "an object naming no tool");
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
