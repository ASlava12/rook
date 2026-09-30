//! A draft of config.toml: typed edits with help, preserving unrelated TOML.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

use super::Config;

// Limits apply before parsing and while editing, including an existing file.
const MAX_CONFIG: u64 = 1 << 20;
const MAX_ITEMS: usize = 256;

#[derive(Clone, Deserialize)]
pub struct Help {
    pub kind: String,
    pub help: String,
    #[serde(default)]
    pub choices: Vec<String>,
}

pub struct Entry {
    pub path: Vec<String>,
    pub value: Value,
    pub default: Value,
    pub explicit: bool,
    pub help: Help,
    pub secret: bool,
}

pub struct Editor {
    path: PathBuf,
    original: Option<String>,
    doc: DocumentMut,
    help: BTreeMap<String, Help>,
}

impl Editor {
    /// Open the file without taking the store lock or contacting any provider.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        // Editing a symlinked dotfile updates its target, not the symlink.
        let path = if path.symlink_metadata().is_ok() {
            path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?
        } else {
            path
        };
        let original = read(&path)?;
        let doc = original.as_deref().unwrap_or_default().parse::<DocumentMut>().map_err(|_| {
            format!("{} is not valid TOML; repair its syntax before opening the form", path.display())
        })?;
        let help = serde_json::from_str(include_str!("help.json"))
            .map_err(|e| format!("invalid built-in help: {e}"))?;
        let editor = Self { path, original, doc, help };
        check_sizes(&editor.raw()?)?;
        Ok(editor)
    }

    pub fn file_path(&self) -> &Path {
        &self.path
    }

    pub fn dirty(&self) -> bool {
        self.doc.to_string() != self.original.as_deref().unwrap_or_default()
    }

    pub fn can_add(&self, path: &[String]) -> bool {
        is_named_map(path)
            || self
                .raw()
                .and_then(|raw| effective(&raw))
                .ok()
                .and_then(|value| at(&value, path).map(Value::is_array))
                .unwrap_or(false)
    }

    /// Defaults are visible even when the file has no corresponding lines.
    pub fn entries(&self, path: &[String]) -> Result<Vec<Entry>, String> {
        let raw = self.raw()?;
        let effective = effective(&raw)?;
        let node = at(&effective, path).ok_or("section no longer exists")?;
        let keys: Vec<String> = match node {
            Value::Object(map) => map.keys().cloned().collect(),
            Value::Array(array) => (0..array.len()).map(|i| i.to_string()).collect(),
            _ => return Err("this value is not a section".into()),
        };
        Ok(keys
            .into_iter()
            .map(|key| {
                let mut child = path.to_vec();
                child.push(key);
                let value = at(&effective, &child).cloned().unwrap_or(Value::Null);
                let default = default_at(&child).unwrap_or(Value::Null);
                let help = self.field_help(&child, &value);
                let secret = child.iter().any(|key| matches!(key.as_str(), "key" | "headers" | "env"))
                    || child.first().is_some_and(|s| s == "proxy")
                    || child.last().is_some_and(|s| s == "proxy");
                Entry { explicit: at(&raw, &child).is_some(), path: child, value, default, help, secret }
            })
            .collect())
    }

    /// Text from a field, not shell or TOML syntax: a string stays a string.
    pub fn set(&mut self, path: &[String], text: &str) -> Result<(), String> {
        if text.len() as u64 > MAX_CONFIG {
            return Err("value exceeds 1 MiB".into());
        }
        let raw = self.raw()?;
        let effective = effective(&raw)?;
        let current = at(&effective, path).ok_or("setting no longer exists")?;
        let help = self.field_help(path, current);
        let value = match help.kind.as_str() {
            "boolean" => Value::Bool(text.parse().map_err(|_| "choose true or false")?),
            "integer" => {
                let value: i64 = text.trim().parse().map_err(|_| "enter a whole number")?;
                if value < 0
                    && !matches!(
                        path.join(".").as_str(),
                        "storage.compression_level" | "storage.gc_grace_secs"
                    )
                {
                    return Err("enter a non-negative number".into());
                }
                json!(value)
            }
            "number" => {
                let value: f64 = text.trim().parse().map_err(|_| "enter a number")?;
                if !value.is_finite() {
                    return Err("enter a finite number".into());
                }
                json!(value)
            }
            "string" => Value::String(text.into()),
            _ => return Err("open this section to edit its entries".into()),
        };
        if !help.choices.is_empty() && !help.choices.iter().any(|s| value.as_str() == Some(s)) {
            return Err(format!("choose one of: {}", help.choices.join(", ")));
        }
        if path == ["agent", "compact_at"] {
            crate::context::check_compact_at(value.as_f64().ok_or("enter a number")? as f32)?;
        }
        self.change(path, Some(item(&value)?))
    }

    /// Removing a fixed field restores the default. Collection entries vanish.
    pub fn remove(&mut self, path: &[String]) -> Result<(), String> {
        if path.len() == 1 {
            return Err("open the section to remove a setting or entry".into());
        }
        let raw = self.raw()?;
        let inherited_list_item = effective(&raw)
            .ok()
            .and_then(|v| at(&v, &path[..path.len() - 1]).map(Value::is_array))
            .unwrap_or(false);
        if at(&raw, path).is_none() && !inherited_list_item {
            return Ok(());
        }
        self.change(path, None)
    }

    /// Add a named map entry or append an item to a list. Names are literal,
    /// including dots in model names and HTTP header names.
    pub fn add(&mut self, path: &[String], name: &str) -> Result<Vec<String>, String> {
        let raw = self.raw()?;
        let effective = effective(&raw)?;
        let node = at(&effective, path).ok_or("section no longer exists")?;
        let mut child = path.to_vec();
        let value = match node {
            Value::Array(values) => {
                if values.len() >= MAX_ITEMS {
                    return Err("a collection may hold at most 256 entries".into());
                }
                child.push(values.len().to_string());
                match path.first().map(String::as_str) {
                    Some("mcp") if path.len() == 1 => {
                        valid_name(name)?;
                        if values.iter().any(|v| v.get("name").and_then(Value::as_str) == Some(name)) {
                            return Err("that server name already exists".into());
                        }
                        json!({"name":name})
                    }
                    Some("lsp") if path.len() == 1 => {
                        valid_name(name)?;
                        json!({"language":name})
                    }
                    Some("hooks") if path.len() == 1 => json!({"event":"post_tool", "command": name}),
                    _ => json!(name),
                }
            }
            Value::Object(values) if is_named_map(path) => {
                valid_name(name)?;
                if values.len() >= MAX_ITEMS {
                    return Err("a collection may hold at most 256 entries".into());
                }
                if values.contains_key(name) {
                    return Err("that name already exists".into());
                }
                child.push(name.into());
                if path.len() == 1 { json!({}) } else { json!("") }
            }
            _ => {
                return Err(
                    "choose models, endpoints, a list, or a map such as MCP headers/env to add an entry"
                        .into(),
                );
            }
        };
        // A missing array may have nonempty defaults. Materialize it before
        // appending, so adding a source does not erase the default source.
        let mut doc = self.doc.clone();
        materialize_arrays(&mut doc, &raw, &effective, path)?;
        if node.is_array() {
            if at(&raw, path).is_none() {
                patch(doc.as_item_mut(), path, Some(item(node)?))?;
            }
            append(doc.as_item_mut(), path, item(&value)?)?;
        } else {
            patch(doc.as_item_mut(), &child, Some(item(&value)?))?;
        }
        self.accept(doc)?;
        Ok(child)
    }

    /// Validate and atomically replace only the file this draft was based on.
    pub fn save(&mut self) -> Result<(), String> {
        let text = self.doc.to_string();
        let config: Config = toml::from_str(&text).map_err(|e| format!("not saved: {}", e.message()))?;
        let errors = config.validation_errors();
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        if !self.dirty() {
            return Ok(());
        }
        if read(&self.path)? != self.original {
            return Err(
                "file changed outside this editor; nothing overwritten. Reopen it to use the newer file"
                    .into(),
            );
        }
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        crate::paths::private_dir(parent).map_err(|e| e.to_string())?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temp.write_all(text.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        temp.persist(&self.path).map_err(|e| e.to_string())?;
        self.original = Some(text);
        Ok(())
    }

    fn raw(&self) -> Result<Value, String> {
        let value: toml::Value = toml::from_str(&self.doc.to_string()).map_err(|e| e.to_string())?;
        serde_json::to_value(value).map_err(|e| e.to_string())
    }

    fn field_help(&self, path: &[String], value: &Value) -> Help {
        if path.len() >= 3
            && path[0] == "tui"
            && path[1] == "keys"
            && let Some(spec) = crate::keybindings::ACTIONS.iter().find(|spec| spec.id == path[2])
        {
            return Help {
                kind: kind(value).into(),
                help: format!(
                    "{}. Missing keeps defaults; an empty list disables. Keys: ctrl/alt/shift/super + key. Terminal support varies. Changes apply when reopening TUI.",
                    spec.help
                ),
                choices: Vec::new(),
            };
        }
        let mut normalized = path.to_vec();
        if path == ["sandbox", "mode"] {
            normalized[1] = "stance".into();
        }
        if path.len() > 1 && matches!(path[0].as_str(), "mcp" | "lsp" | "hooks" | "models" | "endpoints") {
            normalized[1] = "*".into();
        }
        if let Some(help) = self.help.get(&normalized.join(".")) {
            let mut help = help.clone();
            match normalized.join(".").as_str() {
                "sandbox.stance" => {
                    help.choices = rook_tools::policy::Stance::ALL.iter().map(|v| v.as_str().into()).collect()
                }
                "agent.effort" => {
                    help.choices = rook_llm::Effort::ALL.iter().map(|v| v.as_str().into()).collect()
                }
                "models.*.api" | "endpoints.*.api" => {
                    help.choices = std::iter::once(String::new())
                        .chain(rook_llm::Api::ALL.iter().map(|v| v.as_str().into()))
                        .collect()
                }
                _ => {}
            }
            return help;
        }
        if path.len() > 1 {
            let parent = &path[..path.len() - 1];
            let mut key = normalized;
            key.pop();
            if is_named_map(parent) || path.last().is_some_and(|s| s.parse::<usize>().is_ok()) {
                let help = self
                    .help
                    .get(&key.join("."))
                    .map(|h| h.help.clone())
                    .unwrap_or_else(|| "Entry value. Enter edits it; d removes it.".into());
                return Help { kind: kind(value).into(), help, choices: Vec::new() };
            }
        }
        Help {
            kind: kind(value).into(),
            help: "Unknown setting: this build may ignore it. Its existing value is preserved; d removes it."
                .into(),
            choices: Vec::new(),
        }
    }

    fn change(&mut self, path: &[String], replacement: Option<Item>) -> Result<(), String> {
        let raw = self.raw()?;
        let effective = effective(&raw)?;
        let mut doc = self.doc.clone();
        materialize_arrays(&mut doc, &raw, &effective, path)?;
        patch(doc.as_item_mut(), path, replacement)?;
        self.accept(doc)
    }

    fn accept(&mut self, doc: DocumentMut) -> Result<(), String> {
        if doc.to_string().len() as u64 > MAX_CONFIG {
            return Err("configuration exceeds 1 MiB".into());
        }
        self.doc = doc;
        Ok(())
    }
}

fn check_sizes(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            if map.len() > MAX_ITEMS {
                return Err("a section exceeds 256 entries".into());
            }
            for value in map.values() {
                check_sizes(value)?;
            }
        }
        Value::Array(array) => {
            if array.len() > MAX_ITEMS {
                return Err("a collection exceeds 256 entries".into());
            }
            for value in array {
                check_sizes(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG {
        return Err("configuration exceeds 1 MiB".into());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| "configuration must be UTF-8".into())
}

fn kind(value: &Value) -> &str {
    match value {
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        _ => "string",
    }
}
fn valid_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        Err("enter a nonempty name, at most 256 bytes, without control characters".into())
    } else {
        Ok(())
    }
}
fn is_named_map(path: &[String]) -> bool {
    matches!(path, [s] if s == "models" || s == "endpoints")
        || path == ["agent", "trusted_sources"]
        || matches!(path, [s, _, field] if s == "mcp" && (field == "env" || field == "headers"))
}
fn defaults() -> Result<Value, String> {
    let mut value = serde_json::to_value(Config::default()).map_err(|e| e.to_string())?;
    for key in ["models", "endpoints"] {
        value[key] = json!({});
    }
    for key in ["mcp", "lsp", "hooks"] {
        value[key] = json!([]);
    }
    Ok(value)
}
fn template(section: &str) -> Option<Value> {
    match section {
        "models" => serde_json::to_value(super::ModelSource::default()).ok(),
        "endpoints" => serde_json::to_value(super::ApiEndpoint::default()).ok(),
        "mcp" => serde_json::to_value(rook_mcp::ServerConfig::default()).ok(),
        "lsp" => serde_json::to_value(rook_lsp::ServerConfig::default()).ok(),
        "hooks" => serde_json::to_value(crate::hooks::HookConfig::default()).ok(),
        _ => None,
    }
}
fn at<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut at = value;
    for key in path {
        at = if at.is_array() { at.get(key.parse::<usize>().ok()?)? } else { at.get(key)? };
    }
    Some(at)
}
fn default_at(path: &[String]) -> Option<Value> {
    if path == ["sandbox", "mode"] {
        return serde_json::to_value(Config::default().sandbox.stance).ok();
    }
    if path.len() > 1 && is_named_map(&path[..path.len() - 1]) && path.len() > 2 {
        return Some(json!(""));
    }
    let first = path.first()?;
    if path.len() >= 2
        && let Some(template) = template(first)
    {
        return at(&template, &path[2..]).cloned().or_else(|| {
            (path.len() > 2 && at(&template, &path[2..path.len() - 1]).is_some_and(Value::is_array))
                .then(|| json!(""))
        });
    }
    at(&defaults().ok()?, path).cloned()
}
fn merge(base: &mut Value, extra: &Value) {
    match (base, extra) {
        (Value::Object(base), Value::Object(extra)) => {
            for (key, value) in extra {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, extra) => *base = extra.clone(),
    }
}
fn effective(raw: &Value) -> Result<Value, String> {
    let mut value = defaults()?;
    merge(&mut value, raw);
    // The old key is still accepted by Config. Show that line's real meaning
    // rather than an unwritten stance default beside it, and edit it in place.
    if raw.get("sandbox").and_then(|s| s.get("mode")).is_some()
        && raw.get("sandbox").and_then(|s| s.get("stance")).is_none()
        && let Some(sandbox) = value.get_mut("sandbox").and_then(Value::as_object_mut)
    {
        sandbox.remove("stance");
    }
    for key in ["mode", "stance"] {
        if let Some(setting) = value.get_mut("sandbox").and_then(|s| s.get_mut(key))
            && let Some(stance) = setting.as_str().and_then(rook_tools::policy::Stance::parse)
        {
            *setting = json!(stance.as_str());
        }
    }
    for section in ["models", "endpoints", "mcp", "lsp", "hooks"] {
        let entries: Vec<&mut Value> = match &mut value[section] {
            Value::Object(map) => map.values_mut().collect(),
            Value::Array(array) => array.iter_mut().collect(),
            _ => continue,
        };
        for entry in entries {
            if let Some(mut base) = template(section) {
                merge(&mut base, entry);
                *entry = base;
            }
        }
    }
    Ok(value)
}
fn item(value: &Value) -> Result<Item, String> {
    if let Value::Object(map) = value {
        let mut table = toml_edit::Table::new();
        for (key, value) in map {
            if !value.is_null() {
                table.insert(key, item(value)?);
            }
        }
        return Ok(Item::Table(table));
    }
    let wrapped = toml::to_string(&json!({"v":value})).map_err(|e| e.to_string())?;
    let mut doc = wrapped.parse::<DocumentMut>().map_err(|e| e.to_string())?;
    Ok(doc.remove("v").unwrap_or(Item::None))
}
fn materialize_arrays(
    doc: &mut DocumentMut,
    raw: &Value,
    effective: &Value,
    path: &[String],
) -> Result<(), String> {
    for end in 1..=path.len() {
        let prefix = &path[..end];
        if let Some(array) = at(effective, prefix).filter(|v| v.is_array())
            && at(raw, prefix).is_none()
        {
            patch(doc.as_item_mut(), prefix, Some(item(array)?))?;
        }
    }
    Ok(())
}
fn patch(node: &mut Item, path: &[String], replacement: Option<Item>) -> Result<(), String> {
    let Some((key, tail)) = path.split_first() else {
        *node = replacement.unwrap_or(Item::None);
        return Ok(());
    };
    if node.is_none() {
        *node = Item::Table(toml_edit::Table::new());
    }
    if let Some(table) = node.as_table_like_mut() {
        if tail.is_empty() {
            match replacement {
                Some(mut replacement) => {
                    // A value's suffix often explains why it is set. Keep it.
                    if let Some(old) = table.get(key).and_then(Item::as_value)
                        && let Some(new) = replacement.as_value_mut()
                    {
                        *new.decor_mut() = old.decor().clone();
                    }
                    table.insert(key, replacement);
                }
                None => {
                    table.remove(key);
                }
            }
            return Ok(());
        }
        if !table.contains_key(key) {
            table.insert(key, Item::Table(toml_edit::Table::new()));
        }
        return patch(table.get_mut(key).ok_or("missing table")?, tail, replacement);
    }
    let index: usize = key.parse().map_err(|_| "expected a list index")?;
    if let Some(array) = node.as_array_of_tables_mut() {
        if tail.is_empty() && replacement.is_none() {
            if index >= array.len() {
                return Err("list entry no longer exists".into());
            }
            array.remove(index);
            return Ok(());
        }
        let table = array.get_mut(index).ok_or("list entry no longer exists")?;
        let mut value = Item::Table(table.clone());
        patch(&mut value, tail, replacement)?;
        *table = value.into_table().map_err(|_| "expected a table")?;
        return Ok(());
    }
    if let Some(array) = node.as_array_mut() {
        if index >= array.len() {
            return Err("list entry no longer exists".into());
        }
        if tail.is_empty() && replacement.is_none() {
            array.remove(index);
            return Ok(());
        }
        let mut value = Item::Value(array.get(index).ok_or("missing entry")?.clone());
        patch(&mut value, tail, replacement)?;
        array.replace(index, value.into_value().map_err(|_| "expected a value")?);
        return Ok(());
    }
    Err("cannot open this value as a section".into())
}
fn append(node: &mut Item, path: &[String], value: Item) -> Result<(), String> {
    if let Some((key, tail)) = path.split_first() {
        if let Some(table) = node.as_table_like_mut() {
            return append(table.get_mut(key).ok_or("missing section")?, tail, value);
        }
        if let Some(array) = node.as_array_of_tables_mut() {
            let index: usize = key.parse().map_err(|_| "invalid list index")?;
            let table = array.get_mut(index).ok_or("missing entry")?;
            let mut child = Item::Table(table.clone());
            append(&mut child, tail, value)?;
            *table = child.into_table().map_err(|_| "expected table")?;
            return Ok(());
        }
        if let Some(array) = node.as_array_mut() {
            let index: usize = key.parse().map_err(|_| "invalid list index")?;
            let mut child = Item::Value(array.get(index).ok_or("missing entry")?.clone());
            append(&mut child, tail, value)?;
            array.replace(index, child.into_value().map_err(|_| "expected value")?);
            return Ok(());
        }
        return Err("not a collection".into());
    }
    if let Some(array) = node.as_array_of_tables_mut() {
        array.push(value.into_table().map_err(|_| "expected table")?);
    } else if let Some(array) = node.as_array_mut() {
        array.push(value.into_value().map_err(|_| "expected value")?);
    } else {
        return Err("not a list".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn path(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).into()).collect()
    }
    fn open(text: &str) -> (tempfile::TempDir, Editor) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, text).unwrap();
        let editor = Editor::open(file).unwrap();
        (dir, editor)
    }
    #[test]
    fn config_drafts_preserve_comments_validate_and_refuse_concurrent_writes() {
        let original = "# my config\n[agent] # model settings\nmax_steps = 42 # leave this explanation\nmodel = 'local'\n";
        let (_dir, mut editor) = open(original);
        editor.set(&path(&["agent", "max_steps"]), "64").unwrap();
        assert_eq!(std::fs::read_to_string(editor.file_path()).unwrap(), original, "drafts do not write");
        assert!(editor.set(&path(&["agent", "max_steps"]), "banana").is_err());
        assert!(editor.set(&path(&["agent", "compact_at"]), "2").is_err());
        editor.save().unwrap();
        let saved = std::fs::read_to_string(editor.file_path()).unwrap();
        assert!(saved.contains("max_steps = 64 # leave this explanation"), "{saved}");
        assert!(saved.contains("model = 'local'"));
        assert!(saved.contains("[agent] # model settings"));
        assert!(!editor.dirty());
        editor.set(&path(&["agent", "max_steps"]), "77").unwrap();
        std::fs::write(editor.file_path(), "# changed elsewhere\n").unwrap();
        assert!(editor.save().unwrap_err().contains("changed outside"));
        assert_eq!(std::fs::read_to_string(editor.file_path()).unwrap(), "# changed elsewhere\n");
    }
    #[test]
    fn invalid_model_drafts_are_repairable_and_never_replace_the_saved_file() {
        let original = "# keep this file until the whole draft is valid\n";
        let (_dir, mut editor) = open(original);
        editor.add(&path(&["models"]), "local").unwrap();
        for (field, value, missing) in
            [("model", "example", "api` is not set"), ("api", "openai", "url` is not set")]
        {
            editor.set(&path(&["models", "local", field]), value).unwrap();
            let why = editor.save().unwrap_err();
            assert!(why.contains(missing), "{why}");
            assert_eq!(std::fs::read_to_string(editor.file_path()).unwrap(), original);
        }
        editor.set(&path(&["models", "local", "url"]), "http://localhost:8080/v1").unwrap();
        editor.set(&path(&["models", "local", "key"]), "secret:not-resolved-here").unwrap();
        editor.save().unwrap();
        let saved = std::fs::read_to_string(editor.file_path()).unwrap();
        assert!(saved.contains("# keep this file"));
        for value in ["0.09", "0.91", "1"] {
            assert!(editor.set(&path(&["agent", "compact_at"]), value).is_err());
        }
        assert_eq!(std::fs::read_to_string(editor.file_path()).unwrap(), saved);
    }
    #[test]
    fn named_models_mcp_headers_and_arrays_can_be_added_edited_and_removed() {
        let (_dir, mut editor) = open("# original\n");
        let mcp = editor.add(&path(&["mcp"]), "docs").unwrap();
        assert!(editor.save().unwrap_err().contains("set command or url"));
        let mut command = mcp.clone();
        command.push("command".into());
        editor.set(&command, "test-server").unwrap();
        let argpath = path(&["mcp", "0", "args"]);
        editor.add(&argpath, "--flag").unwrap();
        editor.add(&argpath, "with spaces").unwrap();
        let header = editor.add(&path(&["mcp", "0", "headers"]), "X.Api.Key").unwrap();
        editor.set(&header, "secret-value").unwrap();
        let model = editor.add(&path(&["models"]), "local.v1").unwrap();
        let mut name = model.clone();
        name.push("model".into());
        editor.set(&name, "llama").unwrap();
        editor.set(&path(&["models", "local.v1", "api"]), "openai").unwrap();
        editor.set(&path(&["models", "local.v1", "url"]), "http://localhost:8080/v1").unwrap();
        let mut window = model.clone();
        window.push("context_window".into());
        editor.set(&window, "8192").unwrap();
        editor.save().unwrap();
        let config = Config::load_from(editor.file_path().into()).unwrap();
        assert_eq!(config.mcp[0].args, ["--flag", "with spaces"]);
        assert_eq!(config.mcp[0].headers["X.Api.Key"], "secret-value");
        assert_eq!(config.models["local.v1"].context_window, Some(8192));
        editor.remove(&path(&["mcp", "0", "args", "0"])).unwrap();
        editor.remove(&header).unwrap();
        editor.remove(&model).unwrap();
        editor.save().unwrap();
        let config = Config::load_from(editor.file_path().into()).unwrap();
        assert_eq!(config.mcp[0].args, ["with spaces"]);
        assert!(config.mcp[0].headers.is_empty());
        assert!(config.models.is_empty());
        editor.remove(&mcp).unwrap();
        editor.save().unwrap();
        assert!(Config::load_from(editor.file_path().into()).unwrap().mcp.is_empty());
    }
    #[test]
    fn editing_existing_array_tables_preserves_other_servers_and_default_lists() {
        let (_dir, mut editor) = open(
            "[[mcp]] # first\nname='one'\ncommand='first' # why\n\n[[mcp]] # second\nname='two'\ncommand='second'\n",
        );
        editor.set(&path(&["mcp", "0", "command"]), "changed").unwrap();
        editor.add(&path(&["mcp", "1", "args"]), "two words").unwrap();
        editor.add(&path(&["skill_sources"]), "/my/source").unwrap();
        editor.save().unwrap();
        let text = std::fs::read_to_string(editor.file_path()).unwrap();
        assert!(text.contains("# first") && text.contains("# second") && text.contains("# why"), "{text}");
        let config = Config::load_from(editor.file_path().into()).unwrap();
        assert_eq!(config.mcp[1].args, ["two words"]);
        assert_eq!(config.skill_sources.len(), Config::default().skill_sources.len() + 1);
        editor.remove(&path(&["skill_sources", "0"])).unwrap();
        editor.save().unwrap();
        assert_eq!(Config::load_from(editor.file_path().into()).unwrap().skill_sources, ["/my/source"]);
    }
    #[test]
    fn every_editable_field_has_help_including_optional_and_collection_fields() {
        let (_dir, mut editor) = open("");
        for section in ["models", "endpoints", "mcp", "lsp", "hooks"] {
            editor.add(&path(&[section]), "example").unwrap();
        }
        fn visit(editor: &Editor, path: &[String], seen: &mut usize) {
            for entry in editor.entries(path).unwrap() {
                assert!(!entry.help.help.starts_with("Unknown"), "missing help for {:?}", entry.path);
                assert!(!entry.help.help.is_empty());
                *seen += 1;
                if entry.value.is_object() {
                    visit(editor, &entry.path, seen);
                }
                if entry.value.is_array() && matches!(entry.path[0].as_str(), "mcp" | "lsp" | "hooks") {
                    visit(editor, &entry.path, seen);
                }
            }
        }
        let mut seen = 0;
        visit(&editor, &[], &mut seen);
        assert!(seen > 130, "only checked {seen} fields");
        let optional = editor
            .entries(&path(&["agent"]))
            .unwrap()
            .into_iter()
            .find(|e| e.path.last().unwrap() == "context_window")
            .unwrap();
        assert!(optional.default.is_null());
        assert_eq!(optional.help.kind, "integer");
    }

    #[test]
    fn prompt_actions_have_live_help_and_conflicting_keys_cannot_be_saved() {
        let (_dir, mut editor) = open("");
        let actions = editor.entries(&path(&["tui", "keys"])).unwrap();
        let undo = actions.iter().find(|entry| entry.path.last().unwrap() == "prompt.undo").unwrap();
        assert!(undo.help.help.contains("does not rewind files"));
        let key = path(&["tui", "keys", "prompt.undo", "0"]);
        editor.set(&key, "ctrl+p").unwrap();
        assert!(editor.save().is_err(), "the default palette binding participates in validation");
        editor.set(&key, "alt+u").unwrap();
        editor.save().unwrap();
    }
    #[test]
    fn legacy_approval_keys_keep_their_meaning_and_expose_every_supported_choice() {
        let (_dir, mut editor) = open("[sandbox]\nmode = 'auto' # legacy choice\n");
        let entries = editor.entries(&path(&["sandbox"])).unwrap();
        let mode = entries.iter().find(|e| e.path.last().unwrap() == "mode").unwrap();
        assert_eq!(mode.value, "autonomous");
        assert!(!entries.iter().any(|e| e.path.last().unwrap() == "stance"));
        assert_eq!(mode.help.choices.len(), rook_tools::policy::Stance::ALL.len());
        assert!(mode.help.choices.iter().any(|v| v == "free"));
        editor.set(&path(&["sandbox", "mode"]), "free").unwrap();
        editor.save().unwrap();
        assert_eq!(
            Config::load_from(editor.file_path().into()).unwrap().sandbox.stance,
            rook_tools::policy::Stance::Free
        );
        assert!(std::fs::read_to_string(editor.file_path()).unwrap().contains("# legacy choice"));
    }

    #[test]
    fn configuration_and_collection_limits_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, vec![b' '; MAX_CONFIG as usize + 1]).unwrap();
        assert!(std::fs::metadata(&file).unwrap().len() > MAX_CONFIG);
        assert!(Editor::open(file).err().unwrap().contains("1 MiB"));
        let sources: Vec<_> = (0..MAX_ITEMS).map(|i| format!("source-{i}")).collect();
        let (_dir, mut editor) = open(&toml::to_string(&json!({"skill_sources":sources})).unwrap());
        assert_eq!(editor.entries(&path(&["skill_sources"])).unwrap().len(), MAX_ITEMS);
        assert!(editor.add(&path(&["skill_sources"]), "overflow").unwrap_err().contains("256"));
    }
    #[cfg(unix)]
    #[test]
    fn saving_a_symlinked_config_keeps_the_link_and_makes_the_target_private() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfile");
        let link = dir.path().join("config.toml");
        std::fs::write(&target, "# dotfiles\n").unwrap();
        symlink(&target, &link).unwrap();
        let mut editor = Editor::open(link.clone()).unwrap();
        editor.set(&path(&["agent", "max_steps"]), "55").unwrap();
        editor.save().unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(Config::load_from(link).unwrap().agent.max_steps, 55);
        assert_eq!(target.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}
