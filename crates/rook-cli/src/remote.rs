//! A turn run by `rookd` rather than in this process.
//!
//! The store takes one writer, so a second window cannot run a loop of its own
//! — but the daemon holding it is the same engine, and its chat socket is the
//! same conversation from the other side. This is only the socket: what the
//! events mean to a front end is the front end's business.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use rook_proto::{ChatEvent, ClientMessage};

/// Hold one conversation until the socket closes or the sender is dropped.
///
/// Everything typed goes in through `outgoing` and everything the turn does
/// comes out through `incoming`, so the caller never touches the socket and
/// the drawing loop stays a drawing loop.
pub async fn hold(
    base: &str,
    workspace: &std::path::Path,
    outgoing: &mut mpsc::UnboundedReceiver<ClientMessage>,
    incoming: mpsc::UnboundedSender<ChatEvent>,
) -> Result<()> {
    let url = format!(
        "{}/api/chat?workspace={}",
        base.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1),
        escaped(&workspace.display().to_string())
    );
    // No `Origin`: this is not a browser, and the socket's own guard turns away
    // pages rather than programs — a request without one is curl, an editor, or
    // this.
    let (socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("connecting to the daemon at {url}"))?;
    let (mut write, mut read) = socket.split();

    loop {
        tokio::select! {
            said = outgoing.recv() => match said {
                Some(message) => write.send(Message::text(serde_json::to_string(&message)?)).await?,
                // The window has moved on, and the turn with it.
                None => break,
            },
            heard = read.next() => match heard {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ChatEvent>(&text) {
                    Ok(event) => {
                        if incoming.send(event).is_err() {
                            break;
                        }
                    }
                    // A newer daemon may say things this build has no name for,
                    // and dropping the connection over one of them would lose
                    // the turn. Skipped, and the turn goes on.
                    Err(_) => continue,
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e).context("reading from the daemon"),
            },
        }
    }
    let _ = write.close().await;
    Ok(())
}

/// A query value safe to paste into a url, by the same rule as everywhere else
/// here: one line of it, rather than a crate for ten.
fn escaped(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// What a command shows while the daemon runs a turn, and what it has to
/// answer while it does.
///
/// One of these rather than one per command: `run` and `chat` are the same
/// front end with two entry points, a turn watched from a terminal looks the
/// same whichever started it, and two copies of that drift. What differs is
/// only how the command got here — one line and out, or a prompt that comes
/// back.
pub struct Watching {
    /// What `--yes` decided. The daemon asks the socket rather than the
    /// terminal asking a person, and the rule is the local path's: a command
    /// is scripted more often than watched, so it refuses what it cannot get
    /// approved rather than prompting into a pipe.
    pub yes: bool,
    /// Under `--json` the one output is the object at the end, so the stream a
    /// person would watch would only corrupt it.
    pub json: bool,
    calls: crate::fmt::Calls,
    said: String,
    tools: usize,
    session: String,
}

/// A turn the daemon has finished, as a command reports it.
pub struct Ended {
    pub session: String,
    pub said: String,
    pub tools: usize,
    pub done: ChatEvent,
}

impl Watching {
    pub fn new(yes: bool, json: bool) -> Self {
        Self {
            yes,
            json,
            calls: crate::fmt::Calls::default(),
            said: String::new(),
            tools: 0,
            session: String::new(),
        }
    }

    /// Takes one event. `Some` when the turn is over, and the caller decides
    /// what to do with a turn that has ended — print and leave, or ask for the
    /// next line.
    pub fn saw(
        &mut self,
        event: ChatEvent,
        to_daemon: &mpsc::UnboundedSender<ClientMessage>,
    ) -> Option<Ended> {
        use std::io::Write;
        let mut out = std::io::stdout();
        match event {
            ChatEvent::Started { session } | ChatEvent::Attached { session, .. } => {
                self.session = session;
            }
            ChatEvent::Text { text } => {
                self.said.push_str(&text);
                if !self.json {
                    let _ = write!(out, "{text}");
                    self.calls.said(&text);
                    let _ = out.flush();
                }
            }
            ChatEvent::Tool { name, doing } => {
                self.tools += 1;
                if !self.json {
                    let shown = match doing.is_empty() {
                        true => name.clone(),
                        false => doing,
                    };
                    let _ = write!(out, "{}", self.calls.started(&name, &shown));
                    let _ = out.flush();
                }
            }
            ChatEvent::Approval { id, tool, action, .. } => {
                let decision = match self.yes {
                    true => rook_proto::ApprovalDecision::ForRun,
                    false => {
                        eprintln!(
                            "refused {tool}: {action} — `--yes` allows what the deny list does not forbid"
                        );
                        rook_proto::ApprovalDecision::Deny
                    }
                };
                let _ = to_daemon.send(ClientMessage::Approval { id, decision });
            }
            ChatEvent::Error { message } => {
                eprintln!("{message}");
            }
            done @ ChatEvent::Done { .. } => {
                return Some(Ended {
                    session: std::mem::take(&mut self.session),
                    said: std::mem::take(&mut self.said),
                    tools: std::mem::replace(&mut self.tools, 0),
                    done,
                });
            }
            _ => {}
        }
        None
    }
}
