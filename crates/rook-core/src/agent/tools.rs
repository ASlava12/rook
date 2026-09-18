//! Tool dispatch, execution receipts, skills, memory and documentation.

use super::delegation::{Crew, Nursery};
use super::effects::Shown;
use super::{
    AgentLoop, CHANGES_THINGS, DELEGATE, DOCS, FIND_SKILL, FORGET, LOAD_SKILL, PLAN, Progress, RECALL,
    REMEMBER, STANCE, SUBAGENTS, TurnOutcome, VERIFY, WRITE_SKILL,
};
use crate::error::{CoreError, Result};
use crate::hooks::{self};
use rook_store::EventKind;

/// A set, read for whoever asked.
///
/// Both addresses on every answer: the local one, which is what the answer was
/// made of, and the source, which is what somebody else can check. A model
/// asked for "the link" has no way to know there are two unless it is holding
/// both — and the local copy alone is a citation of ourselves.
fn answer_from(set: &crate::docs::DocSet, question: &str, preamble: String) -> String {
    let reference = crate::docs::reference(&set.topic, &set.version);
    let mut out = preamble;
    out.push_str(&format!(
        "{} ({}) — kept here as {reference}, read {} from {} page(s).\n",
        set.topic,
        set.version,
        crate::docs::age(set.fetched_at),
        set.pages.len()
    ));

    let passages = match question.is_empty() {
        true => Vec::new(),
        false => set.passages(question, DOC_PASSAGES),
    };
    if passages.is_empty() && !question.is_empty() {
        out.push_str(
            "\nNothing in the local copy is about that in those words. What it does cover is \
             below; `refresh` reads the site again, and `web_search` looks wider.\n",
        );
    }
    for (text, url) in passages {
        out.push_str(&format!("\n[from {url}]\n{}\n", rook_tools::elide_middle(&text, DOC_PASSAGE_BYTES)));
    }

    // Always, and numbered. A model handed four passages and wanting the rest
    // asked the same question again, because nothing said there was a whole
    // page to read or how to ask for one — it looped until the turn ended.
    out.push_str("\nThe pages it was made from — `page: n` reads one whole:\n");
    for (n, page) in set.pages.iter().enumerate() {
        out.push_str(&format!("{}. {} — {}\n", n + 1, page.title, page.url));
    }
    out
}

/// One page of a set, as it was read.
///
/// The rest of what a set holds, for a model that has seen the passages and
/// wants what is around them. Bounded, because a documentation page can be a
/// hundred kilobytes and the middle of one is where a context window goes.
fn whole_page(set: &crate::docs::DocSet, at: usize) -> String {
    let Some(page) = at.checked_sub(1).and_then(|at| set.pages.get(at)) else {
        return format!(
            "{} ({}) has {} page(s), so there is no page {at} — ask for one of them, or ask a \
             question and get the passages that answer it.",
            set.topic,
            set.version,
            set.pages.len()
        );
    };
    format!(
        "{} — read from {}, kept in {}.\n\n{}",
        page.title,
        page.url,
        crate::docs::reference(&set.topic, &set.version),
        rook_tools::elide_middle(&page.text, DOC_PAGE_BYTES)
    )
}

/// How many passages one `docs` answer carries, and how long each may be.
///
/// A documentation page is mostly navigation, and handing a model the whole of
/// one to answer a sentence is how a context window goes. Four paragraphs is
/// enough to answer from and short enough to read.
const DOC_PASSAGES: usize = 4;

const DOC_PASSAGE_BYTES: usize = 1_200;

/// And one whole page, when the passages were not enough: a documentation page
/// is a few thousand words, and the ones that are not are mostly navigation.
const DOC_PAGE_BYTES: usize = 8_000;

impl<'a> AgentLoop<'a> {
    pub(super) async fn dispatch_recorded<'f>(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
        crew: &'f Crew<'a>,
        nursery: &mut Nursery<'f>,
    ) -> Result<(String, bool)>
    where
        'a: 'f,
    {
        let effects = !self.hooks.is_empty()
            || self
                .tools
                .get(&call.name)
                .map(|tool| tool.risk(&call.arguments) != rook_tools::policy::Risk::ReadOnly)
                .unwrap_or_else(|| CHANGES_THINGS.contains(&call.name.as_str()));
        if effects && let Some(reason) = self.rook.recovery_block(self.session)? {
            self.rook.log(
                self.session,
                EventKind::ToolCall,
                &call.name,
                &self.vault.redact(&call.arguments.to_string()),
            )?;
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &reason)?;
            return Ok((reason, true));
        }
        let journal = self
            .execution
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| CoreError::Other("execution receipt is missing".into()))?;
        let background = call.name == "run_command"
            && call.arguments.get("background").and_then(serde_json::Value::as_bool) == Some(true);
        journal.begin(
            &call.name,
            &self.vault.redact(&call.arguments.to_string()),
            effects,
            background,
            self.tool_ctx.jobs.as_deref(),
        )?;
        *self.launched_job.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let result = self.dispatch(call, outcome, on_progress, crew, nursery).await;
        let job = self.launched_job.lock().unwrap_or_else(|e| e.into_inner()).take();
        journal.complete(&result.0, self.tool_ctx.jobs.as_deref(), job.as_deref())?;
        Ok(result)
    }

    /// The text the model sees, and whether the call failed — which the outcome
    /// knows and the text only hints at.
    async fn dispatch<'f>(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
        crew: &'f Crew<'a>,
        nursery: &mut Nursery<'f>,
    ) -> (String, bool)
    where
        'a: 'f,
    {
        // Refused before it is recorded, and the order is the point: a verdict
        // from a checker that called nothing is reported as unproven, and a
        // reach for a tool it was never given is not a call it made. Counting it
        // would let a check reach for `write_skill`, be refused, and have its
        // recollection stand as evidence.
        if self.checking && CHANGES_THINGS.contains(&call.name.as_str()) {
            let refusal = format!("{}: a check may not change anything", call.name);
            return (refusal, true);
        }

        // Before the gate, not after: a call whose arguments did not parse has
        // no risk worth weighing, and asking somebody to approve running the
        // empty string is a question with no answer.
        if let Some(unusable) = rook_tools::unusable_arguments(&call.name, &call.arguments) {
            let refusal = unusable.to_string();
            self.rook.log(self.session, EventKind::Error, &call.name, &refusal).ok();
            return (refusal, true);
        }

        outcome.tools_called.push(call.name.clone());

        if call.name == crate::worktrees::TOOL {
            if call.arguments.get("action").and_then(|v| v.as_str()) == Some("remove") {
                let tree = match crate::worktrees::owned(self.rook, self.session, &call.arguments) {
                    Ok((_, tree)) => tree,
                    Err(why) => return (why.to_string(), true),
                };
                let risk = rook_tools::policy::Risk::Write(vec![tree.path.display().to_string()]);
                if let Some(refusal) = self
                    .gate_risk(
                        &call.name,
                        &call.arguments,
                        risk,
                        Shown::Text("Remove this worktree. discard=true deletes its unmerged edits."),
                    )
                    .await
                {
                    self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
                    return (refusal, true);
                }
            }
            let (text, failed) = match crate::worktrees::inspect(
                self.rook,
                self.session,
                &call.arguments,
                &self.vault,
            )
            .await
            {
                Ok(text) => (self.vault.redact(&text), false),
                Err(why) => (why.to_string(), true),
            };
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
            return (text, failed);
        }

        if call.name == crate::results::READ_RESULT {
            let (text, failed) = match crate::results::read(self.rook, self.session, &call.arguments) {
                Ok(text) => (self.vault.redact(&text), false),
                Err(why) => (why.to_string(), true),
            };
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
            return (text, failed);
        }

        if call.name == VERIFY {
            let text = self.verify(&call.arguments, outcome, on_progress).await;
            self.rook.log(self.session, EventKind::ToolResult, VERIFY, &text).ok();
            return (text, false);
        }

        if call.name == DELEGATE {
            let text = self.delegate(&call.arguments, outcome, on_progress, crew, nursery).await;
            self.rook.log(self.session, EventKind::ToolResult, DELEGATE, &text).ok();
            return (text, false);
        }

        if call.name == STANCE {
            let text = self.request_stance(&call.arguments).await;
            self.rook.log(self.session, EventKind::ToolResult, STANCE, &text).ok();
            return (text, false);
        }

        if call.name == SUBAGENTS {
            let text = self.subagents(&call.arguments, outcome, nursery).await;
            self.rook.log(self.session, EventKind::ToolResult, SUBAGENTS, &text).ok();
            return (text, false);
        }

        match call.name.as_str() {
            REMEMBER | FORGET | RECALL => {
                let text = self.memory_tool(&call.name, &call.arguments, outcome);
                self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
                return (text, false);
            }
            _ => {}
        }

        if call.name == PLAN {
            let steps: Vec<serde_json::Value> =
                call.arguments.get("steps").and_then(|s| s.as_array()).cloned().unwrap_or_default();
            let list: Vec<String> = steps
                .iter()
                .map(|s| {
                    let done = s.get("done").and_then(serde_json::Value::as_bool).unwrap_or(false);
                    let text = s.get("step").and_then(serde_json::Value::as_str).unwrap_or("");
                    format!("- [{}] {text}", if done { "x" } else { " " })
                })
                .collect();
            if list.is_empty() {
                return ("a plan needs at least one step".into(), true);
            }
            let plan = list.join("\n");
            if let Err(e) = self.rook.set_plan(self.session, &plan) {
                return (format!("could not keep the plan: {e}"), true);
            }
            let left = steps
                .iter()
                .filter(|s| !s.get("done").and_then(serde_json::Value::as_bool).unwrap_or(false))
                .count();
            let said = format!("plan kept, {left} step(s) left:\n{plan}");
            self.rook.log(self.session, EventKind::ToolResult, PLAN, &said).ok();
            return (said, false);
        }

        if call.name == LOAD_SKILL {
            let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            return match self.rook.skills().resolve(name, self.rook.env()) {
                Ok(resolved) => {
                    outcome.skills_loaded.push(resolved.skill.id());
                    let said = self.skill_source(&resolved);
                    self.rook.log(self.session, EventKind::SkillLoaded, &resolved.skill.id(), &said).ok();
                    (said, false)
                }
                // The reason matters: "needs docker >=27" is actionable, "not
                // found" sends the model looking for a typo that is not there.
                // It is logged as well as returned: a skill that never loaded is
                // otherwise invisible when reading the transcript afterwards.
                Err(e) => {
                    let mut message = format!("could not load skill {name:?}: {e}");
                    for card in self.rook.skills().search(name, self.rook.env(), 5) {
                        message.push_str(&format!("\n- {}: {}", card.name, card.description));
                    }
                    self.rook.log(self.session, EventKind::Error, LOAD_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if call.name == DOCS {
            let text = self.documentation(&call.arguments).await;
            let failed = text.starts_with("could not") || text.starts_with("no ");
            let kind = match failed {
                true => EventKind::Error,
                false => EventKind::ToolResult,
            };
            self.rook.log(self.session, kind, DOCS, &text).ok();
            return (text, failed);
        }

        if call.name == FIND_SKILL {
            let query = call.arguments.get("query").and_then(|q| q.as_str()).unwrap_or_default();
            let Some(name) = call.arguments.get("install").and_then(|n| n.as_str()) else {
                // Searching reads; only installing writes.
                let text = self.skills_matching(query);
                self.rook.log(self.session, EventKind::ToolResult, FIND_SKILL, &text).ok();
                return (text, false);
            };

            let target = crate::paths::user_skills_dir().join(name);
            let risk = rook_tools::policy::Risk::Write(vec![target.display().to_string()]);
            if let Some(refusal) = self.gate_risk(FIND_SKILL, &call.arguments, risk, Shown::Nothing).await {
                self.rook.log(self.session, EventKind::Error, FIND_SKILL, &refusal).ok();
                return (refusal, true);
            }
            return match self.rook.install_skill(name) {
                Ok(path) => {
                    outcome.skills_written.push(name.to_string());
                    let message = format!(
                        "installed skill {name:?} to {}. Load it to see what it says.",
                        path.display()
                    );
                    self.rook.log(self.session, EventKind::Note, FIND_SKILL, &message).ok();
                    (message, false)
                }
                Err(e) => {
                    // The same call usually carries the search that led to the
                    // name, and dropping it turned a failed install into a dead
                    // end: a small model asked to install "rust" while searching
                    // for "config.rs", got only the refusal, asked again, and
                    // gave up — holding, unused, the answer it had read two
                    // steps earlier. The search it also asked for still runs.
                    let mut message = format!("could not install {name:?}: {e}");
                    if !query.is_empty() {
                        message.push_str(&format!("\n\n{}", self.skills_matching(query)));
                    }
                    self.rook.log(self.session, EventKind::Error, FIND_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if call.name == WRITE_SKILL {
            // It writes files a user would call theirs, so it answers to the
            // policy like any other write.
            let target = crate::paths::user_skills_dir()
                .join(call.arguments.get("name").and_then(|n| n.as_str()).unwrap_or("?"));
            // Every file by name, not just the directory: a skill that lays
            // down a script is asking to write a program, and the approval
            // should say which.
            let mut writing = vec![target.join("SKILL.md").display().to_string()];
            if let Some(files) = call.arguments.get("files").and_then(|f| f.as_object()) {
                writing.extend(files.keys().map(|rel| target.join(rel).display().to_string()));
            }
            let risk = rook_tools::policy::Risk::Write(writing);
            // The body is the whole of what it would write, so the body is the
            // preview: there is nothing on disk to diff it against.
            let body = call.arguments.get("body").and_then(|b| b.as_str());
            let shown = body.map(Shown::Text).unwrap_or(Shown::Nothing);
            if let Some(refusal) = self.gate_risk(WRITE_SKILL, &call.arguments, risk, shown).await {
                self.rook.log(self.session, EventKind::Error, WRITE_SKILL, &refusal).ok();
                return (refusal, true);
            }
            return match serde_json::from_value(call.arguments.clone())
                .map_err(|e| CoreError::Other(format!("{WRITE_SKILL}: {e}")))
                .and_then(|skill: crate::service::AuthoredSkill| {
                    self.rook.write_skill(&skill).map(|path| (skill.name, path))
                }) {
                Ok((name, path)) => {
                    outcome.skills_written.push(name.clone());
                    // Where it went, and how to get it back: skills live in
                    // the agent's own directory, which is outside the
                    // workspace, so a model that reads the path out of this
                    // message and hands it to `read_file` is refused for
                    // being outside — which is what happened, twice, before
                    // the sentence was here. `load_skill` and not `find_skill`:
                    // the first reads what is installed, which this now is, and
                    // the second searches the sources you could install from,
                    // where a skill just written by hand will never appear. The
                    // wrong one of those two was named here for a day, and the
                    // next smoke run showed a model following the advice into
                    // "no source offers a skill called \"config_port\"".
                    // The person is told where it went; the model is not.
                    // A path here has been read as an instruction twice: first
                    // handed to `read_file` and refused for being outside the
                    // workspace, and then — when the state directory was a
                    // temporary one — judged ephemeral, so the model went
                    // hunting for "the real skills store" and spent a turn
                    // trying to write into it. Where a skill lives is the
                    // agent's business; what the model needs is that it is
                    // installed and how to read it back.
                    let note = format!("wrote skill {name:?} to {}", path.display());
                    // A note rather than a kind of its own: a new `EventKind`
                    // is a record older builds cannot decode, and the log is
                    // just as readable with the fact in the label.
                    self.rook.log(self.session, EventKind::Note, WRITE_SKILL, &note).ok();
                    let message = format!(
                        "wrote skill {name:?}. It is installed for this agent — read it back with \
                         `{LOAD_SKILL}`, and it is offered to later sessions in the catalog."
                    );
                    // As a tool result as well, because that is the kind the
                    // replay turns back into an answer. Every other tool the
                    // loop implements logs one; this one logged a note, which
                    // reaches nobody, so the next turn replayed the call with
                    // "no result was recorded: the turn did not finish" under
                    // it — a model reading that about a skill it had just
                    // written has every reason to doubt the skill exists.
                    self.rook.log(self.session, EventKind::ToolResult, WRITE_SKILL, &message).ok();
                    (message, false)
                }
                Err(e) => {
                    let message = format!("could not write the skill: {e}");
                    self.rook.log(self.session, EventKind::Error, WRITE_SKILL, &message).ok();
                    self.rook.log(self.session, EventKind::ToolResult, WRITE_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if let Some(refusal) = self.gate(call).await {
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
            return (refusal, true);
        }

        // `_writing` is held across the call and dropped when it returns: the
        // window it protects is the one between the checkpoint and the write.
        let (_writing, unprotected) = match self.checkpoint_before(call) {
            Ok(pair) => pair,
            Err(refusal) => {
                self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
                return (refusal, true);
            }
        };
        // A command names no paths, so nothing was checkpointed and nothing
        // can be put back. What can be had is the list of what it wrote, and
        // without it a turn that ran `sed -i` reports no files changed at all.
        let watching = self.watching_a_command(call).then(std::time::SystemTime::now);
        // What the files this call names are like before it writes them. The
        // answer is discarded: it is the baseline, and what it is worth is the
        // difference from it afterwards.
        let will_write = self.will_write(call);
        let unseen: Vec<_> = will_write.iter().filter(|p| self.unseen(p)).cloned().collect();
        let _ = self.what_this_broke(&unseen).await;
        // A call that takes a while says so while it takes it, and whoever is
        // watching hears it as it happens. Without this a long command and a
        // wedged one are the same await and the same unchanging line: the tool
        // knows which, and had nowhere to say it.
        let (say, mut said) = tokio::sync::mpsc::unbounded_channel::<String>();
        let ctx = rook_tools::ToolContext { watching: Some(say), ..self.tool_ctx.clone() };
        let calling = self.tools.call(&ctx, &call.name, &call.arguments);
        tokio::pin!(calling);
        let outcome = loop {
            tokio::select! {
                // What it says goes out before the call is noticed to have
                // ended, so its last word is not lost to the ending.
                biased;
                Some(word) = said.recv() => {
                    on_progress(Progress::Working { call: &call.name, said: &word });
                }
                done = &mut calling => break done,
            }
        };
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => rook_tools::ToolOutcome::error(format!("tool error: {e}")),
        };
        // A command names no paths, so what it wrote is discovered rather than
        // declared — and is checked the same way.
        let wrote = match watching {
            Some(since) => self.note_what_was_written(since),
            None => will_write,
        };
        if !outcome.is_error {
            self.note_paths(&wrote);
        }
        // A checker's reading is not recorded. The registry holds one holder
        // per path, so a look from any session makes every other session's
        // overwrite stale until it looks again — right for a sub-task, which
        // may have changed the file, and a false alarm from a loop that has no
        // writing tools: the goal check read a file and the turn it was
        // checking was then refused the fix the check had just asked for.
        if !outcome.is_error
            && !self.checking
            && let Some(tool) = self.tools.get(&call.name)
        {
            let seen: Vec<std::path::PathBuf> = tool
                .observed_paths(&call.arguments)
                .iter()
                .filter_map(|p| self.tool_ctx.resolve(p).ok())
                .collect();
            self.rook.touched(self.session, &seen);
        }
        if call.name == "run_command" {
            *self.launched_job.lock().unwrap_or_else(|e| e.into_inner()) =
                outcome.meta.get("job").and_then(serde_json::Value::as_str).map(str::to_owned);
        }
        let mut text = match self.after_tool(call, &outcome).await {
            Some(extra) => format!("{}\n\n{extra}", outcome.content),
            None => outcome.content,
        };
        if let Some(note) = unprotected {
            text.push_str(&format!("\n\n{note}"));
        }
        if !outcome.is_error
            && let Some(broken) = self.what_this_broke(&wrote).await
        {
            text.push_str(&format!("\n\n{broken}"));
        }
        // The one place a tool's answer becomes context, so the one place a
        // value has to be taken back out of it: before the model reads it and
        // before the store keeps it. A command's output, a page, a file and an
        // MCP server's answer are all the same question here.
        let text = self.vault.redact(&text);
        match self.rook.log(self.session, EventKind::ToolResult, &call.name, &text) {
            Ok(seq) if matches!(call.name.as_str(), "run_command" | "job") => {
                if let Err(why) = crate::results::register_output(self.rook, self.session, seq, &outcome.meta)
                {
                    tracing::warn!("full output could not be registered: {why}");
                }
            }
            Err(why) => tracing::warn!("tool result could not be saved: {why}"),
            _ => {}
        }
        (text, outcome.is_error)
    }

    /// `post_tool` hooks, whose output the model sees appended to the result.
    ///
    /// The whole outcome, not just its text: `meta` is where a tool says which
    /// MCP server answered, whether a command timed out, and how much of a file
    /// was returned — the facts a hook would otherwise have to parse back out of
    /// prose written for a model.
    async fn after_tool(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &rook_tools::ToolOutcome,
    ) -> Option<String> {
        if self.hooks.is_empty() {
            return None;
        }
        let payload = self.payload(serde_json::json!({
            "tool": call.name,
            "input": call.arguments,
            "result": outcome.content,
            "is_error": outcome.is_error,
            "truncated": outcome.truncated,
            "full_bytes": outcome.full_bytes,
            "meta": outcome.meta,
        }));
        self.hooks.run(hooks::Event::PostTool, &call.name, &payload).await.context()
    }

    /// The last `count` exchanges as plain text, for a child asked to inherit
    /// context. Deliberately flattened: the child gets what was said, not a
    /// replayable tool-call history it cannot answer for.
    pub(super) fn recent_exchanges(&self, count: usize) -> Result<String> {
        let entries = self.rook.transcript(self.session, 0, usize::MAX, 2000)?;
        let mut tail: Vec<String> = entries
            .iter()
            .rev()
            .filter(|e| e.kind == "user" || e.kind == "assistant")
            .take(count)
            .map(|e| format!("{}: {}", e.kind, e.body))
            .collect();
        tail.reverse();
        Ok(format!("Context from the conversation that delegated this:\n{}", tail.join("\n")))
    }

    fn memory_tool(&self, name: &str, args: &serde_json::Value, outcome: &mut TurnOutcome) -> String {
        let string = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            REMEMBER => {
                let text = string("text");
                if text.trim().is_empty() {
                    return "remember needs non-empty text".into();
                }
                let scope = match string("scope").as_str() {
                    "global" => crate::memory::Scope::Global,
                    _ => crate::memory::Scope::Project(self.rook.workspace.display().to_string()),
                };
                let tags = args
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let mut fact = crate::memory::Fact::new(text, scope).with_tags(tags).from_turn(
                    self.session,
                    self.rook.store.get_session(self.session).ok().flatten().map(|m| m.next_seq).unwrap_or(0),
                );
                fact.pinned = args.get("pinned").and_then(|p| p.as_bool()).unwrap_or(false);
                let (id, remembered) = (fact.id.clone(), fact.text.clone());
                // Named, not merged: only the model knows whether this replaces
                // the older fact, narrows it, or contradicts it.
                let close = self.rook.similar_facts(&fact.text).unwrap_or_default();
                match self.rook.remember(fact, Some(format!("learned in turn {}", outcome.steps))) {
                    Ok(crate::memory::Learned::ScopedElsewhere(scope)) => format!(
                        "already remembered as [{id}], but scoped to {} — this workspace will \
                         not see it. Remember it with scope \"global\" to widen it.",
                        scope.label()
                    ),
                    Ok(crate::memory::Learned::Unchanged) => format!("already remembered as [{id}]"),
                    Ok(learned) => {
                        if learned == crate::memory::Learned::New {
                            outcome.facts_learned.push(remembered.clone());
                        }
                        let mut reply = format!("remembered as [{id}]");
                        for other in close {
                            reply.push_str(&format!(
                                "\nclose to [{}] {:?} — `forget` it if this replaces it",
                                other.id, other.text
                            ));
                        }
                        reply
                    }
                    Err(e) => format!("could not remember: {e}"),
                }
            }
            FORGET => match self.rook.forget(&string("id"), Some("forgotten by the agent".into())) {
                Ok(Some(fact)) => {
                    outcome.facts_forgotten.push(fact.text.clone());
                    format!("forgot [{}] {}", fact.id, fact.text)
                }
                // Ids here are all one shape, so a model reading "checked in
                // session 01M1N…" off a `verify` result hands that id to this
                // — three readings running, twice in one turn. Saying what the
                // id actually names costs a store lookup nobody notices.
                Ok(None) => {
                    let id = string("id");
                    match self.rook.session_named(&id) {
                        Ok(_) => format!(
                            "{id:?} is a session, not a fact — `{FORGET}` removes what was \
                             remembered, and nothing was remembered under that id"
                        ),
                        Err(_) => format!("no fact {id:?} to forget"),
                    }
                }
                Err(e) => format!("could not forget: {e}"),
            },
            _ => {
                match self.rook.recall(&string("query"), self.rook.config.memory.context_budget_tokens * 2) {
                    Ok(facts) if facts.is_empty() => "nothing remembered about that".into(),
                    Ok(facts) => {
                        facts.iter().map(|f| format!("[{}] {}", f.id, f.text)).collect::<Vec<_>>().join("\n")
                    }
                    Err(e) => format!("could not recall: {e}"),
                }
            }
        }
    }

    /// The same decision for something the toolbox does not own. A pseudo-tool
    /// that changes the machine has to pass here too, or `readonly` means
    /// "readonly except for the tools the loop implements itself".
    /// What the configured sources offer for a query, as the model reads it.
    ///
    /// One function because two callers ask the same question: a search on its
    /// own, and a search that came alongside an install that failed.
    /// Answer from the documentation kept here, gathering it first on a miss.
    ///
    /// The order matters and is the whole point: look locally, and only reach
    /// for the network when there is nothing to look at. A model that answers
    /// about a technology from what it was trained on is answering from a
    /// snapshot it cannot date, and neither it nor the person reading can tell
    /// which sentences are still true. What comes back here has a source url on
    /// every passage, so both can.
    async fn documentation(&self, args: &serde_json::Value) -> String {
        let topic = args.get("topic").and_then(|t| t.as_str()).unwrap_or_default().trim();
        if topic.is_empty() {
            return "docs needs a topic: the technology to look up.".into();
        }
        let version = args
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(crate::docs::LATEST);
        let question = args.get("question").and_then(|q| q.as_str()).unwrap_or_default().trim();
        let refresh = args.get("refresh").and_then(|r| r.as_bool()).unwrap_or(false);
        let page = args.get("page").and_then(|p| p.as_u64()).map(|p| p as usize);

        let kept = match self.rook.docs(topic, Some(version)) {
            Ok(kept) => kept,
            Err(e) => return format!("could not read the documentation kept here: {e}"),
        };
        if let Some(set) = kept.as_ref().filter(|_| !refresh) {
            return match page {
                Some(page) => whole_page(set, page),
                None => answer_from(set, question, String::new()),
            };
        }

        // A miss is not always a miss. A narrower question gathered an hour ago
        // leaves `docs/redis-persistence/latest` on the disk, and asking about
        // `redis` used to walk past it to the network — five fetches to arrive
        // at the same site. Only when it actually answers the question, and
        // always saying which set answered: a passage from a neighbouring topic
        // presented as this one is a different claim.
        if kept.is_none()
            && !question.is_empty()
            && let Ok(near) = self.rook.docs_like(topic)
            && let Some(set) = near.into_iter().find(|set| !set.passages(question, 1).is_empty())
        {
            let preamble = format!(
                "nothing is kept for {topic:?} itself, and this answers from {}, which is here. \
                 `docs {{\"topic\": {topic:?}, \"refresh\": true}}` gathers the wider set if this \
                 is not what was meant.\n\n",
                crate::docs::reference(&set.topic, &set.version)
            );
            return answer_from(&set, question, preamble);
        }

        // Only a miss costs the network — and it is asked for before it is
        // approved, so a person is never prompted about a fetch that config
        // has already ruled out.
        let sources = match self.rook.doc_sources() {
            Ok(sources) => sources,
            Err(why) => {
                return format!(
                    "no documentation for {topic:?} is kept here and none can be fetched: {why}. \
                     Answer from memory if that is all there is, and say that is what it is."
                );
            }
        };
        let risk = rook_tools::policy::Risk::Network(format!("{topic} documentation"));
        if let Some(refusal) = self.gate_risk(DOCS, args, risk, Shown::Nothing).await {
            // What to do instead, said here rather than left to the model.
            // Refused in an unattended run, one asked the same thing again
            // until the step limit: the refusal told it to stop, and stopping
            // is not what a model does when it has been told to check first.
            return format!(
                "{refusal}\n\nNothing is kept for {topic:?} here and nothing may be fetched, so \
                 there is no copy to answer from. Answer from what you know, say that it is \
                 unsourced, and do not ask for this again in this turn."
            );
        }
        // A refresh of something already here asks the sources what changed
        // rather than downloading it again: each page is asked for with the
        // validator its server gave, and a 304 costs a round trip and no body.
        // So refreshing is cheap enough to do, which is the point — a check
        // nobody can afford is a copy nobody updates.
        let (reference, set, notes) = match kept {
            Some(kept) => match self.rook.recheck_docs(&kept, &sources).await {
                Ok((reference, checked)) => {
                    let said = match checked.changed.len() {
                        0 => "checked every page against its source: none had changed".to_string(),
                        n => format!(
                            "{n} page(s) had changed and were read again: {}",
                            checked.changed.join(", ")
                        ),
                    };
                    let notes: Vec<String> = std::iter::once(said).chain(checked.unreadable).collect();
                    (reference, checked.set, notes)
                }
                Err(e) => return format!("could not check {topic:?} against its sources: {e}"),
            },
            None => match self.rook.gather_docs(topic, version, &sources).await {
                Ok((reference, set, notes)) => (
                    reference,
                    set,
                    notes
                        .into_iter()
                        .map(|note| format!("some of what came back was unreadable: {note}"))
                        .collect(),
                ),
                Err(e) => return format!("could not gather documentation for {topic:?}: {e}"),
            },
        };
        let mut preamble = format!(
            "{} page(s) in {reference}, which later turns and later sessions read without \
             fetching again.",
            set.pages.len()
        );
        for note in notes.iter().take(3) {
            preamble.push_str(&format!("\n{note}"));
        }
        preamble.push_str("\n\n");
        match page {
            Some(page) => format!("{preamble}{}", whole_page(&set, page)),
            None => answer_from(&set, question, preamble),
        }
    }

    fn skills_matching(&self, query: &str) -> String {
        let (offered, errors) = self.rook.skills_offered(query, false);
        let listed: Vec<String> = offered
            .iter()
            .take(10)
            .map(|o| format!("- {}: {}", o.name, o.description.chars().take(160).collect::<String>()))
            .collect();
        if !listed.is_empty() {
            return format!(
                "{}\n\nInstall one by name with `install`, or write your own.",
                listed.join("\n")
            );
        }
        // "No source offers it" and "there are no sources" read the same and
        // are not: the first is an answer, the second is a setting nobody has
        // filled in, and a model told the first goes looking for another name.
        if self.rook.config.skill_sources.is_empty() {
            return format!(
                "there are no skill sources configured, so nothing can be found or installed — \
                 add them under `[skill_sources]` in config.toml, or write the skill with \
                 `{WRITE_SKILL}`."
            );
        }
        match errors.is_empty() {
            true => format!("no source offers a skill matching {query:?}."),
            false => format!("no source offers a skill matching {query:?}. {}", errors.join("; ")),
        }
    }
}
