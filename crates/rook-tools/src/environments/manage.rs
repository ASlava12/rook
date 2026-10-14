use super::*;
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Inspect,
    PrepareRelease,
    PrepareRecover,
    PrepareStop,
    Stop { token: String, note: String },
    Recover { token: String, note: String },
    Read { call: String, which: String, path: String, offset: u64 },
    Changes { call: String, after: Option<String> },
    Release { token: String, note: String },
}
impl container::Resource {
    pub(super) async fn inspect(&self) -> Result<Value> {
        let files = rook_contain::files::RecoveryFiles::open(
            &self.state_root,
            Path::new("execution-environments"),
            true,
        )
        .map_err(|e| invalid(e.to_string()))?;
        let bytes = match files.read(Path::new(&format!("{}.json", self.identity)), 16384) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(invalid(e.to_string())),
        };
        let saved = bytes
            .as_ref()
            .map(|bytes| serde_json::from_slice::<container::State>(bytes))
            .transpose()
            .map_err(|e| invalid(format!("invalid retained execution state: {e}")))?;
        let mut observed = None;
        if let Some(saved) = &saved {
            if saved.version != 1
                || saved.identity != self.identity
                || !(saved.container == self.name() || immutable_id(&saved.container))
            {
                return Err(invalid("retained execution resource identity mismatch"));
            }
            if saved.status != "released" {
                let outcome = self.administrative(&["inspect".into(), "--type".into(), "container".into(),
                    "--format".into(), r#"{"id":{{json .Id}},"owner":{{json (index .Config.Labels "rook.execution")}},"running":{{json .State.Running}}}"#.into(), saved.container.clone()]).await?;
                if !outcome.is_error {
                    let text = outcome
                        .content
                        .strip_prefix("exit 0\n")
                        .ok_or_else(|| invalid("invalid Docker inspection response"))?;
                    let value: Value =
                        serde_json::from_str(text.trim()).map_err(|e| invalid(e.to_string()))?;
                    let id =
                        value.get("id").and_then(Value::as_str).filter(|id| immutable_id(id)).ok_or_else(
                            || invalid("Docker inspection did not return an immutable resource ID"),
                        )?;
                    if value["owner"] != self.identity
                        || (immutable_id(&saved.container) && id != saved.container)
                    {
                        return Err(invalid(
                            "container ownership changed; refusing to manage an unrelated resource",
                        ));
                    }
                    observed = Some(value);
                } else {
                    // A failed inspect can mean an unavailable daemon. Independently ask
                    // for exact matching IDs; only a successful empty result proves absence.
                    let filter = if immutable_id(&saved.container) {
                        format!("id={}", saved.container)
                    } else {
                        format!("name=^/{}$", saved.container)
                    };
                    let listed = self
                        .administrative(&[
                            "ps".into(),
                            "--all".into(),
                            "--no-trunc".into(),
                            "--filter".into(),
                            filter,
                            "--format".into(),
                            "{{.ID}}".into(),
                        ])
                        .await?;
                    if listed.is_error
                        || listed.content.strip_prefix("exit 0\n").is_none_or(|s| !s.trim().is_empty())
                    {
                        return Err(invalid(
                            "resource inspection is uncertain; restore Docker connectivity and inspect again",
                        ));
                    }
                }
            }
        }
        Ok(json!({"identity":self.identity,"workspace":self.root,"saved":saved,
            "source":bytes.as_ref().map(identity).transpose()?,"observed":observed,"busy":self.gate.available_permits()==0,"foreign_owner_active":saved.as_ref().is_some_and(|state|container::foreign_owner_alive(state,&self.owner))}))
    }
    pub(super) async fn manage(&self, request: Request) -> Result<Value> {
        if matches!(request, Request::Inspect | Request::PrepareRelease) {
            let mut view = self.inspect().await?;
            if matches!(request, Request::PrepareRelease) {
                if view["saved"].is_null() || view["saved"]["status"] == "released" {
                    return Err(invalid("this environment has no retained resource to release"));
                }
                if view["foreign_owner_active"] == true {
                    return Err(invalid(
                        "another execution process owns this resource; stop it through its active frontend",
                    ));
                }
                if view["busy"] == true {
                    return Err(invalid("environment has an owned active command or job; stop it first"));
                }
                view["token"] = json!(identity(&view)?);
                view["action"] = json!("release");
            }
            return Ok(view);
        }
        let Request::Release { token, note } = request else {
            return Err(invalid("invalid management operation"));
        };
        if token.len() != 64
            || !token.bytes().all(|b| b.is_ascii_hexdigit())
            || note.trim().is_empty()
            || note.len() > 4096
        {
            return Err(invalid(
                "release needs the inspected token and a nonempty note of at most 4096 bytes",
            ));
        }
        let _permit = self
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| invalid("environment has an owned active command or job; stop it first"))?;
        let mut current = self.inspect().await?;
        // This call itself holds the gate; tokens describe an idle resource.
        current["busy"] = json!(false);
        if current["foreign_owner_active"] == true {
            return Err(invalid("another execution process still owns this resource"));
        }
        if current["saved"].is_null() || current["saved"]["status"] == "released" {
            return Err(invalid("this resource was already released"));
        }
        if identity(&current)? != token {
            return Err(invalid("execution state changed; prepare release again"));
        }
        if let Some(id) = current["observed"]["id"].as_str() {
            // Preserve the inspected immutable ID before attempting release, even when
            // this pool only loaded a prior process's saved name.
            *self.container_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_owned());
            // The immutable ID and immutable ownership label were checked above.
            if !self.control_args(&["rm", "--force", id]) {
                self.uncertain.store(true, std::sync::atomic::Ordering::Release);
                return Err(invalid("container release was not confirmed; inspect again before continuing"));
            }
        }
        if current["observed"].is_null()
            && let Some(id) = current["saved"]["container"].as_str().filter(|id| immutable_id(id))
        {
            *self.container_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_owned());
        }
        self.state("released")?;
        self.initialized.store(false, std::sync::atomic::Ordering::Release);
        self.uncertain.store(false, std::sync::atomic::Ordering::Release);
        *self.container_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(
            json!({"state":"released","identity":self.identity,"note":note,"replayed":false,"released_resource":current["observed"]}),
        )
    }
}
fn immutable_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
