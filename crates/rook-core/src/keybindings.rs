//! Named terminal actions shared by configuration, help and input dispatch.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Prompt,
}

macro_rules! actions {
    ($( $action:ident, $id:literal, $scope:ident, $help:literal, [$($key:literal),*]; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Action { $($action,)* }
        pub const ACTIONS: &[Spec] = &[$(Spec {
            action: Action::$action, id: $id, scope: Scope::$scope, help: $help, defaults: &[$($key),*],
        },)*];
    }
}

pub struct Spec {
    pub action: Action,
    pub id: &'static str,
    pub scope: Scope,
    pub help: &'static str,
    pub defaults: &'static [&'static str],
}

actions! {
    Stop, "stop", Global, "Stop the active turn; quit when idle", ["ctrl+c"];
    Palette, "palette", Global, "Open the command and action palette", ["ctrl+p"];
    Mouse, "mouse", Global, "Toggle terminal selection and mouse scrolling", ["ctrl+s"];
    Stance, "stance", Global, "Cycle the permission stance", ["f2"];
    Effort, "effort", Global, "Cycle reasoning effort", ["f3"];
    Tasks, "tasks", Global, "Open scheduled tasks", ["f4"];
    Home, "prompt.home", Prompt, "Move to the start of the prompt", ["home", "ctrl+a"];
    End, "prompt.end", Prompt, "Move to the end; show newest output when empty", ["end"];
    Editor, "prompt.editor", Prompt, "Edit the draft in an external console editor", ["ctrl+e"];
    History, "prompt.history", Prompt, "Search the conversation history", ["ctrl+f"];
    KillWord, "prompt.delete_word", Prompt, "Delete the word before the cursor", ["ctrl+w"];
    KillStart, "prompt.delete_start", Prompt, "Delete from the start to the cursor", ["ctrl+u"];
    KillEnd, "prompt.delete_end", Prompt, "Delete from the cursor to the end", ["ctrl+k"];
    Calls, "prompt.calls", Prompt, "Inspect tool arguments and results", ["ctrl+o"];
    ToolPrevious, "prompt.tool_previous", Prompt, "Select the previous saved tool result in chat", ["f5"];
    ToolNext, "prompt.tool_next", Prompt, "Select the next saved tool result in chat", ["f6"];
    ToolResult, "prompt.tool_result", Prompt, "Open the selected saved tool result (latest by default)", ["f7"];
    Complete, "prompt.complete", Prompt, "Complete a command or file mention", ["tab"];
    Escape, "prompt.escape", Prompt, "Clear the prompt; quit when it is empty", ["escape"];
    Left, "prompt.left", Prompt, "Move left", ["left"];
    Right, "prompt.right", Prompt, "Move right", ["right"];
    Delete, "prompt.delete", Prompt, "Delete the character after the cursor", ["delete"];
    Up, "prompt.up", Prompt, "Move up; recall the previous prompt above the first row", ["up"];
    Down, "prompt.down", Prompt, "Move down; recall the next prompt below the last row", ["down"];
    Newline, "prompt.newline", Prompt, "Insert a newline without submitting", ["ctrl+j", "shift+enter", "alt+enter"];
    Submit, "prompt.submit", Prompt, "Submit the prompt or the current answer", ["enter"];
    FollowUp, "prompt.followup", Prompt, "Queue the draft after the current turn or whole goal completes", [];
    Backspace, "prompt.backspace", Prompt, "Delete the character before the cursor", ["backspace", "ctrl+h"];
    PageUp, "prompt.page_up", Prompt, "Scroll conversation upward", ["pageup"];
    PageDown, "prompt.page_down", Prompt, "Scroll conversation downward", ["pagedown"];
    Undo, "prompt.undo", Prompt, "Undo a draft edit (does not rewind files)", ["ctrl+z"];
    Redo, "prompt.redo", Prompt, "Redo an undone draft edit", ["alt+z"];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub undo_events: usize,
    pub undo_bytes: usize,
    /// Missing actions keep their defaults; an empty array disables an action.
    pub keys: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            undo_events: 256,
            undo_bytes: 1024 * 1024,
            keys: ACTIONS
                .iter()
                .map(|s| (s.id.into(), s.defaults.iter().map(|k| (*k).into()).collect()))
                .collect(),
        }
    }
}

pub struct Bindings(BTreeMap<String, (Action, Scope)>);

impl Settings {
    pub fn bindings(&self) -> Result<Bindings, Vec<String>> {
        let mut errors = Vec::new();
        if !(1..=4096).contains(&self.undo_events) {
            errors.push("tui.undo_events: expected 1..=4096".into());
        }
        if !(4096..=16 * 1024 * 1024).contains(&self.undo_bytes) {
            errors.push("tui.undo_bytes: expected 4096..=16777216".into());
        }
        for id in self.keys.keys() {
            if !ACTIONS.iter().any(|spec| spec.id == id) {
                errors.push(format!("tui.keys: unknown action {id:?}"));
            }
        }
        let mut keys = BTreeMap::new();
        for spec in ACTIONS {
            let defaults: Vec<String> = spec.defaults.iter().map(|key| (*key).into()).collect();
            let chosen = self.keys.get(spec.id).unwrap_or(&defaults);
            if chosen.len() > 8 {
                errors.push(format!("tui.keys.{}: at most eight bindings per action", spec.id));
                continue;
            }
            for value in chosen {
                let Some(key) = normalize(value) else {
                    errors.push(format!("tui.keys.{}: invalid key {value:?}", spec.id));
                    continue;
                };
                if let Some((other, _)) = keys.insert(key.clone(), (spec.action, spec.scope)) {
                    let id = ACTIONS.iter().find(|s| s.action == other).map_or("unknown", |s| s.id);
                    errors.push(format!("tui.keys: {key} conflicts between {id} and {}", spec.id));
                }
            }
        }
        if errors.is_empty() { Ok(Bindings(keys)) } else { Err(errors) }
    }
}

impl Bindings {
    pub fn action(&self, key: &str, prompt: bool) -> Option<Action> {
        self.0.get(key).and_then(|(action, scope)| (*scope == Scope::Global || prompt).then_some(*action))
    }

    pub fn label(&self, action: Action) -> String {
        let names: Vec<&str> = self
            .0
            .iter()
            .filter_map(|(key, (found, _))| (*found == action).then_some(key.as_str()))
            .collect();
        if names.is_empty() { "unbound".into() } else { names.join(" / ") }
    }
}

/// Terminal-independent spelling; aliases normalize before collision checks.
fn normalize(value: &str) -> Option<String> {
    if value.len() > 64 {
        return None;
    }
    let value = value.trim().to_ascii_lowercase();
    let mut parts: Vec<&str> = value.split('+').collect();
    let key = match parts.pop()? {
        "esc" => "escape",
        "return" => "enter",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        "space" => "space",
        "plus" => "plus",
        key => key,
    };
    let valid = matches!(
        key,
        "escape"
            | "enter"
            | "tab"
            | "backspace"
            | "delete"
            | "insert"
            | "home"
            | "end"
            | "up"
            | "down"
            | "left"
            | "right"
            | "pageup"
            | "pagedown"
            | "space"
            | "plus"
    ) || (key.len() == 1 && key.bytes().all(|c| c.is_ascii_graphic()))
        || key.strip_prefix('f').is_some_and(|digits| {
            digits.parse::<u8>().ok().is_some_and(|n| (1..=12).contains(&n) && digits == n.to_string())
        });
    if !valid {
        return None;
    }
    let mut modifiers = BTreeSet::new();
    for part in parts {
        let modifier = match part {
            "control" => "ctrl",
            "ctrl" | "alt" | "shift" | "super" => part,
            _ => return None,
        };
        if !modifiers.insert(modifier) {
            return None;
        }
    }
    let mut canonical = String::new();
    for modifier in ["ctrl", "alt", "shift", "super"] {
        if modifiers.contains(modifier) {
            canonical.push_str(modifier);
            canonical.push('+');
        }
    }
    canonical.push_str(key);
    Some(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_defaults_and_unbound_actions_stay_unbound() {
        let settings: Settings = toml::from_str("[keys]\n'prompt.editor'=['alt+e']\npalette=[]").unwrap();
        let bindings = settings.bindings().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(bindings.action("alt+e", true), Some(Action::Editor));
        assert_eq!(bindings.action("ctrl+e", true), None);
        assert_eq!(bindings.action("ctrl+p", true), None);
        assert_eq!(bindings.action("ctrl+c", false), Some(Action::Stop));
        assert_eq!(bindings.action("alt+e", false), None);
    }

    #[test]
    fn collisions_include_defaults_and_normalized_aliases() {
        let settings: Settings = toml::from_str("[keys]\n'prompt.undo'=['CONTROL+P']").unwrap();
        assert!(settings.bindings().err().unwrap().iter().any(|e| e.contains("conflicts")));
        assert_eq!(normalize("alt+control+x"), Some("ctrl+alt+x".into()));
        for bad in ["ctrl+", "ctrl+ctrl+x", "f13", "hyper+x", "ctrl+two"] {
            assert!(normalize(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn followup_is_a_bindable_prompt_action_without_taking_newline_keys() {
        let settings = Settings::default();
        let defaults = settings.bindings().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(defaults.label(Action::FollowUp), "unbound");
        for key in ["ctrl+j", "shift+enter", "alt+enter"] {
            assert_eq!(defaults.action(key, true), Some(Action::Newline));
        }
        let remapped: Settings = toml::from_str("[keys]\n'prompt.followup'=['f9']").unwrap();
        let bindings = remapped.bindings().unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(bindings.action("f9", true), Some(Action::FollowUp));
        assert_eq!(bindings.action("f9", false), None);
        assert_eq!(bindings.action("enter", true), Some(Action::Submit));
        assert_eq!(bindings.action("alt+enter", true), Some(Action::Newline));
    }
}
