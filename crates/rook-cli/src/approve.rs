//! Asking the person at the terminal.

use std::io::{BufRead, Write};

use async_trait::async_trait;
use rook_tools::ask::{Answer, Question};
use rook_tools::policy::{Approval, Approver, Risk};

pub struct Terminal;

#[async_trait]
impl Approver for Terminal {
    async fn ask(&self, tool: &str, risk: &Risk, preview: Option<&str>) -> Approval {
        crate::notify::attention();
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
            read_approval(&mut std::io::stdin().lock())
        })
        .await
        .unwrap_or_else(|_| Approval::Deny("the prompt failed".into()))
    }
}

#[async_trait]
impl rook_tools::ask::Asker for Terminal {
    async fn ask_ephemeral(&self, questions: &[Question]) -> Vec<Answer> {
        let mut answers = Vec::new();
        for question in questions {
            answers.push(crate::private_input::read_question(question.clone()).await);
        }
        answers
    }
    fn supports_private_input(&self) -> bool {
        crate::private_input::available()
    }

    async fn request_secret(
        &self,
        prompt: rook_proto::SecretPrompt,
        sink: std::sync::Arc<dyn rook_tools::private_input::SecretSink>,
    ) -> rook_proto::SecretStatus {
        let (draft, status) = crate::private_input::read(prompt).await;
        let status = match draft.value(status) {
            Some(value) => sink.save(value),
            None => status,
        };
        sink.close();
        status
    }

    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        crate::notify::attention();
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

    read_answer(q, &mut std::io::stdin().lock())
}

fn read_approval(input: &mut impl BufRead) -> Approval {
    let mut answer = String::new();
    match input.read_line(&mut answer) {
        Ok(0) => return Approval::Unanswered("terminal input ended without an answer".into()),
        Err(_) => return Approval::Unanswered("could not read an answer".into()),
        Ok(_) => {}
    }
    match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => Approval::Once,
        "a" | "always" => Approval::ForRun,
        "k" | "kind" => Approval::KindForRun,
        _ => Approval::declined(),
    }
}

fn read_answer(q: &Question, input: &mut impl BufRead) -> Answer {
    let mut answer = String::new();
    match input.read_line(&mut answer) {
        Ok(0) | Err(_) => q.unanswered(),
        Ok(_) => q.interpret(&answer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn eof_and_a_blank_approval_never_authorize_an_action() {
        assert!(matches!(read_approval(&mut Cursor::new(b"")), Approval::Unanswered(_)));
        assert!(matches!(read_approval(&mut Cursor::new(b"\n")), Approval::Deny(_)));
        assert!(matches!(read_approval(&mut Cursor::new(b"no\n")), Approval::Deny(_)));
        assert!(matches!(read_approval(&mut Cursor::new(b"yes\n")), Approval::Once));
    }

    #[test]
    fn eof_is_an_unanswered_question_even_when_a_recommendation_exists() {
        let question =
            Question { question: "Pick a target".into(), choices: vec!["production".into()], multi: false };
        assert!(read_answer(&question, &mut Cursor::new(b"")).chosen.is_empty());
        assert!(read_answer(&question, &mut Cursor::new(b"/skip\n")).chosen.is_empty());
        assert_eq!(read_answer(&question, &mut Cursor::new(b"1\n")).chosen, vec!["production"]);
    }
}
