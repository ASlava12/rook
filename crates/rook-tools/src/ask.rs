//! Asking the user a question the agent cannot answer for them.
//!
//! Separate from [`crate::policy`], which asks whether an action is allowed.
//! This asks which action to take, and only the person can answer.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{Result, Tool, ToolContext, ToolOutcome, policy::Risk};
use rook_llm::ToolSpec;

/// More than a handful on one form and people stop reading them.
const MAX_QUESTIONS: usize = 4;
const MAX_CHOICES: usize = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    /// Empty means free text.
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub multi: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    /// Echoed back so the model needs no question ids to match answers up.
    pub question: String,
    /// One entry for free text or a single choice; more for a multi-select.
    /// Empty means the user skipped it.
    pub chosen: Vec<String>,
}

impl Question {
    /// One line of typed text, read as an answer to this question.
    ///
    /// Numbers pick choices; anything that names no choice is taken verbatim,
    /// which is the "Other" row every other agent has to render explicitly. An
    /// empty line takes the recommendation, because the first choice is the
    /// recommendation and a single-select has one.
    pub fn interpret(&self, line: &str) -> Answer {
        let answer = |chosen| Answer { question: self.question.clone(), chosen };
        let line = line.trim();
        if line.is_empty() {
            return match (self.multi, self.choices.first()) {
                (false, Some(first)) => answer(vec![first.clone()]),
                _ => answer(Vec::new()),
            };
        }
        if self.choices.is_empty() {
            return answer(vec![line.to_string()]);
        }
        let picked: Vec<String> = line
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter_map(|token| token.parse::<usize>().ok())
            .filter_map(|n| self.choices.get(n.wrapping_sub(1)).cloned())
            .collect();
        match picked.as_slice() {
            [] => answer(vec![line.to_string()]),
            _ if self.multi => answer(picked),
            [first, ..] => answer(vec![first.clone()]),
        }
    }

    /// What to print after the choices, if any.
    pub fn ask_line(&self) -> &'static str {
        match (self.choices.is_empty(), self.multi) {
            (true, _) => "your answer: ",
            (false, true) => "numbers, comma-separated, or your own answer: ",
            (false, false) => "a number, or your own answer: ",
        }
    }

    pub fn unanswered(&self) -> Answer {
        Answer { question: self.question.clone(), chosen: Vec::new() }
    }
}

#[async_trait]
pub trait Asker: Send + Sync {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer>;
}

/// Registered only when a front end can actually reach a person, so an
/// unattended run does not advertise a tool that would hang or lie.
pub struct AskUser(pub Arc<dyn Asker>);

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask".into(),
            description: format!(
                "Ask the user when the decision is theirs and guessing wastes real work. \
                 Independent questions go in one call (1-{MAX_QUESTIONS}); ones whose answers \
                 depend on each other go in separate calls. Options belong in `choices` (up to \
                 {MAX_CHOICES}, recommended first), never in the question text where the user \
                 cannot pick them. Decide low-stakes questions yourself."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string" },
                                "choices": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "maxItems": MAX_CHOICES
                                },
                                "multi": { "type": "boolean" }
                            },
                            "required": ["question"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        }
    }

    fn risk(&self, _args: &serde_json::Value) -> Risk {
        Risk::ReadOnly
    }

    async fn call(&self, _ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let questions = parse(args)?;
        let answers = self.0.ask(&questions).await;
        Ok(ToolOutcome::ok(render(&answers)).with("questions", questions.len() as i64))
    }
}

fn parse(args: &serde_json::Value) -> Result<Vec<Question>> {
    let invalid = |message: &str| crate::ToolError::Invalid { tool: "ask".into(), message: message.into() };

    let mut questions: Vec<Question> =
        serde_json::from_value(args.get("questions").cloned().unwrap_or_default()).map_err(|e| {
            invalid(&format!("`questions` must be an array of {{question, choices?, multi?}}: {e}"))
        })?;

    if questions.is_empty() {
        return Err(invalid("nothing to ask — `questions` needs at least one entry"));
    }
    questions.truncate(MAX_QUESTIONS);
    for q in &mut questions {
        if q.question.trim().is_empty() {
            return Err(invalid("a question with no text cannot be answered"));
        }
        q.choices.truncate(MAX_CHOICES);
    }
    Ok(questions)
}

fn render(answers: &[Answer]) -> String {
    if answers.is_empty() {
        return "The user answered nothing.".into();
    }
    answers
        .iter()
        .map(|a| match a.chosen.as_slice() {
            [] => format!("{}\n  (skipped)", a.question),
            chosen => format!("{}\n  {}", a.question, chosen.join("\n  ")),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What a front end is being asked to put to the user.
#[derive(Clone, Debug)]
pub struct AskRequest {
    pub id: String,
    pub questions: Vec<Question>,
}

/// Hands the questions to whatever is driving the UI and waits for the answers
/// to come back by id. Used by every front end that has one.
pub struct ChannelAsker(crate::pending::Pending<AskRequest, Vec<Vec<String>>>);

impl ChannelAsker {
    pub fn new(
        requests: tokio::sync::mpsc::UnboundedSender<AskRequest>,
        patience: std::time::Duration,
    ) -> Self {
        Self(crate::pending::Pending::new(requests, patience))
    }

    /// One entry per question, in the order they were asked; an empty one is a
    /// question the user skipped. The front end sends only what was chosen —
    /// pairing it back to the question text happens here, so a UI cannot get it
    /// wrong.
    pub fn answer(&self, id: &str, chosen: Vec<Vec<String>>) {
        self.0.answer(id, chosen);
    }
}

#[async_trait]
impl Asker for ChannelAsker {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        let request = |id| AskRequest { id, questions: questions.to_vec() };
        // A question nobody answered is one the model must decide for itself,
        // which is exactly what a skipped answer says.
        let chosen = self.0.ask(request).await.unwrap_or_default();
        questions
            .iter()
            .enumerate()
            .map(|(i, q)| Answer {
                question: q.question.clone(),
                chosen: chosen.get(i).cloned().unwrap_or_default(),
            })
            .collect()
    }
}

/// The answer for a front end that cannot reach anyone. Says what to do instead
/// rather than leaving the model to guess why its question went nowhere.
pub struct NoOne;

#[async_trait]
impl Asker for NoOne {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        questions.iter().map(Question::unanswered).collect()
    }
}
