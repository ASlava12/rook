//! Language-server offers, installation and completion reporting.

use super::delegation::short;
use super::effects::Shown;
use super::{AgentLoop, Reported};
use rook_store::EventKind;
use rook_tools::policy::Stance;

/// A fetch under way, and what it is for, so its report names it.
pub(super) type Fetching = (
    &'static crate::install::Recipe,
    tokio::task::JoinHandle<std::result::Result<crate::install::Installed, String>>,
);

/// Where a language server goes when the agent installs one.
enum How {
    System,
    Fetch,
}

fn system_risk(recipe: &crate::install::Recipe) -> (serde_json::Value, rook_tools::policy::Risk) {
    let command = recipe.system_command().unwrap_or_default();
    (serde_json::json!({ "command": command }), rook_tools::policy::Risk::Execute(command))
}

impl<'a> AgentLoop<'a> {
    /// Once per session. What is installed serves the next session: the pool
    /// of servers is built by the front end before the first turn, which is
    /// what keeps rust-analyzer from re-indexing every turn, and the same fact
    /// means one added now is not in it yet. The report says so.
    pub(super) async fn offer_language_server(&self) {
        let config = &self.rook.config.agent;
        if !config.install_servers || self.depth > 0 || self.rook.offered_server(self.session).unwrap_or(true)
        {
            return;
        }
        let missing = crate::lsp::missing_here(&self.rook.config, &self.rook.workspace);
        let Some((language, recipe)) = missing
            .iter()
            .find_map(|(language, command)| crate::install::recipe_for(command).map(|r| (*language, r)))
        else {
            return;
        };
        self.rook.note_offered_server(self.session).ok();

        let local = format!("fetch into {}", crate::paths::servers_dir().display());
        let system = recipe.system_command().map(|c| format!("run `{c}`"));
        let how = match self.policy.stance() {
            // Nothing may change the machine, so there is nothing to ask: a
            // question whose every answer the policy then refuses is a wasted
            // one. Said once, for whoever reads the outcome.
            Stance::ReadOnly => {
                self.report(Reported::Open(format!(
                    "this workspace has {language} files and no {} — read-only, so nothing was \
                     installed; `rook lsp install {}` does it by hand",
                    recipe.command, recipe.command
                )));
                return;
            }
            // A person chooses where it goes. Without one there is nobody to
            // choose, and the question waits for whoever reads the outcome.
            Stance::Assist => {
                let Some(asker) = &self.asker else {
                    self.report(Reported::Open(format!(
                        "this workspace has {language} files and no {} — `rook lsp install {}` fetches \
                         one, or run `{}` yourself",
                        recipe.command,
                        recipe.command,
                        system.as_deref().unwrap_or("the system's installer")
                    )));
                    return;
                };
                let mut choices = vec![local.clone()];
                choices.extend(system.clone());
                choices.push("not now".into());
                let question =
                    format!("There are {language} files here and no {}. Install it?", recipe.command);
                let asked = rook_tools::ask::Question { question, choices, multi: false };
                let answer =
                    asker.ask(&[asked]).await.into_iter().next().and_then(|a| a.chosen.into_iter().next());
                match answer.as_deref() {
                    Some(chosen) if chosen == local => {
                        self.answered(&self.fetch_risk(recipe).1);
                        How::Fetch
                    }
                    Some(chosen) if Some(chosen) == system.as_deref() => {
                        self.answered(&system_risk(recipe).1);
                        How::System
                    }
                    _ => {
                        self.report(Reported::Decision(format!(
                            "{} not installed — declined",
                            recipe.command
                        )));
                        return;
                    }
                }
            }
            Stance::Autonomous => How::Fetch,
            Stance::Free => match system {
                Some(_) => How::System,
                None => How::Fetch,
            },
        };

        // Started rather than waited for. What it fetches serves the next
        // session, so the turn that pays for it gets nothing back — and it was
        // paid before the first request, which is a person's first minute in a
        // new project spent watching nothing happen. It is collected at the end
        // of the turn, where its report belongs anyway.
        if matches!(how, How::Fetch) {
            let (args, risk) = self.fetch_risk(recipe);
            if let Some(refusal) = self.gate_risk("lsp install", &args, risk, Shown::Nothing).await {
                self.report(Reported::Open(refusal));
                return;
            }
            let env = self.rook.env().clone();
            // Resolved here rather than inside the future: the future outlives
            // this borrow of `self`, and a proxy read through it would not
            // compile — which is the right way round, since what it must carry
            // is the setting as it was when the download was approved.
            let proxy = self.rook.config.proxy.for_install();
            let receipt = match self.record_background("harness:lsp install", recipe.command) {
                Ok(receipt) => receipt,
                Err(error) => {
                    self.report(Reported::Open(error.to_string()));
                    return;
                }
            };
            let fetching = tokio::spawn(async move {
                let result = match crate::install::Installer::new(crate::paths::servers_dir(), &proxy) {
                    Ok(installer) => installer.install(recipe, &env).await,
                    Err(error) => Err(error),
                };
                receipt
                    .finish(match &result {
                        Ok(_) => "installed",
                        Err(error) => error,
                    })
                    .map_err(|e| e.to_string())?;
                result
            });
            if let Ok(mut slot) = self.installing.lock() {
                *slot = Some((recipe, fetching));
            }
            return;
        }
        let done = match how {
            How::System => match self.install_with_system(recipe).await {
                Ok(said) => Ok(said),
                // The machine's way failed; the state directory is the fallback,
                // and the report names both.
                Err(first) => self
                    .install_by_fetching(recipe)
                    .await
                    .map(|done| done.describe())
                    .map_err(|second| format!("{first}; then {second}")),
            },
            How::Fetch => self.install_by_fetching(recipe).await.map(|done| done.describe()),
        };
        match done {
            Ok(said) => {
                let said = format!("{said} — it serves from the next session on");
                self.rook.log(self.session, EventKind::Note, "lsp install", &said).ok();
                self.report(Reported::Decision(said));
            }
            Err(why) => {
                let said = format!(
                    "could not install {}: {why} — `rook lsp install {}` by hand, or say how",
                    recipe.command, recipe.command
                );
                self.rook.log(self.session, EventKind::Note, "lsp install", &said).ok();
                self.report(Reported::Open(said));
            }
        }
    }

    /// A person who chose an install through the asker has answered; the
    /// approver asking about the command or the download it takes would be
    /// the same question twice. Granted for the run rather than once, which
    /// is the grant the policy has, and a deny rule still comes first.
    fn answered(&self, risk: &rook_tools::policy::Risk) {
        self.policy.grant_for_run(&risk.subject());
    }

    async fn install_with_system(
        &self,
        recipe: &crate::install::Recipe,
    ) -> std::result::Result<String, String> {
        let (args, risk) = system_risk(recipe);
        let command = recipe.system_command().ok_or("no system installer for it")?;
        if let Some(refusal) = self.gate_risk("run_command", &args, risk, Shown::Nothing).await {
            return Err(refusal);
        }
        let receipt =
            self.record_background("harness:system install", &command).map_err(|e| e.to_string())?;
        let result = self.tools.call(&self.tool_ctx, "run_command", &args).await;
        receipt
            .finish(&match &result {
                Ok(out) => self.vault.redact(&out.content),
                Err(error) => error.to_string(),
            })
            .map_err(|e| e.to_string())?;
        let out = result.map_err(|e| e.to_string())?;
        match out.is_error {
            false => Ok(format!("installed {} with `{command}`", recipe.command)),
            true => Err(format!("`{command}` failed: {}", short(&out.content))),
        }
    }

    async fn install_by_fetching(
        &self,
        recipe: &crate::install::Recipe,
    ) -> std::result::Result<crate::install::Installed, String> {
        let (args, risk) = self.fetch_risk(recipe);
        if let Some(refusal) = self.gate_risk("lsp install", &args, risk, Shown::Nothing).await {
            return Err(refusal);
        }
        let installer = crate::install::Installer::new(
            crate::paths::servers_dir(),
            &self.rook.config.proxy.for_install(),
        )?;
        let receipt =
            self.record_background("harness:lsp install", recipe.command).map_err(|e| e.to_string())?;
        let result = installer.install(recipe, self.rook.env()).await;
        receipt
            .finish(match &result {
                Ok(_) => "installed",
                Err(error) => error,
            })
            .map_err(|e| e.to_string())?;
        result
    }

    /// What fetching a server is, for the policy: a command for the sources
    /// that are one, a request to the release host for the one that is a
    /// download.
    fn fetch_risk(&self, recipe: &crate::install::Recipe) -> (serde_json::Value, rook_tools::policy::Risk) {
        let into = crate::paths::servers_dir().join(recipe.command).join("current");
        match recipe.command_into(&into) {
            Some((command, _)) => {
                (serde_json::json!({ "command": command }), rook_tools::policy::Risk::Execute(command))
            }
            None => {
                let api = "https://api.github.com";
                (serde_json::json!({ "url": api }), rook_tools::policy::Risk::Network(api.into()))
            }
        }
    }

    /// A server fetched once is one somebody has to remember to update. Past
    /// the configured age, once per session: an autonomous turn fetches again,
    /// one with a person asks, and one with nobody to ask leaves it for whoever
    /// reads the outcome.
    pub(super) async fn offer_server_update(&self) {
        let config = &self.rook.config.agent;
        let after = config.server_update_after_days;
        if !config.install_servers
            || after == 0
            || self.depth > 0
            || self.rook.offered_update(self.session).unwrap_or(true)
        {
            return;
        }
        let stale = crate::install::stale(
            &crate::paths::servers_dir(),
            std::time::Duration::from_secs(after.saturating_mul(86_400)),
        );
        if stale.is_empty() {
            return;
        }
        self.rook.note_offered_update(self.session).ok();
        let named = stale
            .iter()
            .map(|(r, tag, days)| format!("{} ({tag}, {days} days ago)", r.command))
            .collect::<Vec<_>>();
        let named = named.join(", ");

        let fetch = match (self.policy.stance(), &self.asker) {
            (Stance::Autonomous | Stance::Free, _) => true,
            (Stance::Assist, Some(asker)) => {
                let question = format!("Fetched more than {after} days ago: {named}. Update now?");
                let choices = vec!["update now".to_string(), "not now".to_string()];
                let asked = rook_tools::ask::Question { question, choices, multi: false };
                let answer =
                    asker.ask(&[asked]).await.into_iter().next().and_then(|a| a.chosen.into_iter().next());
                let chosen = answer.as_deref() == Some("update now");
                if chosen {
                    for (recipe, ..) in &stale {
                        self.answered(&self.fetch_risk(recipe).1);
                    }
                }
                chosen
            }
            (Stance::Assist, None) | (Stance::ReadOnly, _) => {
                self.report(Reported::Open(format!(
                    "fetched more than {after} days ago: {named} — `rook lsp update` fetches them again"
                )));
                return;
            }
        };
        if !fetch {
            self.report(Reported::Decision(format!("not updated — declined: {named}")));
            return;
        }
        for (recipe, before, _) in stale {
            let said = match self.install_by_fetching(recipe).await {
                Ok(done) if done.tag == before => Ok(format!("{} already at {before}", recipe.command)),
                Ok(done) => Ok(format!(
                    "{} {before} → {} — it serves from the next session on",
                    recipe.command, done.tag
                )),
                Err(why) => {
                    Err(format!("could not update {}: {why} — `rook lsp update` by hand", recipe.command))
                }
            };
            match said {
                Ok(said) => {
                    self.rook.log(self.session, EventKind::Note, "lsp update", &said).ok();
                    self.report(Reported::Decision(said));
                }
                Err(said) => {
                    self.rook.log(self.session, EventKind::Note, "lsp update", &said).ok();
                    self.report(Reported::Open(said));
                }
            }
        }
    }

    /// What the fetch started at the beginning of the turn came to, said
    /// where every other decision of the turn is said.
    pub(super) async fn collect_install(&self) {
        let Some((recipe, fetching)) = self.installing.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        let done = match fetching.await {
            Ok(done) => done.map(|done| done.describe()),
            Err(e) => Err(format!("the fetch did not finish: {e}")),
        };
        let said = match done {
            Ok(said) => format!("{said} — it serves from the next session on"),
            Err(why) => format!(
                "could not install {}: {why} — `rook lsp install {}` by hand, or say how",
                recipe.command, recipe.command
            ),
        };
        self.rook.log(self.session, EventKind::Note, "lsp install", &said).ok();
        match said.starts_with("could not") {
            true => self.report(Reported::Open(said)),
            false => self.report(Reported::Decision(said)),
        }
    }
}
