use std::sync::Arc;

use async_trait::async_trait;
use rook_tools::ask::{Answer, AskUser, Asker, Question};
use rook_tools::{Tool, ToolContext};

fn question(text: &str, choices: &[&str], multi: bool) -> Question {
    Question { question: text.into(), choices: choices.iter().map(|c| c.to_string()).collect(), multi }
}

#[test]
fn a_number_picks_the_choice_it_names() {
    let q = question("Which target?", &["staging", "prod"], false);
    assert_eq!(q.interpret("2").chosen, ["prod"]);
}

#[test]
fn an_empty_line_takes_the_recommendation() {
    let q = question("Which target?", &["staging", "prod"], false);
    assert_eq!(q.interpret("\n").chosen, ["staging"], "the first choice is the recommended one");
}

#[test]
fn an_empty_line_answers_nothing_when_there_is_no_recommendation() {
    assert!(question("Why?", &[], false).interpret("").chosen.is_empty());
    assert!(question("Which?", &["a", "b"], true).interpret("").chosen.is_empty());
}

#[test]
fn text_that_names_no_choice_is_the_users_own_answer() {
    let q = question("Which target?", &["staging", "prod"], false);
    assert_eq!(
        q.interpret("neither, use the canary").chosen,
        ["neither, use the canary"],
        "typing past the options is the Other row, without one having to be rendered"
    );
}

#[test]
fn a_number_no_choice_has_is_the_users_own_answer_too() {
    let q = question("Which target?", &["staging", "prod"], false);
    assert_eq!(q.interpret("7").chosen, ["7"]);
}

#[test]
fn a_multi_select_takes_every_number_and_a_single_select_takes_one() {
    let choices = ["a", "b", "c"];
    assert_eq!(question("?", &choices, true).interpret("1, 3").chosen, ["a", "c"]);
    assert_eq!(question("?", &choices, false).interpret("1, 3").chosen, ["a"]);
}

#[test]
fn free_text_is_taken_whole_even_when_it_starts_with_a_number() {
    let q = question("How many?", &[], false);
    assert_eq!(q.interpret("3 or 4, I am not sure").chosen, ["3 or 4, I am not sure"]);
}

struct Scripted(Vec<Vec<String>>);

#[async_trait]
impl Asker for Scripted {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        questions
            .iter()
            .zip(&self.0)
            .map(|(q, chosen)| Answer { question: q.question.clone(), chosen: chosen.clone() })
            .collect()
    }
}

fn ask_tool(answers: Vec<Vec<&str>>) -> AskUser {
    let answers = answers.into_iter().map(|a| a.into_iter().map(String::from).collect()).collect();
    AskUser(Arc::new(Scripted(answers)))
}

async fn call(tool: &AskUser, args: serde_json::Value) -> rook_tools::Result<String> {
    let ctx = ToolContext::new(std::env::temp_dir());
    tool.call(&ctx, &args).await.map(|o| o.content)
}

#[tokio::test]
async fn answers_come_back_next_to_the_questions_they_answer() {
    let tool = ask_tool(vec![vec!["prod"], vec!["yes"]]);
    let out = call(
        &tool,
        serde_json::json!({"questions": [
            {"question": "Which target?", "choices": ["staging", "prod"]},
            {"question": "Migrate first?"}
        ]}),
    )
    .await
    .unwrap();

    assert_eq!(out, "Which target?\n  prod\n\nMigrate first?\n  yes");
}

#[tokio::test]
async fn a_skipped_question_says_so_rather_than_reading_as_an_empty_answer() {
    let tool = ask_tool(vec![vec![]]);
    let out = call(&tool, serde_json::json!({"questions": [{"question": "Which target?"}]})).await.unwrap();
    assert_eq!(out, "Which target?\n  (skipped)");
}

#[tokio::test]
async fn asking_nothing_is_an_error_the_model_can_act_on() {
    let tool = ask_tool(vec![]);
    let err = call(&tool, serde_json::json!({"questions": []})).await.unwrap_err().to_string();
    assert!(err.contains("at least one"), "{err}");

    let err = call(&tool, serde_json::json!({"questions": [{"question": "  "}]})).await.unwrap_err();
    assert!(err.to_string().contains("no text"), "{err}");
}

#[tokio::test]
async fn the_wrong_shape_names_the_shape_it_wanted() {
    let tool = ask_tool(vec![vec!["a"]]);
    let err = call(&tool, serde_json::json!({"questions": "Which target?"})).await.unwrap_err().to_string();
    assert!(err.contains("array of {question, choices?, multi?}"), "{err}");
}

#[tokio::test]
async fn more_questions_than_a_form_can_carry_are_cut_rather_than_refused() {
    let tool = ask_tool(vec![vec!["a"]; 6]);
    let many: Vec<_> = (0..6).map(|i| serde_json::json!({"question": format!("q{i}")})).collect();
    let out = call(&tool, serde_json::json!({"questions": many})).await.unwrap();
    assert_eq!(out.matches("q").count(), 4, "asked four of six, and answered those");
}

#[tokio::test]
async fn a_channel_asker_pairs_the_answers_back_onto_their_questions() {
    use rook_tools::ask::{AskRequest, ChannelAsker};

    let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel::<AskRequest>();
    let asker = Arc::new(ChannelAsker::new(tx, std::time::Duration::from_secs(5)));

    let front_end = {
        let asker = asker.clone();
        tokio::spawn(async move {
            let request = requests.recv().await.unwrap();
            asker.answer(&request.id, vec![vec!["prod".into()]]);
        })
    };

    let answers = asker.ask(&[question("Which target?", &["staging", "prod"], false)]).await;
    front_end.await.unwrap();

    assert_eq!(answers[0].question, "Which target?", "the front end sends only what was chosen");
    assert_eq!(answers[0].chosen, ["prod"]);
}

#[tokio::test]
async fn a_front_end_that_never_answers_leaves_the_questions_skipped() {
    use rook_tools::ask::{AskRequest, ChannelAsker};

    let (tx, _requests) = tokio::sync::mpsc::unbounded_channel::<AskRequest>();
    let asker = ChannelAsker::new(tx, std::time::Duration::from_millis(20));

    let answers = asker.ask(&[question("Which target?", &[], false)]).await;

    assert_eq!(answers.len(), 1, "a turn must not be left waiting on a closed tab");
    assert!(answers[0].chosen.is_empty());
    assert_eq!(answers[0].question, "Which target?");
}

#[tokio::test]
async fn short_answers_do_not_shift_onto_the_wrong_questions() {
    use rook_tools::ask::{AskRequest, ChannelAsker};

    let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel::<AskRequest>();
    let asker = Arc::new(ChannelAsker::new(tx, std::time::Duration::from_secs(5)));
    let front_end = {
        let asker = asker.clone();
        tokio::spawn(async move {
            let request = requests.recv().await.unwrap();
            asker.answer(&request.id, vec![vec!["a".into()]]);
        })
    };

    let asked = [question("first", &["a"], false), question("second", &["b"], false)];
    let answers = asker.ask(&asked).await;
    front_end.await.unwrap();

    assert_eq!(answers[0].chosen, ["a"]);
    assert_eq!(answers[1].question, "second");
    assert!(answers[1].chosen.is_empty(), "a missing answer is skipped, not the next one along");
}
