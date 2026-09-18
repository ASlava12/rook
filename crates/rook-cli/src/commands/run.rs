//! Single-turn execution, including piped input and daemon streaming.

use crate::fmt::cached;
use anyhow::{Context, Result, bail};
use rook_core::Rook;
use std::path::PathBuf;

/// The prompt, plus whatever was piped in — `cargo test 2>&1 | rook run "why?"`
/// is how a one-shot turn is usually reached, and dropping the pipe silently
/// answered a question nobody asked.
///
/// Bounded by what the window could ever hold: reading a larger pipe only to be
/// refused for it spends the memory and the time both, and the refusal is more
/// useful than a truncation nobody was told about.
fn with_piped_input(asked: &str, window: usize) -> Result<String> {
    use std::io::IsTerminal;

    let mut piped = String::new();
    if !std::io::stdin().is_terminal() {
        // One byte past the cap, so a pipe exactly at it is not called too big.
        let limit = window.saturating_mul(4);
        piped = read_piped(limit)?;
        if piped.len() > limit {
            bail!(
                "the piped input is larger than the model's {window}-token window. \
                 Write it to a file and ask for that instead — reading a file is paged."
            );
        }
    }
    let piped = piped.trim();
    match (asked.trim(), piped) {
        ("", "") => bail!("nothing to do: pass a prompt, or pipe one in"),
        ("", text) => Ok(text.to_string()),
        (asked, "") => Ok(asked.to_string()),
        (asked, text) => Ok(format!("{asked}\n\n## Piped in\n{text}")),
    }
}

/// Everything on stdin, and a word about it if it takes a moment.
///
/// Reading to end of file is the point — `slow_build | rook run "why?"` has to
/// wait for the build — but an idle pipe never ends, and every supervisor
/// hands a process one: `nohup`, a CI step, a cron wrapper, a shell that
/// backgrounded the command. Then the turn never starts and nothing is
/// printed, ever. Three and a half hours of exactly that on this machine is
/// what this line is for.
fn read_piped(limit: usize) -> Result<String> {
    use std::io::Read;
    /// Long enough that a pipe with something in it never says anything.
    const GRACE: std::time::Duration = std::time::Duration::from_secs(2);

    let (say, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let read = std::io::stdin().take(limit as u64 + 1).read_to_string(&mut text);
        let _ = say.send(read.map(|_| text));
    });
    match heard.recv_timeout(GRACE) {
        Ok(read) => Ok(read?),
        Err(_) => {
            eprintln!(
                "waiting for input on stdin — close it (ctrl-d), or pass `< /dev/null` if \
                 nothing is coming"
            );
            Ok(heard.recv().context("reading stdin")??)
        }
    }
}

pub(crate) fn cmd_run(
    workspace: Option<PathBuf>,
    prompt: Vec<String>,
    session: Option<String>,
    yes: bool,
    json: bool,
    options: rook_proto::TurnOptions,
) -> Result<()> {
    let _attention = crate::notify::OnEnd;
    let asked = if prompt.is_empty() && (options.recipe.is_some() || !options.attachments.is_empty()) {
        "Process the selected recipe or attachments".to_owned()
    } else {
        prompt.join(" ")
    };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    // The exit code is decided inside and taken here, after the store has been
    // dropped: exiting from within would skip closing it cleanly.
    let asked_for_a_workspace = workspace.is_some();
    // Both of these outside the runtime, and that is not tidiness. `Rook::open`
    // is synchronous and `Daemon::running` blocks for its answer, and blocking
    // a thread that is driving the runtime it blocks on is a panic rather than
    // a wait — which this found twice in one day.
    //
    // The store takes one writer, and where `rookd` holds it the daemon is the
    // same engine: its chat socket is this conversation from the other side. So
    // the turn goes there rather than failing, which is what it used to do,
    // with advice the person had already taken — "start rookd before them",
    // said to somebody whose rookd was running.
    let opened = Rook::open(workspace.clone());
    if let Err(locked) = &opened
        && crate::source::is_locked(locked)
        && let Some(daemon) = crate::source::Daemon::running()
    {
        let here = crate::source::asked_about(workspace);
        let elsewhere =
            runtime.block_on(through_the_daemon(&daemon, &here, &asked, session, yes, json, options))?;
        if elsewhere {
            crate::notify::attention();
            std::process::exit(2);
        }
        return Ok(());
    }
    let rook = opened?;
    let unfinished = runtime.block_on(async move {
        let provider = rook_core::models::configured(&rook.config)
            .with_context(|| format!("configuring model {:?}", rook.config.agent.model))?;
        let prompt = with_piped_input(&asked, provider.context_window())?;
        let session = match session {
            Some(s) => rook.session_named(&s)?,
            None => rook.start_session("")?,
        };
        // `-C` is the user deciding; without it, the session decides.
        let elsewhere = if asked_for_a_workspace { None } else { rook.following(session)? };
        let rook = match elsewhere {
            Some(theirs) => {
                eprintln!("continuing in {}, where this session belongs", theirs.workspace.display());
                theirs
            }
            None => rook,
        };
        let mut agent = rook_core::agent::AgentLoop::new(&rook, provider.into(), session);
        agent.options = options;
        // `run` is scripted more often than watched, so it refuses what it cannot
        // get approved rather than prompting into a pipe.
        if yes {
            agent.allow_everything_not_denied();
        }
        let mcp = rook.connect_mcp().await;
        rook_core::agent::equip(
            &mut agent,
            rook_core::agent::servers_for(&rook.config, &rook.workspace),
            &mcp,
            rook_core::agent::jobs_for(&rook.config),
        );
        for (name, error) in &mcp.failures {
            eprintln!("mcp {name}: {error}");
        }
        // Before the loop borrows the agent, for the phrase a call is named by.
        let mut watching = crate::fmt::Watching::new(rook.workspace.clone(), json);
        let outcome = agent.run_with(&prompt, |progress| watching.see(progress)).await?;
        mcp.shutdown().await;
        let changes = rook.changes(session, false).ok();
        // A script that pipes this into something else has to be able to tell a
        // finished turn from one that ran out of steps or was refused, and both
        // of those come back as Ok with the work half done.
        let finished = rook_core::agent::finished(&outcome.stopped);
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "session": rook_store::format_session_id(session),
                    "outcome": outcome,
                    "changes": changes,
                }))?
            );
            return anyhow::Ok(unfinished(finished, &outcome.stopped));
        }
        println!();
        for id in &outcome.delegated {
            eprintln!("sub-agent {id} — `rook session show {id}` for its detail");
        }
        if let Some(note) = outcome.changed_note() {
            eprintln!("{note}");
        }
        if let Some(note) = outcome.memory_note() {
            eprintln!("{note}");
        }
        for text in &outcome.decisions {
            eprintln!("decided: {text}");
        }
        for text in &outcome.open_questions {
            eprintln!("open question: {text}");
        }
        if let Some(changes) = changes.filter(|c| c.touched() > 0) {
            eprintln!(
                "{} — `rook session diff {}`",
                changes.summary(),
                rook_store::format_session_id(session)
            );
        }
        eprintln!(
            "\n[session {} · {} steps · {} in / {} out tokens{} · {} tool calls{}]",
            rook_store::format_session_id(session),
            outcome.steps,
            outcome.input_tokens,
            outcome.output_tokens,
            cached(outcome.cached_tokens),
            outcome.tools_called.len(),
            if outcome.compactions > 0 {
                format!(" · {} compactions", outcome.compactions)
            } else {
                String::new()
            }
        );
        anyhow::Ok(unfinished(finished, &outcome.stopped))
    })?;
    if unfinished {
        crate::notify::attention();
        std::process::exit(2);
    }
    Ok(())
}

/// Whether the caller should hear that the work was not done. stdout is the
/// machine channel — under `--json` the object already carries `stopped` — so
/// the sentence goes to stderr either way.
/// One turn, run by the daemon and printed here.
///
/// What it shows and what it answers is [`crate::remote::Watching`], shared
/// with the REPL: a turn watched from a terminal looks the same whichever
/// command started it.
///
/// Takes the runtime rather than running inside one, which running it found:
/// the daemon client blocks for its answer, and blocking a thread that is
/// driving the runtime it blocks on is a panic rather than a wait. So the
/// choice between the two is made out here, and only the local half is given
/// to the runtime.
async fn through_the_daemon(
    daemon: &crate::source::Daemon,
    workspace: &std::path::Path,
    asked: &str,
    session: Option<String>,
    yes: bool,
    json: bool,
    options: rook_proto::TurnOptions,
) -> Result<bool> {
    use rook_proto::{ChatEvent, ClientMessage};

    eprintln!("using the running rookd at {}", daemon.base);
    let (to_daemon, mut outgoing) = tokio::sync::mpsc::unbounded_channel();
    let (incoming, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (base, here) = (daemon.base.clone(), workspace.to_path_buf());
    let socket =
        tokio::spawn(async move { crate::remote::hold(&base, &here, &mut outgoing, incoming).await });
    to_daemon.send(ClientMessage::Prompt { session, text: asked.to_string(), options })?;

    let mut watching = crate::remote::Watching::new(yes, json);
    let mut ended = None;
    while let Some(event) = events.recv().await {
        if let Some(over) = watching.saw(event, &to_daemon) {
            ended = Some(over);
            break;
        }
    }
    // Dropping it closes the socket, which is what tells the daemon this
    // connection has gone. The turn is the daemon's and outlives it either way.
    drop(to_daemon);
    let _ = socket.await;

    let Some(over) = ended else {
        anyhow::bail!("the daemon closed the connection before the turn finished");
    };
    match &over.done {
        ChatEvent::Failed { message } => anyhow::bail!("{message}"),
        ChatEvent::Cancelled => anyhow::bail!("the turn was cancelled"),
        _ => {}
    }
    let ChatEvent::Done {
        reply: _,
        steps,
        input_tokens,
        output_tokens,
        delegated,
        compactions,
        decisions,
        open_questions,
        files_changed,
        stopped,
    } = over.done
    else {
        anyhow::bail!("a turn ends with `done` and nothing else");
    };
    let (started, said, tools) = (over.session, over.said, over.tools);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session": started,
                "reply": said,
                "steps": steps,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "delegated": delegated,
                "compactions": compactions,
                "decisions": decisions,
                "open_questions": open_questions,
                "files_changed": files_changed,
                "stopped": stopped,
            }))?
        );
        return Ok(unfinished(rook_core::agent::finished(&stopped), &stopped));
    }
    println!();
    for text in &decisions {
        eprintln!("decided: {text}");
    }
    for text in &open_questions {
        eprintln!("open question: {text}");
    }
    if !files_changed.is_empty() {
        eprintln!("{} files changed — `rook session diff {started}`", files_changed.len());
    }
    eprintln!(
        "\n[session {started} · {steps} steps · {input_tokens} in / {output_tokens} out tokens · \
         {tools} tool calls{}]",
        match compactions {
            0 => String::new(),
            n => format!(" · {n} compactions"),
        }
    );
    Ok(unfinished(rook_core::agent::finished(&stopped), &stopped))
}

fn unfinished(finished: bool, stopped: &str) -> bool {
    if finished {
        return false;
    }
    if let Some(why) = rook_core::agent::why_it_stopped(stopped) {
        eprintln!("{why}");
    }
    true
}
