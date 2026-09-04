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
        // Offered only where there is a family to name, and naming it rather
        // than calling it "this kind": approving `cargo test -p rook-core` for
        // the run leaves `cargo test -p rook-cli` to be answered again, and an
        // afternoon of building is the same question with a new argument.
        let kind = risk.kind().map(|kinds| format!(" · [k] every {}", listed(&kinds))).unwrap_or_default();
        let question = format!(
            "\n  {tool} wants to {}\n{shown}  [y]es once · [a]lways this run{kind} · [n]o: ",
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
                "k" | "kind" => Approval::KindForRun,
                _ => Approval::declined(),
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

/// `cargo`, or `cargo and git` — what the answer would allow, in the words the
/// person would use for it.
pub fn listed(kinds: &[String]) -> String {
    let mut seen: Vec<&String> = Vec::new();
    for kind in kinds {
        if !seen.contains(&kind) {
            seen.push(kind);
        }
    }
    match seen.split_last() {
        None => String::new(),
        Some((last, [])) => format!("`{last}`"),
        Some((last, rest)) => {
            let front: Vec<String> = rest.iter().map(|k| format!("`{k}`")).collect();
            format!("{} and `{last}`", front.join(", "))
        }
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
