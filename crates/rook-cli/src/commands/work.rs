//! Multi-turn work and evaluation, including interrupted iteration recovery.

use crate::{fmt, workspace_of};
use anyhow::Result;
use std::path::PathBuf;

/// The same rule `last` follows: sessions belong to the workspace they ran in,
/// and a project's list is what you meant. What is hidden is said, so nobody
/// concludes their history is gone.
/// Work at one goal across many turns, evaluating between them.
///
/// The loop is: take a witness of what the checks guard, run a turn, run the
/// checks, record what happened, and ask [`rook_core::work::after`] whether
/// there is another. The deciding is in core and tested there; what is here is
/// the driving.
///
/// One session per iteration rather than one for the run. Seventy turns in one
/// conversation is a context nobody can afford, and the state that has to
/// survive between them is not the conversation — it is the workspace and what
/// the checks said about it, both of which the next prompt carries.
pub(crate) fn cmd_work(
    workspace: Option<PathBuf>,
    goal: String,
    plan: rook_core::work::Plan,
    yes: bool,
    json: bool,
    resume: bool,
) -> Result<()> {
    use rook_core::work::{Iteration, Next};

    let here = workspace_of(&workspace);
    let read_card = || -> Result<rook_core::evaluation::Scorecard> {
        rook_core::evaluation::read(&here).map_err(anyhow::Error::msg)?.ok_or_else(|| anyhow::anyhow!(
            "{} declares no checks. Write [[check]] tables with a name and run command before starting work.",
            rook_core::evaluation::scorecard_path(&here).display()
        ))
    };
    let plan = rook_core::work::Plan { goal, ..plan };

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let rook = rook_core::Rook::open(workspace.clone())?;

    // Resuming reads the last run for this workspace and carries on counting
    // from where it stopped — the ceilings and the budget are the run's, not
    // this invocation's, or a run resumed four times would have four times the
    // budget somebody set once.
    let last = rook.store.kv_get(&rook_core::work::last_key(&here)).ok().flatten();
    let earlier: Vec<Iteration> = match resume {
        false => Vec::new(),
        true => {
            let Some(id) = last.as_deref().map(String::from_utf8_lossy) else {
                anyhow::bail!(
                    "nothing has been run in {} yet, so there is nothing to carry on from.                      `rook work \"<goal>\"` starts one.",
                    here.display()
                );
            };
            let record = rook
                .store
                .kv_get(&rook_core::work::record_key(&id))
                .ok()
                .flatten()
                .and_then(|bytes| serde_json::from_slice::<Vec<Iteration>>(&bytes).ok());
            match record {
                Some(done) => {
                    eprintln!("carrying on {id}, {} iterations in", done.len());
                    done
                }
                None => anyhow::bail!("the record of {id} is not readable, so it cannot be carried on"),
            }
        }
    };
    let run = match (resume, last.as_deref().map(String::from_utf8_lossy)) {
        (true, Some(id)) => id.to_string(),
        _ => rook_store::format_session_id(rook_store::new_session_id()),
    };
    let mut progress = match if resume { rook_core::work::read_state(&rook, &run)? } else { None } {
        Some(saved) => saved,
        None => {
            anyhow::ensure!(
                !plan.goal.is_empty(),
                "this older run has no stored goal; supply it with --resume"
            );
            rook_core::work::RunState { plan: plan.clone(), card: read_card()?, active: None }
        }
    };
    anyhow::ensure!(
        plan.goal.is_empty() || progress.plan.goal == plan.goal,
        "the saved run has a different goal; start a new run to change it"
    );
    if progress.active.as_ref().is_some_and(|active| active.at as usize <= earlier.len()) {
        progress.active = None;
    }
    let plan = progress.plan.clone();
    let card = progress.card.clone();
    if !resume {
        rook.store.kv_set(&rook_core::work::record_key(&run), b"[]")?;
    }
    rook.store.kv_set(&rook_core::work::last_key(&here), run.as_bytes())?;
    rook_core::work::save_state(&rook, &run, &progress)?;

    let ended = runtime.block_on(async {
        // Built once for the whole run, not once per iteration: an MCP server
        // respawned seventy times is seventy handshakes, and the rule about
        // expensive things belonging to the front end is exactly this.
        let mcp = rook.connect_mcp().await;
        for (name, error) in &mcp.failures {
            eprintln!("mcp {name}: {error}");
        }
        let servers = rook_core::agent::servers_for(&rook.config, &rook.workspace);
        let jobs = rook_core::agent::jobs_for(&rook.config);

        let mut done: Vec<Iteration> = earlier;
        let ended = loop {
            // Read fresh each time: the last turn may have rewritten it, and
            // its own plan is the only thing besides the checks that carries
            // from one iteration to the next.
            let notes = rook_core::work::notes(&here);
            let prompt = match rook_core::work::after(&plan, &done, notes.as_deref()) {
                Next::Stop(why) => break why,
                Next::Again(prompt) => prompt,
            };
            let at = done.len() as u32 + 1;
            if !json {
                eprintln!("\n── iteration {at} ──");
            }

            let (session, before) = if let Some(active) = &progress.active {
                let Some(session) = rook_store::parse_session_id(&active.session) else {
                    break "saved work session is invalid".into();
                };
                (session, active.before.clone())
            } else {
                let session = match rook.start_session(&format!("work {at}")) {
                    Ok(session) => session,
                    Err(why) => break format!("could not start iteration {at}: {why}"),
                };
                let before = rook_core::evaluation::witness(&here, &card);
                progress.active = Some(rook_core::work::ActiveIteration {
                    at,
                    session: rook_store::format_session_id(session),
                    before: before.clone(),
                    answer: None,
                    report: None,
                });
                if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                    break format!("could not persist the active iteration: {why}");
                }
                (session, before)
            };
            let recovering = match rook.execution(session) {
                Ok(receipts) => receipts.iter().any(|receipt| !receipt.unknown.is_empty()),
                Err(why) => break format!("could not inspect execution: {why}"),
            };
            if recovering {
                break format!(
                    "iteration {at} has unknown operation results; inspect `rook session recovery {}` before resuming",
                    rook_store::format_session_id(session)
                );
            }
            // Reuse the core's receipt if the caller died before saving its summary.
            if progress.active.as_ref().is_some_and(|active| active.answer.is_none()) {
                match rook.completed_turn(session) {
                    Ok(Some(answer)) => {
                        if let Some(active) = &mut progress.active {
                            active.answer = Some(Ok(answer));
                        }
                        if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                            break format!("could not persist the recovered answer: {why}");
                        }
                    }
                    Ok(None) => {}
                    Err(why) => break format!("could not read the completed turn: {why}"),
                }
            }
            let failed = if let Some(answer) = progress.active.as_ref().and_then(|active| active.answer.clone()) {
                answer
            } else {
                let partial_spend = match rook_core::work::spent(&rook, session) {
                    Ok(tokens) => tokens,
                    Err(why) => break format!("could not read the active iteration's token bill: {why}"),
                };
                let spent = done.iter().fold(partial_spend, |sum, iteration| sum.saturating_add(iteration.tokens));
                if plan.tokens > 0 && spent >= plan.tokens {
                    break format!("the saved run has spent {spent} tokens, reaching its {} token stopping limit", plan.tokens);
                }
                let prompt = if rook.store.get_session(session).ok().flatten().is_some_and(|meta| meta.next_seq > 0) {
                    format!("{prompt}\n\nResume this existing iteration from its recorded history. Inspect completed operations and current files; do not repeat completed actions just because the process restarted.")
                } else { prompt };
                let provider = match rook_core::models::configured(&rook.config) {
                    Ok(provider) => provider,
                    Err(why) => break format!("no model to run on: {why}"),
                };
                let _ = rook.set_goal(session, &plan.goal);
                let mut agent = rook_core::agent::AgentLoop::new(&rook, provider.into(), session);
                if yes {
                    agent.allow_everything_not_denied();
                }
                rook_core::agent::equip(&mut agent, servers.clone(), &mcp, jobs.clone());

                // An iteration that could not run is an iteration that changed
                // nothing, not the end of the run. A tunnel hiccupped mid-stream
                // here — `unexpected EOF during chunk size line` — and a run that
                // had been going for hours ended on it. One transient failure must
                // not throw that away, and a persistent one does not need a rule of
                // its own: two iterations that change nothing already stop a run,
                // and the reason is carried in the record either way.
                // Watched as it goes, the same as a single turn is. Without this an
                // iteration printed its heading and nothing else until it ended,
                // and an iteration is minutes — which leaves a run of days with no
                // way to tell work from a hang, in the one command where that
                // question is asked most.
                let mut watching = crate::fmt::Watching::new(here.clone(), json);
                let failed = match agent.run_with(&prompt, |progress| watching.see(progress)).await {
                    Ok(outcome) => Ok(outcome),
                    Err(why) => {
                        eprintln!("  iteration {at} did not finish: {why}");
                        Err(why.to_string())
                    }
                };
                if let Some(active) = &mut progress.active {
                    active.answer = Some(failed.clone());
                }
                if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                    break format!("could not persist the iteration answer: {why}");
                }
                failed
            };
            let report = if let Some(report) = progress.active.as_ref().and_then(|active| active.report.clone()) {
                report
            } else {
                let report = match rook.evaluate_recorded(session, &card, &before, Some(&jobs)) {
                    Ok(report) => report,
                    Err(why) => {
                        break format!(
                            "evaluation did not finish: {why}; inspect `rook session recovery {}`",
                            rook_store::format_session_id(session)
                        );
                    }
                };
                if let Some(active) = &mut progress.active {
                    active.report = Some(report.clone());
                }
                if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                    break format!("could not persist the evaluation result: {why}");
                }
                report
            };
            let spent = match rook_core::work::spent(&rook, session) {
                Ok(tokens) => tokens,
                Err(why) => break format!("could not total the iteration's token bill: {why}"),
            };
            let outcome = match failed {
                Ok(outcome) => outcome,
                Err(why) => {
                    if let Some(previous) = done.last_mut() {
                        previous.forget_detail();
                    }
                    done.push(Iteration {
                        at,
                        session: rook_store::format_session_id(session),
                        reply: why.clone(),
                        changed: Vec::new(),
                        steps: 0,
                        tokens: spent,
                        report,
                        // So the run's verdict can say which of the two this
                        // was. An iteration that could not reach its model
                        // changes nothing, and so does one with nothing left
                        // to do; only this tells them apart.
                        failed: Some(why),
                    });
                    let persisted = serde_json::to_vec(&done).map_err(|e| e.to_string()).and_then(|text| {
                        rook.store.kv_set(&rook_core::work::record_key(&run), &text).map_err(|e| e.to_string())
                    });
                    if let Err(why) = persisted {
                        break format!("could not persist the iteration: {why}");
                    }
                    progress.active = None;
                    if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                        break format!("could not finish the iteration receipt: {why}");
                    }
                    continue;
                }
            };
            if !json {
                // The reply was written as it arrived, so what is left is where
                // the iteration got to.
                println!();
                eprintln!("  {}", report.summary());
            }

            // Only the newest iteration's detail is ever read again, and the
            // record is rewritten whole after every one — so the one being
            // replaced gives its text up here. Without this a two-hundred
            // iteration run wrote 385 MiB of its own words.
            if let Some(previous) = done.last_mut() {
                previous.forget_detail();
            }
            done.push(Iteration {
                at,
                session: rook_store::format_session_id(session),
                reply: outcome.reply.clone(),
                changed: outcome.files_changed.clone(),
                steps: outcome.steps,
                tokens: spent,
                report,
                failed: None,
            });
            // After every iteration rather than at the end: a run measured in
            // days is one a machine can lose halfway through, and what it has
            // done by then is worth more than the tidiness of writing once.
            let persisted = serde_json::to_vec(&done).map_err(|e| e.to_string()).and_then(|text| {
                rook.store.kv_set(&rook_core::work::record_key(&run), &text).map_err(|e| e.to_string())
            });
            if let Err(why) = persisted {
                break format!("could not persist the iteration: {why}");
            }
            progress.active = None;
            if let Err(why) = rook_core::work::save_state(&rook, &run, &progress) {
                break format!("could not finish the iteration receipt: {why}");
            }
        };
        mcp.shutdown().await;
        (done, ended)
    });

    let (done, why) = ended;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": run,
                "stopped": why,
                "iterations": done,
            }))?
        );
    } else {
        println!(
            "
{why}"
        );
        println!("`rook session show <id>` reads any one of them:");
        for iteration in &done {
            println!("  {} iteration {}", iteration.session, iteration.at);
        }
    }
    // The verdict is the exit status, the way `eval`'s is, so something driving
    // this from a script reads the status rather than the prose.
    match progress.active.is_none() && done.last().is_some_and(|last| last.report.clean()) {
        true => Ok(()),
        false => std::process::exit(1),
    }
}

/// Run the checks this project declares and print what they said.
///
/// Outside any turn and reachable only from here: the model never calls this.
/// An agent that could run its own evaluation could run it until it passed,
/// which is the whole reason the scorecard is declared rather than asked for.
pub(crate) fn cmd_eval(workspace: Option<PathBuf>, json: bool) -> Result<()> {
    let here = workspace_of(&workspace);
    let Some(card) = rook_core::evaluation::read(&here).map_err(anyhow::Error::msg)? else {
        anyhow::bail!(
            "{} declares no checks, so there is nothing to measure. A scorecard is a list of              `[[check]]` tables, each with a `name` and something to `run`.",
            rook_core::evaluation::scorecard_path(&here).display()
        );
    };

    // Taken now and compared with now, so a plain `rook eval` reports no
    // change: what a run of the loop passes in is the witness from before the
    // turn, and that is where "this was rewritten while it was measured" comes
    // from. Asked the same way in both places rather than two functions.
    let before = rook_core::evaluation::witness(&here, &card);
    let report = rook_core::evaluation::run(&here, &card, &before);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = report
        .checks
        .iter()
        .map(|c| {
            vec![
                if c.passed { "✓".into() } else { "✗".into() },
                c.name.clone(),
                match c.status {
                    Some(code) => format!("exit {code}"),
                    None => "did not finish".into(),
                },
                format!("{:.1}s", c.took_ms as f64 / 1000.0),
                match c.measured {
                    Some(n) => format!("{} {n}", c.measures),
                    None => String::new(),
                },
            ]
        })
        .collect();
    print!("{}", fmt::table(&["", "check", "", "took", ""], &rows));
    println!();
    println!("{}", report.summary());
    for check in report.checks.iter().filter(|c| !c.passed) {
        println!("\n── {} ──\n{}", check.name, check.said.trim_end());
    }
    // The exit status is the verdict, the way the gate's is: something that
    // runs this in a loop reads the status, not the table.
    match report.clean() {
        true => Ok(()),
        false => std::process::exit(1),
    }
}
