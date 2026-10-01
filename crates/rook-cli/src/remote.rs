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

use rook_core::delivery;
use rook_proto::{ChatEvent, ClientMessage};

/// The same budget as the daemon, held through terminal event forwarding.
/// Reading configuration here also covers CLI clients which do not open a store.
pub fn channel(workspace: &std::path::Path) -> Result<(delivery::Sender, delivery::Receiver)> {
    let config = rook_core::Config::load_for(workspace)?;
    Ok(delivery::channel(config.server.chat_queue_events, config.server.chat_queue_bytes))
}

/// Hold one conversation until the socket closes or the sender is dropped.
///
/// Everything typed goes in through `outgoing` and everything the turn does
/// comes out through `incoming`, so the caller never touches the socket and
/// the drawing loop stays a drawing loop.
pub async fn hold(
    base: &str,
    workspace: &std::path::Path,
    outgoing: &mut mpsc::UnboundedReceiver<ClientMessage>,
    incoming: delivery::Sender,
) -> Result<()> {
    let url = format!(
        "{}/api/chat?live_snapshots=true&workspace={}",
        base.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1),
        escaped(&workspace.display().to_string())
    );
    // No `Origin`: this is not a browser, and the socket's own guard turns away
    // pages rather than programs — a request without one is curl, an editor, or
    // this.
    let wire = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(incoming.byte_limit()))
        .max_frame_size(Some(incoming.byte_limit()));
    let (socket, _) = tokio_tungstenite::connect_async_with_config(&url, Some(wire), false)
        .await
        .with_context(|| format!("connecting to the daemon at {url}"))?;
    let (mut write, mut read) = socket.split();

    // Reading can wait for the view's leases without preventing Cancel or an
    // answer from reaching the daemon over the independent write half.
    let mut receiver = Receiving(tokio::spawn(async move {
        while let Some(message) = read.next().await {
            match message.context("reading from the daemon")? {
                Message::Text(text) => {
                    if incoming.send_text(text.as_str()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    }));
    let result = loop {
        tokio::select! {
            said = outgoing.recv() => match said {
                Some(message) => {
                    let encoded = match serde_json::to_string(&message) {
                        Ok(encoded) => encoded, Err(error) => break Err(error.into()),
                    };
                    if let Err(error) = write.send(Message::text(encoded)).await { break Err(error.into()); }
                }
                None => break Ok(()),
            },
            heard = &mut receiver.0 => break heard.context("receiving daemon events").and_then(|result| result),
        }
    };
    drop(receiver);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), write.close()).await;
    result
}

// Dropping a TUI connection cancels its reader too, including a reader which
// is waiting for a frame lease while the external editor owns the terminal.
struct Receiving(tokio::task::JoinHandle<Result<()>>);
impl Drop for Receiving {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A query value safe to paste into a url, by the same rule as everywhere else
/// here: one line of it, rather than a crate for ten.
pub(crate) fn escaped(value: &str) -> String {
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
    last_effort: Option<String>,
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
    truncated: bool,
    view_bytes: usize,
}

/// A turn the daemon has finished, as a command reports it.
pub struct Ended {
    pub session: String,
    pub said: String,
    pub tools: usize,
    pub done: ChatEvent,
    pub truncated: bool,
}

impl Watching {
    pub fn new(yes: bool, json: bool, view_bytes: usize) -> Self {
        Self {
            yes,
            json,
            last_effort: None,
            calls: crate::fmt::Calls::default(),
            said: String::new(),
            tools: 0,
            session: String::new(),
            truncated: false,
            view_bytes: view_bytes.clamp(4096, 32 * 1024 * 1024),
        }
    }

    fn retain_text(&mut self, text: &str) {
        // A long turn may stream many intermediate replies before its final
        // answer. Keep a bounded Unicode tail even in non-interactive clients.
        fn boundary(text: &str, mut index: usize) -> usize {
            while !text.is_char_boundary(index) {
                index += 1;
            }
            index
        }
        let start = boundary(text, text.len().saturating_sub(self.view_bytes));
        if start > 0 {
            self.said.clear();
            self.truncated = true;
        }
        let text = &text[start..];
        let remove =
            boundary(&self.said, self.said.len().saturating_add(text.len()).saturating_sub(self.view_bytes));
        if remove > 0 {
            self.said.drain(..remove);
            self.truncated = true;
        }
        self.said.reserve_exact(text.len());
        self.said.push_str(text);
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
            ChatEvent::Snapshot { session, truncated, .. } => {
                self.truncated = truncated;
                self.session = session;
                self.said.clear();
                self.tools = 0;
                self.calls = Default::default();
                self.last_effort = None;
                if !self.json {
                    let note = if truncated {
                        "live view refreshed; saved conversation is in history"
                    } else {
                        "live view refreshed"
                    };
                    let _ = writeln!(out, "\n[{note}]");
                }
            }
            ChatEvent::Started { session } | ChatEvent::Attached { session, .. } => {
                self.session = session;
            }
            ChatEvent::ModelRequest { model, requested_effort, effort } => {
                let report = format!("{model}: effort requested {requested_effort}; {effort}");
                if !self.json && self.last_effort.as_ref() != Some(&report) {
                    let _ = writeln!(out, "\n  {report}");
                    let _ = out.flush();
                }
                self.last_effort = Some(report);
            }
            ChatEvent::Text { text } => {
                self.retain_text(&text);
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
            ChatEvent::FollowUp { id } => {
                if !self.json {
                    let _ = writeln!(out, "\nStarting follow-up {id}");
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
            ChatEvent::Agent { text, receipt: Some(receipt) }
            | ChatEvent::Interjected { text, receipt: Some(receipt) } => {
                if !self.json {
                    let _ = writeln!(
                        out,
                        "\n[{} · r{} · {:?}] {text}",
                        receipt.reference, receipt.revision, receipt.status
                    );
                    let _ = out.flush();
                }
            }
            ChatEvent::Agent { text, receipt: None } => {
                if !self.json {
                    let _ = writeln!(out, "\n{text}");
                    let _ = out.flush();
                }
            }
            ChatEvent::Error { message } => {
                eprintln!("{message}");
            }
            done @ (ChatEvent::Done { .. } | ChatEvent::Failed { .. } | ChatEvent::Cancelled) => {
                if let ChatEvent::Done { reply: Some(reply), .. } = &done {
                    self.said.clear();
                    self.retain_text(reply);
                }
                return Some(Ended {
                    session: std::mem::take(&mut self.session),
                    said: std::mem::take(&mut self.said),
                    tools: std::mem::replace(&mut self.tools, 0),
                    done,
                    truncated: std::mem::take(&mut self.truncated),
                });
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn terminal_failures_end_a_run_but_setting_errors_do_not() {
        let (send, _) = mpsc::unbounded_channel();
        let mut watching = Watching::new(false, true, 4096);
        assert!(watching.saw(ChatEvent::Error { message: "invalid setting".into() }, &send).is_none());
        assert!(watching.saw(ChatEvent::Failed { message: "provider failed".into() }, &send).is_some());
        assert!(watching.saw(ChatEvent::Cancelled, &send).is_some());
    }

    #[test]
    fn a_partial_recovered_view_is_reported_to_json_callers() {
        let (send, _) = mpsc::unbounded_channel();
        let mut watching = Watching::new(false, true, 4096);
        assert!(
            watching
                .saw(
                    ChatEvent::Snapshot {
                        session: "session".into(),
                        running: true,
                        truncated: true,
                        approvals: vec![],
                        questions: vec![]
                    },
                    &send
                )
                .is_none()
        );
        let ended = watching.saw(ChatEvent::Cancelled, &send).unwrap();
        assert!(ended.truncated);
        assert_eq!(ended.session, "session");
    }

    #[tokio::test]
    async fn a_paused_view_does_not_block_cancel_and_dropping_it_closes_its_reader() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sent, both_sent) = tokio::sync::oneshot::channel();
        let (noticed, cancel_seen) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            for _ in 0..2 {
                let value = serde_json::to_string(&ChatEvent::Text { text: "x".repeat(3000) }).unwrap();
                socket.send(Message::Text(value.into())).await.unwrap();
            }
            sent.send(()).unwrap();
            let Message::Text(command) = socket.next().await.unwrap().unwrap() else {
                panic!("expected a command")
            };
            assert!(matches!(
                serde_json::from_str::<ClientMessage>(&command).unwrap(),
                ClientMessage::Cancel
            ));
            noticed.send(()).unwrap();
            let closed = socket.next().await;
            assert!(
                matches!(closed, None | Some(Err(_)) | Some(Ok(Message::Close(_)))),
                "the cancelled reader still owns its socket"
            );
        });
        let (out, mut outgoing) = mpsc::unbounded_channel();
        let (delivery, mut frames) = delivery::channel(1, 4096);
        let task =
            tokio::spawn(
                async move { hold(&base, std::path::Path::new("."), &mut outgoing, delivery).await },
            );
        both_sent.await.unwrap();
        let held_by_view = frames.recv().await.unwrap();
        assert!(
            held_by_view.text.len() * 2 > 4096,
            "two incoming frames exceed both the byte and event caps"
        );
        out.send(ClientMessage::Cancel).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), cancel_seen).await.unwrap().unwrap();
        // Keep the frame and receiver alive: cancellation, not freed capacity,
        // must release the reader which is waiting on its next delivery.
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(30), server).await.unwrap().unwrap();
        drop(held_by_view);
    }

    #[test]
    fn a_long_cli_stream_retains_a_bounded_unicode_tail_and_marks_it_partial() {
        let (send, _) = mpsc::unbounded_channel();
        let mut watching = Watching::new(false, true, 4096);
        let chunk = "🙂".repeat(900);
        assert!(chunk.len() * 2 > 4096);
        for _ in 0..100 {
            watching.saw(ChatEvent::Text { text: chunk.clone() }, &send);
            assert!(watching.said.len() <= 4096);
        }
        assert_eq!(watching.said.len(), 4096, "the retained byte bound was reached");
        assert!(watching.truncated);
        assert_eq!(watching.said.chars().count(), 1024);
    }
}
