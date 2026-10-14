//! Session-bound inspection and explicit release of trusted execution resources.
use crate::{CoreError, Result, Rook};
pub use rook_tools::environments::Request;
use serde_json::Value;

pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 8192 {
        return Err(CoreError::Other("environment request exceeds 8 KiB".into()));
    }
    if text.trim().is_empty() {
        return Ok(Request::Inspect);
    }
    Ok(serde_json::from_str(text)?)
}
impl Rook {
    pub async fn environment_request(&self, owner: u128, request: Request) -> Result<Value> {
        let meta = self
            .store
            .get_session_limited(owner, 256 * 1024)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(owner)))?;
        let root = self.workspace.canonicalize().map_err(|e| CoreError::Other(e.to_string()))?;
        if std::path::Path::new(&meta.workspace).canonicalize().ok().as_ref() != Some(&root) {
            return Err(CoreError::Other("environment owner belongs to another workspace".into()));
        }
        let ctx = crate::agent::tool_context(&self.config, &self.workspace, &self.output_dir);
        let env = rook_tools::environments::Context {
            settings: self.config.execution_environment.clone(),
            pool: self.execution_environments.clone(),
            state_root: self.output_dir.clone(),
        };
        let recovering = matches!(request, Request::Recover { .. });
        let stopping = matches!(request, Request::Stop { .. });
        if recovering {
            if let Some(reason) = self.recovery_block(owner)? {
                return Err(CoreError::Other(reason));
            }
            if self.config.sandbox.stance == rook_tools::policy::Stance::ReadOnly {
                return Err(CoreError::Other(
                    "SSH writeback recovery changes workspace files; read-only policy refuses it".into(),
                ));
            }
        }
        let action = if recovering {
            "recover"
        } else if stopping {
            "stop"
        } else {
            "release"
        };
        let journal = if recovering || stopping || matches!(request, Request::Release { .. }) {
            let source = env
                .pool
                .request(&ctx, &env, Request::Inspect)
                .await
                .map_err(|e| CoreError::Other(e.to_string()))?;
            let session = rook_store::new_session_id();
            let mut meta = rook_store::SessionMeta::new(
                session,
                if recovering {
                    "SSH writeback recovery"
                } else if stopping {
                    "SSH retained command stop"
                } else {
                    "Execution environment release"
                },
                root.display().to_string(),
                rook_store::now_unix(),
            );
            meta.parent = Some(owner);
            meta.tags = vec![format!("environment-{action}")];
            self.store.create_session(&meta)?;
            let journal = crate::execution::Journal::start(self, session, None, false)?;
            journal.admit(if recovering {
                "Recover the explicitly inspected SSH file changes"
            } else if stopping {
                "Stop the explicitly inspected retained SSH command"
            } else {
                "Release the explicitly inspected owned execution resource"
            })?;
            self.store.append_event(
                session,
                rook_store::NewEvent::new(
                    rook_store::EventKind::Note,
                    rook_store::Kind::Message,
                    &serde_json::to_vec(&source)?,
                )
                .label(&format!("environment-{action}-source")),
            )?;
            journal.begin(
                if recovering {
                    "recover_environment"
                } else if stopping {
                    "stop_environment"
                } else {
                    "release_environment"
                },
                &serde_json::to_string(&request)?,
                true,
                false,
                None,
            )?;
            Some((session, journal))
        } else {
            None
        };
        let mut result = match env.pool.request(&ctx, &env, request).await {
            Ok(result) => result,
            Err(error) => {
                return Err(CoreError::Other(format!(
                    "{error}{}",
                    journal
                        .as_ref()
                        .map(|(session, _)| format!(
                            "; inspect environment operation session {}",
                            rook_store::format_session_id(*session)
                        ))
                        .unwrap_or_default()
                )));
            }
        };
        if let Some((session, journal)) = journal {
            result["session"] = serde_json::json!(rook_store::format_session_id(session));
            let body = serde_json::to_vec(&result)?;
            self.store.append_event(
                session,
                rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &body)
                    .label(&format!("execution-environment-{action}")),
            )?;
            journal.complete(
                &format!("Owned environment {action} confirmed; no original command replayed"),
                None,
                None,
            )?;
            journal.finish("complete", None)?;
        }
        Ok(result)
    }
}
