//! Asking the person at the terminal.

use std::io::{BufRead, Write};

use async_trait::async_trait;
use rook_tools::ask::{Answer, Question};
use rook_tools::policy::{Approval, Approver, Risk};

pub struct Terminal;

#[async_trait]
impl Approver for Terminal {
    async fn ask(&self, tool: &str, risk: &Risk, preview: Option<&str>) -> Approval {
        let shown = preview.map(|p| format!("\n{}\n", indented(p))).unwrap_or_default();
        let question = format!(
            "\n  {tool} wants to {}\n{shown}  [y]es once · [a]lways this run · [n]o: ",
            risk.describe()
        );
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

#[async_trait]
impl rook_tools::ask::Asker for Terminal {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        let asked = questions.to_vec();
        // stdin is blocking, and blocking it on the runtime's worker would stall
        // every other task in the turn.
        tokio::task::spawn_blocking(move || asked.iter().map(prompt).collect())
            .await
            .unwrap_or_else(|_| questions.iter().map(unanswered).collect())
    }
}

/// Under the question rather than at the left margin, so the diff reads as part
/// of it and not as output the command produced.
fn indented(text: &str) -> String {
    text.lines().map(|l| format!("    {l}\n")).collect()
}

fn unanswered(q: &Question) -> Answer {
    q.unanswered()
}

fn prompt(q: &Question) -> Answer {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "\n  {}", q.question);
    for (i, choice) in q.choices.iter().enumerate() {
        let recommended = if i == 0 && !q.multi { "  (recommended)" } else { "" };
        let _ = writeln!(out, "    {}. {choice}{recommended}", i + 1);
    }
    let _ = write!(out, "  {}", q.ask_line());
    let _ = out.flush();

    let mut answer = String::new();
    match std::io::stdin().lock().read_line(&mut answer) {
        Ok(_) => q.interpret(&answer),
        Err(_) => q.unanswered(),
    }
}
