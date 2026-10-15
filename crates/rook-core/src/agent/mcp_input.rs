//! Server-owned forms use native questions, then explicit review; values go to
//! that server, never into the model's arguments, prompt or durable receipts.
use super::AgentLoop;
use rook_mcp::elicitation::{
    ElicitationAction as Action, ElicitationOrigin, ElicitationResponse as Response, Elicitor,
    MAX_ELICITATION_BYTES, within_elicitation,
};
use rook_tools::ask::{Asker, Question};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

static WRITING: Mutex<()> = Mutex::new(());
const RECEIPT_BYTES: usize = 16384;
#[derive(Serialize, Deserialize)]
struct Ledger {
    scope: String,
    seen: BTreeMap<String, String>,
}
struct Dialog {
    store: Arc<rook_store::Store>,
    session: u128,
    scope: String,
    asker: Arc<dyn Asker>,
    vault: Arc<crate::Vault>,
    tool: String,
}
struct Receipt<'a> {
    dialog: &'a Dialog,
    hash: String,
    finished: bool,
}
impl Receipt<'_> {
    fn finish(&mut self, status: &str) -> std::result::Result<(), String> {
        let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut ledger) = self.dialog.load()? else { return Err("missing elicitation receipt".into()) };
        if ledger.scope != self.dialog.scope || ledger.seen.get(&self.hash).is_none_or(|s| s != "waiting") {
            return Err("elicitation receipt changed".into());
        }
        ledger.seen.insert(self.hash.clone(), status.into());
        self.dialog.save(&ledger)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Receipt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish("cancel");
        }
    }
}
impl Dialog {
    fn key(&self) -> String {
        format!("mcp-input/{}", rook_store::format_session_id(self.session))
    }
    fn load(&self) -> std::result::Result<Option<Ledger>, String> {
        let Some(bytes) = self
            .store
            .kv_get_limited(&self.key(), RECEIPT_BYTES)
            .map_err(|_| "cannot read elicitation receipts")?
        else {
            return Ok(None);
        };
        let ledger: Ledger = serde_json::from_slice(&bytes).map_err(|_| "invalid elicitation receipts")?;
        if ledger.scope.len() != 64
            || ledger.seen.len() > 32
            || ledger.seen.iter().any(|(key, status)| {
                key.len() != 64
                    || !key.bytes().all(|b| b.is_ascii_hexdigit())
                    || !["waiting", "accept", "decline", "cancel"].contains(&status.as_str())
            })
        {
            return Err("invalid elicitation receipts".into());
        }
        Ok(Some(ledger))
    }
    fn save(&self, ledger: &Ledger) -> std::result::Result<(), String> {
        crate::persistence::save_json(&self.store, &self.key(), ledger)
            .map_err(|_| "cannot save elicitation receipt")?;
        self.store.flush().map_err(|_| "cannot persist elicitation receipt")?;
        Ok(())
    }
    fn admit(&self, hash: &str) -> std::result::Result<bool, String> {
        let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut ledger = self
            .load()?
            .filter(|l| l.scope == self.scope)
            .unwrap_or_else(|| Ledger { scope: self.scope.clone(), seen: BTreeMap::new() });
        if ledger.seen.contains_key(hash) || ledger.seen.len() >= 32 {
            return Ok(false);
        }
        ledger.seen.insert(hash.into(), "waiting".into());
        self.save(&ledger)?;
        Ok(true)
    }
    async fn choose(&self, text: String, choices: Vec<String>) -> Option<String> {
        let answers = self.asker.ask_ephemeral(&[Question { question: text, choices, multi: false }]).await;
        let chosen = answers.first()?.chosen.as_slice();
        match chosen {
            [one] if one.len() <= 8192 => Some(one.clone()),
            _ => None,
        }
    }
    async fn pick(&self, field: &Field, prefix: &str) -> Option<Collected> {
        let mut actions = vec!["Enter or select a value".to_owned()];
        if field.default.is_some() {
            actions.push("Use the stated default".into());
        }
        if field.kind == "string" {
            actions.push("Use empty text".into());
        }
        if field.array {
            actions.push("Use empty selection".into());
        }
        if !field.required {
            actions.push("Omit this field".into());
        }
        actions.push("Cancel".into());
        let default = field.default.as_ref().map(|v| format!("\nDefault: {v}")).unwrap_or_default();
        let action = self
            .choose(
                format!("{prefix}\n{}\n{}\nType: {}{default}", field.label, field.description, field.kind),
                actions,
            )
            .await?;
        match action.as_str() {
            "Use the stated default" => return field.default.clone().map(Collected::Value),
            "Use empty text" if field.kind == "string" => return Some(Collected::Value(json!(""))),
            "Use empty selection" if field.array => return Some(Collected::Value(json!([]))),
            "Omit this field" if !field.required => return Some(Collected::Omit),
            "Enter or select a value" => {}
            _ => return None,
        }
        if !field.options.is_empty() {
            let mut selected = Vec::new();
            loop {
                let remaining: Vec<_> =
                    field.options.iter().filter(|(_, value)| !selected.contains(value)).collect();
                let mut page = 0;
                let value = loop {
                    let shown: Vec<_> = remaining.iter().skip(page * 3).take(3).collect();
                    let mut choices: Vec<_> = shown.iter().map(|(label, _)| label.clone()).collect();
                    let mut more = "More choices".to_owned();
                    while field.options.iter().any(|(label, value)| *label == more || *value == more) {
                        more.push('·');
                    }
                    if remaining.len() > 3 {
                        choices.push(more.clone());
                    }
                    let answer = self
                        .choose(format!("{prefix}\n{}\n{}", field.label, field.description), choices)
                        .await?;
                    if answer == more {
                        page = if (page + 1) * 3 < remaining.len() { page + 1 } else { 0 };
                        continue;
                    }
                    break field
                        .options
                        .iter()
                        .find(|(label, value)| answer == *label || answer == *value)
                        .map(|(_, value)| value.clone())?;
                };
                if selected.contains(&value) {
                    return None;
                }
                selected.push(value);
                if !field.array {
                    return selected.pop().map(|value| Collected::Value(Value::String(value)));
                }
                let next = self
                    .choose(
                        format!("{prefix}\nSelected for {}: {}", field.label, json!(selected)),
                        vec!["Done selecting".into(), "Add another".into(), "Cancel".into()],
                    )
                    .await?;
                if next == "Done selecting" {
                    return Some(Collected::Value(json!(selected)));
                }
                if next != "Add another" || selected.len() >= field.options.len() {
                    return None;
                }
            }
        }
        let choices = if field.kind == "boolean" { vec!["true".into(), "false".into()] } else { Vec::new() };
        let answer = self
            .choose(format!("{prefix}\nValue for {}\n{}", field.label, field.description), choices)
            .await?;
        if field.kind == "string" {
            Some(Collected::Value(Value::String(answer)))
        } else {
            serde_json::from_str(&answer).ok().map(Collected::Value)
        }
    }
    async fn form(&self, prefix: &str, form: &Form) -> std::result::Result<Response, String> {
        match self.choose(format!("{prefix}\nForm data goes to this MCP server. Do not enter passwords, API keys or payment credentials."),vec!["Fill in the form".into(),"Decline".into(),"Cancel".into()]).await.as_deref() {
            Some("Fill in the form")=>{},Some("Decline")=>return Ok(response(Action::Decline,None)),_=>return Ok(Response::cancel()),
        }
        for _ in 0..3 {
            let mut data = serde_json::Map::new();
            for field in &form.fields {
                let Some(value) = self.pick(field, prefix).await else { return Ok(Response::cancel()) };
                if let Collected::Value(value) = value {
                    data.insert(field.name.clone(), value);
                }
            }
            let data = Value::Object(data);
            let valid = form.validator.is_valid(&data)
                && form.fields.iter().all(|field| {
                    field.pattern.as_ref().is_none_or(|pattern| {
                        data.get(&field.name)
                            .and_then(Value::as_str)
                            .is_none_or(|value| pattern.is_match(value))
                    })
                })
                && within_elicitation(&data, 8192);
            let summary = if valid {
                format!("Review the exact response before sending:\n{data}")
            } else {
                "Some answers do not match the requested types or constraints. Revise them or cancel.".into()
            };
            let choices = if valid {
                vec!["Send this response", "Change answers", "Decline", "Cancel"]
            } else {
                vec!["Revise answers", "Decline", "Cancel"]
            };
            let chosen = self
                .choose(format!("{prefix}\n{summary}"), choices.into_iter().map(str::to_owned).collect())
                .await;
            match chosen.as_deref() {
                Some("Send this response") if valid => return Ok(response(Action::Accept, Some(data))),
                Some("Change answers" | "Revise answers") => {}
                Some("Decline") => return Ok(response(Action::Decline, None)),
                _ => return Ok(Response::cancel()),
            }
        }
        Ok(Response::cancel())
    }
}
fn response(action: Action, content: Option<Value>) -> Response {
    Response { action, content }
}

#[async_trait::async_trait]
impl Elicitor for Dialog {
    async fn elicit(
        &self,
        origin: &ElicitationOrigin,
        params: &Value,
    ) -> std::result::Result<Response, String> {
        if !within_elicitation(params, MAX_ELICITATION_BYTES) {
            return Err("elicitation exceeds its limit".into());
        }
        let encoded = params.to_string();
        if self.vault.redact(&encoded) != encoded {
            return Err("elicitation contains a known credential".into());
        }
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .filter(|s| safe(s, 2048))
            .ok_or("elicitation needs a bounded message")?;
        let mode = params.get("mode").and_then(Value::as_str).unwrap_or("form");
        let mut fingerprint = params.clone();
        let parsed_url = if mode == "url" {
            let raw =
                params.get("url").and_then(Value::as_str).filter(|s| safe(s, 8192)).ok_or("missing URL")?;
            let url = reqwest::Url::parse(raw).map_err(|_| "invalid elicitation URL")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("elicitation requires a credential-free HTTPS URL".into());
            }
            if !params.get("elicitationId").and_then(Value::as_str).is_some_and(|s| safe(s, 256)) {
                return Err("missing elicitation ID".into());
            }
            let mut identity = url.clone();
            identity.set_query(None);
            identity.set_fragment(None);
            fingerprint["url"] = json!(identity.as_str());
            if let Some(fields) = fingerprint.as_object_mut() {
                fields.remove("elicitationId");
            }
            Some(url)
        } else if mode == "form" {
            None
        } else {
            return Err("unsupported elicitation mode".into());
        };
        let form = if mode == "form" {
            Some(Form::parse(params.get("requestedSchema").ok_or("missing form schema")?)?)
        } else {
            None
        };
        let hash =
            rook_store::ObjectId::of(json!([origin.server, fingerprint]).to_string().as_bytes()).to_hex();
        if !self.admit(&hash)? {
            return Ok(Response::cancel());
        }
        let mut receipt = Receipt { dialog: self, hash, finished: false };
        let server: String = origin.server.chars().filter(|c| !c.is_control()).take(128).collect();
        let prefix = format!(
            "MCP server {server} · {} · invocation {}/{}\n{message}",
            self.tool, origin.connection, origin.request
        );
        let answer = if let Some(url) = parsed_url {
            let answer=self.choose(format!("{prefix}\nTarget host: {}\nOpen this HTTPS link yourself if you consent:\n{url}\nOpening the link does not confirm authorization completed.",url.host_str().unwrap_or_default()),vec!["I opened this link".into(),"Decline".into(),"Cancel".into()]).await;
            match answer.as_deref() {
                Some("I opened this link") => response(Action::Accept, None),
                Some("Decline") => response(Action::Decline, None),
                _ => Response::cancel(),
            }
        } else if let Some(form) = form {
            self.form(&prefix, &form).await?
        } else {
            Response::cancel()
        };
        receipt.finish(match answer.action {
            Action::Accept => "accept",
            Action::Decline => "decline",
            Action::Cancel => "cancel",
        })?;
        Ok(answer)
    }
}

fn safe(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}
enum Collected {
    Omit,
    Value(Value),
}
struct Field {
    name: String,
    label: String,
    description: String,
    kind: String,
    required: bool,
    default: Option<Value>,
    pattern: Option<regex::Regex>,
    options: Vec<(String, String)>,
    array: bool,
}
struct Form {
    fields: Vec<Field>,
    validator: jsonschema::Validator,
}
impl Form {
    fn parse(schema: &Value) -> std::result::Result<Self, String> {
        let invalid = || "unsupported or oversized flat elicitation schema".to_owned();
        let root = schema.as_object().ok_or_else(invalid)?;
        if root.keys().any(|k| {
            !["type", "properties", "required", "additionalProperties", "$schema", "title", "description"]
                .contains(&k.as_str())
        }) || schema["type"] != "object"
        {
            return Err(invalid());
        }
        let properties = schema["properties"]
            .as_object()
            .filter(|p| !p.is_empty() && p.len() <= 16)
            .ok_or_else(invalid)?;
        let required = match schema.get("required") {
            None => BTreeSet::new(),
            Some(Value::Array(items)) if items.len() <= 16 => items
                .iter()
                .map(|i| {
                    i.as_str().filter(|s| properties.contains_key(*s)).map(str::to_owned).ok_or_else(invalid)
                })
                .collect::<std::result::Result<BTreeSet<_>, _>>()?,
            _ => return Err(invalid()),
        };
        let mut sanitized = schema.clone();
        let mut fields = Vec::new();
        for (name, property) in properties {
            if !safe(name, 128) {
                return Err(invalid());
            }
            let lower = name.to_ascii_lowercase().replace(['_', '-', ' '], "");
            if [
                "password",
                "passwd",
                "apikey",
                "accesstoken",
                "refreshtoken",
                "privatekey",
                "creditcard",
                "cardnumber",
                "cvv",
                "secret",
            ]
            .iter()
            .any(|secret| lower.contains(secret))
            {
                return Err("sensitive data requires URL mode".into());
            }
            let object = property.as_object().ok_or_else(invalid)?;
            if object.keys().any(|k| {
                ![
                    "type",
                    "title",
                    "description",
                    "default",
                    "minLength",
                    "maxLength",
                    "format",
                    "pattern",
                    "enum",
                    "oneOf",
                    "minimum",
                    "maximum",
                    "items",
                    "minItems",
                    "maxItems",
                ]
                .contains(&k.as_str())
            }) {
                return Err(invalid());
            }
            let kind = property["type"]
                .as_str()
                .filter(|k| ["string", "integer", "number", "boolean", "array"].contains(k))
                .ok_or_else(invalid)?;
            let label = property.get("title").and_then(Value::as_str).unwrap_or(name);
            let description = property.get("description").and_then(Value::as_str).unwrap_or("");
            if !safe(label, 256) || (!description.is_empty() && !safe(description, 512)) {
                return Err(invalid());
            }
            let pattern = match property.get("pattern") {
                None => None,
                Some(Value::String(pattern)) if pattern.len() <= 512 => Some(
                    regex::RegexBuilder::new(pattern)
                        .size_limit(1024 * 1024)
                        .nest_limit(64)
                        .build()
                        .map_err(|_| invalid())?,
                ),
                _ => return Err(invalid()),
            };
            if let Some(object) = sanitized["properties"][name].as_object_mut() {
                object.remove("pattern");
            }
            if kind == "array" {
                let items = property.get("items").and_then(Value::as_object).ok_or_else(invalid)?;
                if items.keys().any(|key| !["type", "enum", "oneOf", "anyOf"].contains(&key.as_str()))
                    || items.get("type").and_then(Value::as_str) != Some("string")
                {
                    return Err(invalid());
                }
            }
            let options =
                choices(if kind == "array" { property.get("items").ok_or_else(invalid)? } else { property })?;
            if kind == "array" && options.is_empty() {
                return Err(invalid());
            }
            fields.push(Field {
                name: name.clone(),
                label: label.into(),
                description: description.into(),
                kind: kind.into(),
                required: required.contains(name),
                default: property.get("default").cloned(),
                pattern,
                options,
                array: kind == "array",
            });
        }
        let validator =
            jsonschema::options().should_validate_formats(true).build(&sanitized).map_err(|_| invalid())?;
        Ok(Self { fields, validator })
    }
}
fn choices(schema: &Value) -> std::result::Result<Vec<(String, String)>, String> {
    let invalid = || "invalid elicitation choices".to_owned();
    let mut result = Vec::new();
    if let Some(values) = schema.get("enum") {
        for value in values.as_array().filter(|v| v.len() <= 32).ok_or_else(invalid)? {
            let value = value.as_str().filter(|v| safe(v, 256)).ok_or_else(invalid)?;
            result.push((value.into(), value.into()));
        }
    } else if let Some(values) = schema.get("oneOf").or_else(|| schema.get("anyOf")) {
        for value in values.as_array().filter(|v| v.len() <= 32).ok_or_else(invalid)? {
            let fields = value.as_object().ok_or_else(invalid)?;
            if fields.keys().any(|k| !["const", "title"].contains(&k.as_str())) {
                return Err(invalid());
            }
            let literal = value["const"].as_str().filter(|v| safe(v, 256)).ok_or_else(invalid)?;
            let title = value.get("title").and_then(Value::as_str).unwrap_or(literal);
            if !safe(title, 256) {
                return Err(invalid());
            }
            result.push((format!("{title} ({literal})"), literal.into()));
        }
    }
    let labels: BTreeSet<_> = result.iter().map(|(label, _)| label).collect();
    let values: BTreeSet<_> = result.iter().map(|(_, value)| value).collect();
    if labels.len() != result.len() || values.len() != result.len() {
        return Err(invalid());
    }
    Ok(result)
}

impl AgentLoop<'_> {
    pub(super) fn mcp_elicitor(&self, call: &rook_llm::ToolCall) -> Option<Arc<dyn Elicitor>> {
        if self.checking
            || self.review_only
            || self.tools.get(&call.name)?.mcp_source(&call.arguments).is_none()
        {
            return None;
        }
        let asker = self.asker.clone()?;
        let boundary = if let Some(run) = &self.managed_work {
            json!([run.id, run.generation])
        } else {
            json!(self.began_at_seq)
        };
        let scope = rook_store::ObjectId::of(
            json!([self.rook.workspace.to_string_lossy(), boundary]).to_string().as_bytes(),
        )
        .to_hex();
        Some(Arc::new(Dialog {
            store: self.rook.store.clone(),
            session: self.session,
            scope,
            asker,
            vault: self.vault.clone(),
            tool: call.name.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rook_tools::ask::Answer;
    use std::collections::VecDeque;

    struct Person {
        answers: Mutex<VecDeque<Option<String>>>,
        questions: Mutex<Vec<Question>>,
    }
    impl Person {
        fn new(answers: &[Option<&str>]) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.iter().map(|v| v.map(str::to_owned)).collect()),
                questions: Mutex::new(Vec::new()),
            })
        }
    }
    #[async_trait::async_trait]
    impl Asker for Person {
        async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
            self.questions.lock().unwrap().extend_from_slice(questions);
            questions
                .iter()
                .map(|q| Answer {
                    question: q.question.clone(),
                    chosen: self
                        .answers
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected extra prompt")
                        .into_iter()
                        .collect(),
                })
                .collect()
        }
    }
    fn dialog(dir: &std::path::Path, asker: Arc<dyn Asker>) -> Dialog {
        Dialog {
            store: Arc::new(rook_store::Store::open(dir.join("store")).unwrap()),
            session: 1,
            scope: "a".repeat(64),
            asker,
            vault: Arc::new(crate::Vault::empty()),
            tool: "mcp_service_collect".into(),
        }
    }
    fn origin() -> ElicitationOrigin {
        ElicitationOrigin { server: "service".into(), connection: 4, request: 2, method: "tools/call".into() }
    }
    fn form() -> Value {
        json!({"message":"Choose a display name","requestedSchema":{"type":"object","properties":{"name":{"type":"string","default":"unconfirmed-default","minLength":2}},"required":["name"],"additionalProperties":false}})
    }

    #[tokio::test]
    async fn accepted_forms_require_review_and_receipts_keep_no_values() {
        let dir = tempfile::tempdir().unwrap();
        let person = Person::new(&[
            Some("Fill in the form"),
            Some("Enter or select a value"),
            Some("PRIVATE-FORM-VALUE"),
            Some("Send this response"),
        ]);
        let dialog = dialog(dir.path(), person.clone());
        let answer = dialog.elicit(&origin(), &form()).await.unwrap();
        assert_eq!(answer.action, Action::Accept);
        assert_eq!(answer.content.unwrap()["name"], "PRIVATE-FORM-VALUE");
        assert!(
            person.questions.lock().unwrap().last().unwrap().question.contains("Review the exact response")
        );
        let saved = dialog.store.kv_get_limited(&dialog.key(), RECEIPT_BYTES).unwrap().unwrap();
        assert!(!String::from_utf8(saved).unwrap().contains("PRIVATE-FORM-VALUE"));
        assert_eq!(dialog.elicit(&origin(), &form()).await.unwrap().action, Action::Cancel);
        assert_eq!(person.questions.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn refusal_absence_and_unsubmitted_defaults_never_accept_or_repeat_after_reopen() {
        for answers in [
            vec![Some("Decline")],
            vec![None],
            vec![Some("Fill in the form"), None],
            vec![Some("Fill in the form"), Some("Use the stated default"), None],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let person = Person::new(&answers);
            let dialog = dialog(dir.path(), person.clone());
            let answer = dialog.elicit(&origin(), &form()).await.unwrap();
            assert_ne!(answer.action, Action::Accept);
            assert!(answer.content.is_none());
            let count = person.questions.lock().unwrap().len();
            drop(dialog);
            let reopened = super::tests::dialog(dir.path(), person.clone());
            assert_eq!(reopened.elicit(&origin(), &form()).await.unwrap().action, Action::Cancel);
            assert_eq!(person.questions.lock().unwrap().len(), count);
        }
    }

    #[tokio::test]
    async fn url_refusal_is_not_repeated_for_a_fresh_authorization_nonce() {
        let dir = tempfile::tempdir().unwrap();
        let person = Person::new(&[Some("Decline")]);
        let dialog = dialog(dir.path(), person.clone());
        let mut params = json!({"mode":"url","message":"Authorize service","url":"https://EXAMPLE.org/authorize?state=one","elicitationId":"one"});
        assert_eq!(dialog.elicit(&origin(), &params).await.unwrap().action, Action::Decline);
        params["url"] = json!("https://example.org/authorize?state=two");
        params["elicitationId"] = json!("two");
        assert_eq!(dialog.elicit(&origin(), &params).await.unwrap().action, Action::Cancel);
        let questions = person.questions.lock().unwrap();
        assert_eq!(questions.len(), 1);
        assert!(questions[0].question.contains("Target host: example.org"));
    }

    #[tokio::test]
    async fn a_timed_out_or_aborted_native_question_releases_its_owner_without_reprompting() {
        for abort in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let asker = Arc::new(rook_tools::ask::ChannelAsker::new(
                tx,
                std::time::Duration::from_millis(20),
                Default::default(),
            ));
            let dialog = Arc::new(dialog(dir.path(), asker.clone()));
            let task = tokio::spawn({
                let dialog = dialog.clone();
                async move { dialog.elicit(&origin(), &form()).await }
            });
            let request = rx.recv().await.unwrap();
            assert!(asker.is_waiting());
            if abort {
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            } else {
                assert_eq!(task.await.unwrap().unwrap().action, Action::Cancel);
            }
            assert!(!asker.is_waiting());
            asker.answer(&request.id, vec![vec!["Fill in the form".into()]]);
            assert_eq!(dialog.elicit(&origin(), &form()).await.unwrap().action, Action::Cancel);
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn forms_refuse_sensitive_nested_unbounded_and_reference_schemas() {
        for schema in [
            json!({"type":"object","properties":{"api_key":{"type":"string"}}}),
            json!({"type":"object","properties":{"nested":{"type":"object"}}}),
            json!({"type":"object","properties":{"choice":{"type":"array","items":{"type":"string","enum":["ok"],"$ref":"https://invalid/schema"}}}}),
        ] {
            assert!(Form::parse(&schema).is_err());
        }
        let many = json!({"type":"object","properties":(0..17).map(|i|(format!("f{i}"),json!({"type":"string"}))).collect::<serde_json::Map<_,_>>()});
        assert!(Form::parse(&many).is_err());
    }
}
