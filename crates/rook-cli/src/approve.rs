//! Asking the person at the terminal.

use std::io::{BufRead, Write};

use async_trait::async_trait;
use rook_tools::policy::{Approval, Approver, Risk};

pub struct Terminal;

#[async_trait]
impl Approver for Terminal {
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval {
        let question =
            format!("\n  {tool} wants to {}\n  [y]es once · [a]lways this run · [n]o: ", risk.describe());
        // stdin is blocking, and blocking it on the runtime's worker would stall
        // every other task in the turn.
        tokio::task::spawn_blocking(move || {
            let mut out = std::io::stdout();
            let _ = write!(out, "{question}");
            let _ = out.flush();
            let mut answer = String::new();
            if std::io::stdin().lock().read_line(&mut answer).is_err() {
                return Approval::Deny("could not read an answer".into());
            }
            match answer.trim().to_lowercase().as_str() {
                "y" | "yes" | "" => Approval::Once,
                "a" | "always" => Approval::ForRun,
                _ => Approval::Deny("the user declined".into()),
            }
        })
        .await
        .unwrap_or_else(|_| Approval::Deny("the prompt failed".into()))
    }
}
