//! What a tool call is doing, in the words a person watching would use.
//!
//! A call was rendered in four places and four ways: the chat REPL printed
//! `read_file`, `rook run` printed `read_file({"path":"src/main.rs",…})`, the
//! TUI printed the name when it came over the socket and the argument when it
//! did not, and the browser printed the name. The name alone answers "it is
//! reading something" and never "which file", which is the question somebody
//! watching a turn is actually asking, and raw JSON answers it in a shape
//! nobody reads at a glance.
//!
//! It lives in core rather than beside the tools because the loop adds six of
//! its own — `delegate`, `load_skill` and `docs` among them — so `rook-tools`
//! is the wrong layer to hold the whole list.

use serde_json::Value;

/// The phrase for one call: the verb a person would use and the argument they
/// are checking. Unknown tools — an MCP server's, a plugin's — keep their name,
/// which is all anything knows about them.
///
/// Not shortened here: how much room there is belongs to whoever is drawing.
pub fn doing(name: &str, arguments: Option<&Value>) -> String {
    let field =
        |key: &str| arguments.and_then(|a| a.get(key)).and_then(|v| v.as_str()).map(|v| v.trim().to_string());
    // `edit_file` takes its work as a list so a refactor across several files
    // is one call; the first path is what the call is about at a glance.
    let first_path = || {
        field("path").or_else(|| {
            arguments
                .and_then(|a| a.get("files"))
                .and_then(|f| f.as_array())
                .and_then(|files| files.first())
                .and_then(|first| first.get("path"))
                .and_then(|p| p.as_str())
                .map(str::to_string)
        })
    };
    match name {
        "read_file" => first_path().map(|p| format!("read {p}")),
        "write_file" => first_path().map(|p| format!("write {p}")),
        "edit_file" => first_path().map(|p| format!("edit {p}")),
        "delete_file" => first_path().map(|p| format!("delete {p}")),
        "move_file" => field("from").map(|from| format!("move {from}")),
        "list_dir" => first_path().map(|p| format!("list {p}")),
        "run_command" => field("command").map(|c| format!("run {c}")),
        "search" => field("pattern").map(|p| format!("search {p}")),
        "web_fetch" => field("url").map(|u| format!("fetch {u}")),
        "web_search" => field("query").map(|q| format!("search the web for {q}")),
        "docs" => field("topic").map(|t| format!("docs {t}")),
        "load_skill" | "find_skill" => field("name").map(|n| format!("{name} {n}")),
        "verify" => field("claim").map(|c| format!("verify {c}")),
        "delegate" => Some("delegate".into()),
        _ => None,
    }
    .unwrap_or_else(|| name.to_string())
}

/// The same phrase in the room there is for it. One line however long the
/// argument: a command that fills the pane pushes the answer off it.
pub fn within(said: &str, room: usize) -> String {
    match said.chars().count() > room {
        true => format!("{}…", said.chars().take(room.saturating_sub(1)).collect::<String>()),
        false => said.to_string(),
    }
}

/// The calls a turn has announced and not yet finished, oldest first.
///
/// A message announces several calls before any of them runs, and a result
/// carries only the tool's name — so two reads in one message are told apart by
/// the order they were announced in and by nothing else. Every front end has to
/// answer the same question, "which announced call is this result", and the two
/// terminal ones were not: they marked whichever line the cursor happened to be
/// on, so a turn that listed a directory and read a file put the first tick at
/// the end of the second line and the second tick on a line of its own.
#[derive(Debug, Default)]
pub struct Running(Vec<(String, String)>);

impl Running {
    pub fn started(&mut self, name: &str, doing: &str) {
        self.0.push((name.to_string(), doing.to_string()));
    }

    /// What the finishing call was doing. A finish with no start — a window
    /// that attached to a daemon mid-turn — is the tool's name, which is all
    /// the result carries.
    pub fn finished(&mut self, name: &str) -> String {
        match self.0.iter().position(|(started, _)| started == name) {
            Some(at) => self.0.remove(at).1,
            None => name.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_call_is_named_by_what_it_is_working_on() {
        let cases = [
            ("read_file", json!({"path": "src/main.rs"}), "read src/main.rs"),
            ("run_command", json!({"command": "cargo test"}), "run cargo test"),
            ("search", json!({"pattern": "TODO"}), "search TODO"),
            ("edit_file", json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]}), "edit a.rs"),
            ("web_fetch", json!({"url": "https://redis.io"}), "fetch https://redis.io"),
            ("move_file", json!({"from": "a.rs", "to": "b.rs"}), "move a.rs"),
        ];
        for (name, arguments, want) in cases {
            assert_eq!(doing(name, Some(&arguments)), want, "{name} says what it is doing");
        }
    }

    #[test]
    fn a_tool_nothing_here_knows_keeps_its_name() {
        // An MCP server's tool, and a call whose argument is not there at all:
        // both are the name, which is what a caller can still read.
        assert_eq!(doing("github__create_issue", Some(&json!({"title": "x"}))), "github__create_issue");
        assert_eq!(doing("read_file", None), "read_file");
        assert_eq!(doing("read_file", Some(&json!({"paths": ["a"]}))), "read_file");
    }

    #[test]
    fn two_calls_to_one_tool_are_told_apart_by_the_order_they_were_announced() {
        let mut running = Running::default();
        running.started("read_file", "read a.rs");
        running.started("read_file", "read b.rs");
        running.started("run_command", "run cargo test");

        // The results carry the tool's name and nothing else.
        assert_eq!(running.finished("read_file"), "read a.rs");
        assert_eq!(running.finished("run_command"), "run cargo test", "and not by turn either");
        assert_eq!(running.finished("read_file"), "read b.rs");

        // A window that attached mid-turn saw no start for this one.
        assert_eq!(running.finished("search"), "search", "a finish alone still says something");
    }

    #[test]
    fn a_long_command_is_cut_to_the_room_there_is() {
        let long = "run ".to_string() + &"cargo test --workspace ".repeat(10);
        let cut = within(&long, 40);
        assert_eq!(cut.chars().count(), 40, "it fills the room and no more: {cut:?}");
        assert!(cut.ends_with('…'), "and says it was cut: {cut:?}");
        assert_eq!(within("run cargo test", 40), "run cargo test", "one that fits is untouched");
    }
}
