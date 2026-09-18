//! Read-only verification and completion checks with explicit verdicts.

use super::LOAD_SKILL;
use super::compaction::render_span;
use super::effects::CHANGES_FILES;
use super::{AgentLoop, Progress, TurnOutcome, saying_it_waits};
use crate::error::Result;
use rook_llm::Delta;
use rook_store::EventKind;

/// The verdict a checker ended with, if it ended with one of the three.
///
/// Tolerant of how a model dresses the line — bold, a bullet, a different case,
/// a full stop — because the whole mechanism turns on finding it, and a check
/// that ran the build and read the code is not "unchecked" for having written
/// `**VERDICT: holds**`.
///
/// Not tolerant of a fourth word. `VERDICT: probably` is a hedge, and reporting
/// a hedge as a verdict is what asking for one of three exists to prevent.
fn verdict_in(reply: &str) -> Option<&'static str> {
    reply.lines().rev().find_map(verdict_line)
}

fn verdict_line(line: &str) -> Option<&'static str> {
    const DRESSING: [char; 6] = ['*', '_', '#', '-', '>', '`'];
    let line = line.trim().trim_start_matches(|c: char| DRESSING.contains(&c) || c == ' ');
    let (head, rest) = line.split_at_checked("VERDICT:".len())?;
    if !head.eq_ignore_ascii_case("VERDICT:") {
        return None;
    }
    let word = rest
        .trim_start_matches(|c: char| DRESSING.contains(&c) || c == ' ')
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_lowercase();
    ["holds", "fails", "unproven"].into_iter().find(|known| *known == word)
}

/// The reply with its verdict line taken out, for a report that overrules it.
/// A small model reads the last line, and a discounted `holds` left at the
/// bottom of the quotation was read as the answer.
fn without_verdict(reply: &str) -> String {
    let mut lines: Vec<&str> = reply.lines().collect();
    if let Some(at) = lines.iter().rposition(|line| verdict_line(line).is_some()) {
        lines.remove(at);
    }
    lines.join("\n").trim_end().to_string()
}

/// What a checker may spend before it must answer.
///
/// It reads what the turn changed and perhaps runs one thing; it does not do
/// the work again. Left at the turn's own budget it did — for longer than the
/// turn, on a local model.
const CHECKER_STEPS: u32 = 12;

const VERDICT_NUDGE: &str = "\
You stopped without a verdict. If something is still to be run or read, do it now \
with the tools rather than describing it; then end with exactly one of \
`VERDICT: holds`, `VERDICT: fails`, `VERDICT: unproven`.";

const VERDICT_INSTRUCTIONS: &str = "\
You are checking a claim somebody else made. You did not do the work and you have \
no stake in it being true.

Do not take the claim's word for anything, and do not answer from memory: a \
verdict reached without reaching for something is a recollection, and is reported \
as unproven however sure it sounded.

Where something can be run — a build, a test, a linter, a command that prints the \
value in question — run it, and let what it printed be the reason. Where it is \
about this code, read it and quote the lines that decide it. Where it is about \
the world, find where it is said and quote that, with the address it came from; \
if the tools for reaching the web are not here, that is a claim you cannot settle \
and should say so.

Separate what a source states from what it argues. `The figure was 400` is \
something a page asserts and can be attributed; `the figure was disappointing` is \
its writer, and belongs in your answer only as theirs. Two sources that copy one \
another are one source.

You have no tools for writing files: you are judging this, not fixing it.

End with exactly one of these lines, and nothing after it:

VERDICT: holds
VERDICT: fails
VERDICT: unproven

`fails` means you found the claim to be false. A command that would not run \
tells you nothing about the claim and is not that: a missing `Cargo.toml` or an \
argument the tool rejected is `unproven`, and the thing to say is what would \
settle it. `unproven` is the honest answer whenever nothing available settles \
it — say what would. Above that line, give the evidence: the command and its output, the lines \
you read, or the quotation and where it is from. Not a summary of your \
reasoning.";

/// How many of a turn's files the checker is told about by name. Enough to
/// know where to look, and not the forty a refactor touched.
const FILES_NAMED_TO_CHECKER: usize = 10;

impl<'a> AgentLoop<'a> {
    /// Check a claim in a context that did not make it.
    ///
    /// The author is the worst judge of its own work: it knows what it meant,
    /// which is exactly the thing under question. So the checking happens in a
    /// fresh session that is told the claim and nothing about why it should be
    /// believed.
    ///
    /// Two things make this a mechanism rather than an instruction. The checker
    /// is handed a toolbox with the writing tools taken out — it cannot repair
    /// what it was asked to judge, and a verifier that fixes things has stopped
    /// verifying. And it is asked for a verdict in a fixed shape, so "it looks
    /// fine" is a failure to answer rather than an answer.
    ///
    /// It is not isolation: `run_command` can still write, and closing that
    /// needs the sandbox the roadmap describes. What it is is the difference
    /// between a rule the model weighs and a tool it does not have.
    pub(super) async fn verify(
        &self,
        args: &serde_json::Value,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> String {
        let claim = args.get("claim").and_then(|c| c.as_str()).unwrap_or("").trim();
        if claim.is_empty() {
            return "verify needs a claim to check".into();
        }
        let settles = args.get("settles").and_then(|s| s.as_str()).unwrap_or("").trim();
        // Named by what is being checked rather than by the claim: a window
        // showed the first forty-eight characters of the claim beside every
        // call the checker made, and the claim starts with the same sentence
        // every time.
        self.check(claim, settles, outcome, on_progress).await.0
    }

    /// The report and the verdict it carries. The report is what a model reads;
    /// the verdict is what the loop acts on when it asked the question itself.
    async fn check(
        &self,
        claim: &str,
        settles: &str,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> (String, Option<&'static str>) {
        let ceiling = self.rook.config.agent.max_subagents_per_turn;
        let claimed = self.spawned.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |started| (started < ceiling).then_some(started + 1),
        );
        if claimed.is_err() {
            let refused = format!(
                "this turn has already started {ceiling} sub-agents, which is the limit \
                 (`[agent] max_subagents_per_turn`)."
            );
            return (refused, None);
        }

        let mut instruction = format!("{VERDICT_INSTRUCTIONS}\n\nThe claim:\n{claim}");
        if !settles.is_empty() {
            instruction.push_str(&format!("\n\nWhat the author says would settle it:\n{settles}"));
        }

        let (doing, mut steps) = tokio::sync::mpsc::unbounded_channel::<(usize, String)>();
        let running = self.run_checker(&instruction, doing);
        tokio::pin!(running);
        let checked = loop {
            tokio::select! {
                biased;
                Some((at, doing)) = steps.recv() => {
                    on_progress(Progress::Delegating { at, doing: &doing })
                }
                done = &mut running => break done,
            }
        };
        while let Ok((at, doing)) = steps.try_recv() {
            on_progress(Progress::Delegating { at, doing: &doing });
        }

        // Read before the verdict is acted on: what this turn has written, and
        // what it had written the last time this same claim failed. A claim
        // that failed, and holds now that the files under it have been
        // rewritten, is a different claim.
        let wrote_now = self.wrote_paths.lock().map(|w| w.clone()).unwrap_or_default();
        let rewritten_since = self
            .failed_claims
            .lock()
            .ok()
            .and_then(|failed| failed.get(claim).cloned())
            .map(|before| wrote_now.difference(&before).cloned().collect::<Vec<_>>())
            .filter(|since| !since.is_empty())
            .map(|since| since.join(", "));

        match checked {
            Ok((id, child)) => {
                outcome.delegated.push(id.clone());
                outcome.input_tokens += child.input_tokens;
                outcome.output_tokens += child.output_tokens;
                outcome.cached_tokens += child.cached_tokens;
                match verdict_in(&child.reply) {
                    // A verdict from a checker that ran nothing and read nothing
                    // is the model's memory with a label on it, which is exactly
                    // what asking a second agent was supposed to get past. It is
                    // reported as unproven whatever it said.
                    Some(verdict) if verdict != "unproven" && child.tools_called.is_empty() => (
                        format!(
                            "checked in session {id}, which reached for nothing — no command, no file, \
                             no page — so its `{verdict}` is recollection rather than a check:\n{}\n\n\
                             VERDICT: unproven — nothing was run or read to settle it",
                            without_verdict(&child.reply)
                        ),
                        Some("unproven"),
                    ),
                    // "checked by <ULID>" reads as a fact: ids here are the
                    // same shape, and a model twice handed this one to `forget`
                    // before going back to the claim. Naming what the id is
                    // costs a word.
                    // A `fails` says what to report, and a small model read it
                    // as a task: asked to verify that `add` returns the sum, it
                    // rewrote `add` twice until the verdict flipped and then
                    // reported the claim verified. Said on the result rather
                    // than in the tool's description, where it would be paid for
                    // on every request of every turn — the whole advertised list
                    // is 2,500 tokens and has a test holding it there — instead
                    // of in the one turn where a claim has just failed.
                    Some("fails") => {
                        // Remembered against the files as they stand now, so a
                        // later `holds` is compared with what was actually
                        // checked rather than with the start of the turn.
                        if let Ok(mut failed) = self.failed_claims.lock() {
                            failed.insert(claim.to_string(), wrote_now);
                        }
                        (
                            format!(
                                "checked in session {id}:\n{}\n\nThat is the answer to report. \
                                 Editing what was checked until it passes answers a different question.",
                                child.reply
                            ),
                            Some("fails"),
                        )
                    }
                    // The same claim, failing before and holding now, with the
                    // turn having rewritten something in between. Reported as
                    // unproven rather than as a pass, the way a checker that
                    // reached for nothing is: both are a verdict about
                    // something other than the question asked, and in both
                    // cases saying so in prose has already been tried.
                    Some("holds") if rewritten_since.is_some() => {
                        let since = rewritten_since.unwrap_or_default();
                        (
                            format!(
                                "checked in session {id}, and it holds now — but it failed earlier in \
                                 this turn, and {since} changed in between, so what holds is the code \
                                 as rewritten and not the claim that was made about it:\n{}\n\n\
                                 VERDICT: unproven — what was checked changed between the two checks",
                                without_verdict(&child.reply)
                            ),
                            Some("unproven"),
                        )
                    }
                    Some(verdict) => (format!("checked in session {id}:\n{}", child.reply), Some(verdict)),
                    // Not treated as passing: a check that would not commit is
                    // the outcome this exists to make visible.
                    None => (
                        format!(
                            "checked in session {id}, and it did not answer with a verdict:\n{}\n\n\
                             The claim is unchecked — neither held nor failed",
                            child.reply
                        ),
                        None,
                    ),
                }
            }
            Err(e) => (format!("could not check {claim:?}: {e}"), None),
        }
    }

    pub(super) async fn completion_check(
        &self,
        task: &str,
        latest: &str,
        reply: &str,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> std::result::Result<crate::completion::Action, String> {
        if self.overspent(outcome) {
            return Err(self.spend_note());
        }
        if self.out_of_time() {
            return Err(self.time_note());
        }
        let mut request = crate::completion::request(task, latest, reply);
        if self.max_turn_tokens > 0 {
            request.max_output_tokens =
                u64::from(request.max_output_tokens).min(self.left_to_spend(outcome)) as u32;
        }
        let mut patience = std::time::Duration::from_secs(60);
        if let Some(by) = self.by {
            patience = patience.min(by.saturating_duration_since(std::time::Instant::now()));
        }
        let response = saying_it_waits(
            tokio::time::timeout(patience, self.provider.complete(request)),
            patience,
            &mut *on_progress,
        )
        .await
        .map_err(|_| "completion check timed out; task completion is unknown".to_owned())?
        .map_err(|e| format!("completion check failed; task completion is unknown: {e}"))?;
        outcome.input_tokens = outcome.input_tokens.saturating_add(response.usage.input_tokens);
        outcome.output_tokens = outcome.output_tokens.saturating_add(response.usage.output_tokens);
        outcome.cached_tokens = outcome.cached_tokens.saturating_add(response.usage.cache_read_tokens);
        on_progress(Progress::Spent {
            input: outcome.input_tokens,
            output: outcome.output_tokens,
            cached: outcome.cached_tokens,
        });
        self.rook
            .store
            .append_event(
                self.session,
                rook_store::NewEvent::new(
                    EventKind::Note,
                    rook_store::Kind::Message,
                    response.message.content.as_bytes(),
                )
                .label("completion check")
                .usage(response.usage.input_tokens, response.usage.output_tokens),
            )
            .map_err(|e| e.to_string())?;
        crate::completion::verdict(&response).ok_or_else(|| {
            "completion check returned no valid verdict; task completion is unknown".to_owned()
        })
    }

    /// Whether the goal is met, asked of a checker before an autonomous turn
    /// ends — and whether anything the person said not to do was done anyway.
    ///
    /// A turn is the unit because it is the last moment the agent can still
    /// act: told afterwards, it can only apologise. Asked of a checker rather
    /// than of the turn itself, because the author is the worst judge of its
    /// own work.
    pub(super) async fn goal_check(
        &self,
        goal: &str,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> (String, Option<&'static str>) {
        // What it wrote, because a checker looks at the workspace *after* the
        // turn and cannot see what it was like before. One judged a file that
        // now compiles and answered `fails`: the file was fine, so "there was
        // nothing to fix" — the question it settled was whether the task had
        // been worth doing, which is nobody's to reopen. The list is the
        // filesystem's, not the agent's account of itself, so it is evidence
        // of the same kind as the files.
        let wrote =
            self.wrote_paths.lock().map(|w| w.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let mut written = match wrote.len() {
            // "It wrote nothing" was read as "nothing was done": a turn that
            // recorded a skill and said so was checked against an empty
            // workspace, told it had written nothing, believed it, and spent
            // the rest of its steps hunting the filesystem for the skill it
            // had just written. Not everything a turn leaves behind is a file
            // in the workspace, so this says which is which.
            0 => "It wrote no files in the workspace.".to_string(),
            _ => format!(
                "It wrote: {}. That list is the filesystem's rather than the agent's account of \
                 itself — read the files.",
                wrote.iter().take(FILES_NAMED_TO_CHECKER).cloned().collect::<Vec<_>>().join(", ")
            ),
        };
        // The other two things a turn leaves behind, both in the agent's own
        // directory and neither visible to a checker looking at the workspace.
        if !outcome.skills_written.is_empty() {
            written.push_str(&format!(
                " It recorded {} as a skill — skills live in the agent's own directory rather than \
                 in the workspace, and `{LOAD_SKILL}` is what reads one back.",
                outcome.skills_written.join(", ")
            ));
        }
        if !outcome.facts_learned.is_empty() {
            written.push_str(&format!(
                " It remembered: {}. Memory is the agent's own as well, and not on disk here.",
                outcome.facts_learned.join("; ")
            ));
        }
        let written = crate::sources::data("workspace_changes", "recorded paths and memory", &written);
        let claim = format!(
            "The person set this goal for the session, and the agent has just finished a turn \
             towards it:\n\n{goal}\n\nYou are looking at the workspace as it stands after that \
             turn, so what it put right is already in place. {written}\n\nTwo questions, both \
             answered from what is on disk and what runs rather than from the agent's own account: \
             is the goal met now, and was anything the person asked not to do done anyway? \
             `holds` means both are as they should be. `fails` means the goal is not met, or \
             something the person forbade was done — say which, and what would put it right. \
             Whether the task was worth doing is not one of the questions.\n\nSome tasks are \
             answered rather than built. Where the person asked a question, for a check, for a \
             review, the goal is that they were answered truthfully — not that the answer came \
             out one way. A claim the agent was asked to check and found false is the work done, \
             and the claim still being false is not the goal unmet: judge the answering, and \
             treat any change to the thing under question as the thing that would have been \
             forbidden.{}",
            self.what_happened()
        );
        self.check(&claim, "", outcome, on_progress).await
    }

    /// This turn, compressed, for the checker to read beside the workspace.
    ///
    /// It had only the end state, and that is not enough to tell work from a
    /// world bent to fit the claim. The smoke job showed both halves of it in
    /// one run. An agent asked to check a claim was told by `verify` that the
    /// claim was false — and that editing what was checked answers a different
    /// question — then edited the file until it passed, and the goal check read
    /// the mended file and said `holds`. Another agent never answered its
    /// question at all, left no file to look at, and was told the goal was met.
    /// Three failing scenarios, three verdicts of `holds`, two of them word for
    /// word the same sentence.
    ///
    /// What the disk cannot hold is the order things happened in: `verify:
    /// fails` followed by an edit to the file it judged is the whole of that
    /// finding, and it is one line of history. So the history goes in, and the
    /// framing with it — this is what happened, the disk is still what says
    /// whether the goal is met, and a tool's result is the tool's word rather
    /// than the agent's.
    ///
    /// Bounded, and by the same function compaction uses to fit a span into a
    /// request: newest first, and what will not fit is said to have been
    /// elided rather than dropped silently.
    fn what_happened(&self) -> String {
        /// Enough for the shape of a turn — the calls, their arguments, what
        /// came back — and not so much that the check costs more than the turn.
        /// A step is a few hundred tokens, so this is tens of steps.
        const BUDGET_TOKENS: usize = 6_000;
        /// Per entry, before the budget above is applied. A tool result of a
        /// megabyte is a turn nobody can read either.
        const PER_ENTRY_BYTES: usize = 2_000;

        let Ok(entries) = self.rook.transcript(self.session, self.began_at_seq, usize::MAX, PER_ENTRY_BYTES)
        else {
            return String::new();
        };
        // What reaches a model is one question and it has one answer, which
        // this did not ask the first time: the whole transcript went in,
        // bookkeeping and all. A checkpoint carries, under a key called
        // `files`, the store's object id for each path — sixty-four hex digits
        // beside a filename, which is what a content hash looks like and is
        // not one. A checker read the checkpoint taken before a rename, hashed
        // the moved file, found the two did not agree and reported that the
        // contents had been altered; the turn then spent every step it had
        // left hunting a difference that was not there. Measured afterwards:
        // the file was byte-identical throughout, and its plain SHA256 is the
        // one the file has now, not the id the checkpoint recorded.
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|e| rook_store::EventKind::named(&e.kind).is_some_and(crate::context::reaches_the_model))
            .collect();
        let span = render_span(&entries, BUDGET_TOKENS);
        if span.trim().is_empty() {
            return String::new();
        }
        let span = crate::sources::data("transcript", "this turn; recorded evidence", &span);
        format!(
            "\n\nThis is what the turn did, in order. It is evidence of how the workspace came \
             to be as it is, not of whether the goal is met — that is still what is on disk and \
             what runs. Read it for the question the disk cannot answer: whether the agent \
             reached the goal or moved it. A tool's result is that tool's word and not the \
             agent's; the agent's own sentences are its account of itself and carry no weight \
             beyond what they can be checked against.\n\n{span}"
        )
    }

    async fn run_checker(
        &self,
        instruction: &str,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.session, instruction)?;
        let mut child = AgentLoop::new(self.rook, self.provider.clone(), session);
        child.depth = self.depth + 1;
        child.tools = self.tools.without(CHANGES_FILES);
        child.tool_ctx = self.tool_ctx.clone();
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        child.hooks = self.hooks.clone();
        child.servers = self.servers.clone();
        child.spawned = self.spawned.clone();
        // No relay of what the user says mid-turn, unlike a sub-task: a checker
        // is asked to be the one party with no stake in the answer, and a remark
        // from the person whose work is being checked is a stake.
        child.checking = true;
        // Not lowered the way a delegated errand is: an errand is bounded work
        // to get through, and a check is the judgement the parent could not make
        // for itself.
        child.effort = self.effort;
        // Bounded, though, because a check is not the work: a checker with the
        // whole turn's step budget spent twenty-six minutes on a goal check at
        // `high` effort against a local model — longer than the turn it was
        // checking — and ended on a provider timeout with no verdict at all.
        // Enough steps to read a few files and run one command, and no more.
        child.max_steps = self.max_steps.min(CHECKER_STEPS);

        // Cloned out before the closure: a phrase names a path relative to the
        // workspace, and the closure outlives this borrow of `self`.
        let where_it_runs = self.rook.workspace.clone();
        let mut relay = |progress: Progress<'_>| {
            if let Progress::Delta(Delta::ToolCall(call)) = progress {
                let _ =
                    doing.send((0, crate::calls::doing(&call.name, Some(&call.arguments), &where_it_runs)));
            }
        };
        let mut outcome = Box::pin(child.run_with(instruction, &mut relay)).await?;
        // A small model narrates what it would run and stops, or reasons its
        // way to the end and forgets the line. Asked once, in the same session,
        // it usually does what it said; a second silence is reported as one.
        if verdict_in(&outcome.reply).is_none() {
            let finished = Box::pin(child.run_with(VERDICT_NUDGE, &mut relay)).await?;
            outcome.reply = finished.reply;
            outcome.stopped = finished.stopped;
            outcome.steps += finished.steps;
            outcome.input_tokens += finished.input_tokens;
            outcome.output_tokens += finished.output_tokens;
            outcome.cached_tokens += finished.cached_tokens;
            outcome.tools_called.extend(finished.tools_called);
        }
        Ok((rook_store::format_session_id(session), outcome))
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::verdict_in;

    /// The whole mechanism turns on finding this line, and a check that ran the
    /// build is not "unchecked" for having written it in bold.
    #[test]
    fn a_verdict_is_read_however_the_model_dressed_it() {
        assert_eq!(verdict_in("evidence\n\nVERDICT: holds"), Some("holds"));
        assert_eq!(verdict_in("**VERDICT: fails**"), Some("fails"));
        assert_eq!(verdict_in("- verdict: Unproven."), Some("unproven"));
        assert_eq!(verdict_in("> `VERDICT:` holds"), Some("holds"));
        assert_eq!(verdict_in("VERDICT: holds\nVERDICT: fails"), Some("fails"), "the last one is the one");
    }

    /// Asking for one of three is what stops a hedge being reported as a check.
    #[test]
    fn anything_but_the_three_words_is_not_a_verdict() {
        assert_eq!(verdict_in("VERDICT: probably holds"), None);
        assert_eq!(verdict_in("I would say it holds"), None);
        assert_eq!(verdict_in("VERDICT:"), None);
    }
}
