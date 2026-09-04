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
