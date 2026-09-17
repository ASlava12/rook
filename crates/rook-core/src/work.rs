//! One goal, worked at across many turns, with an evaluation between them.
//!
//! A turn ends. Something has to decide whether there is another one, and on
//! what — and if that decision is made by the same model that did the work, the
//! loop is a model marking its own homework for as long as the budget lasts.
//! So the decision is made here, from two things the model does not write: what
//! [`crate::evaluation`] measured, and what the filesystem says changed.
//!
//! The deciding is a function of the iterations so far and nothing else, which
//! is why it is here and not in the front end that drives it. Whether to go
//! again is the part worth testing; rendering a turn is not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::evaluation::Report;

/// Where the agent keeps what it is going to do next.
///
/// Its own file, unlike `.rook/evaluation.toml`, and the difference is the
/// point: the scorecard is how the work is judged and belongs to whoever asked
/// for it; these are the agent's intentions and belong to the agent. A plan is
/// not evidence, so nothing is lost by letting it write this — the checks stay
/// the judge either way.
///
/// It exists because "keep improving it" has no meaning without it. Told the
/// checks pass and asked to carry on, a turn that cannot see what it already
/// tried starts again from the same idea; the paper this follows calls that
/// balancing repairs against new capabilities, and a list of both is the whole
/// of what it needs.
pub fn notes_path(workspace: &Path) -> PathBuf {
    workspace.join(".rook").join("plan.md")
}

/// What the agent has written down, or nothing where it has written nothing.
///
/// Bounded, because it is carried into every iteration and an agent that can
/// append to a file forever eventually spends the whole prompt on it.
pub fn notes(workspace: &Path) -> Option<String> {
    /// A page or so. Long enough for a backlog and what was tried; short
    /// enough that seventy iterations of appending cannot crowd out the work.
    const MOST: usize = 8_000;

    let text = std::fs::read_to_string(notes_path(workspace)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // The end rather than the start: a backlog is appended to, and what was
    // written last is what is current.
    let kept = match text.len() > MOST {
        false => text,
        true => {
            let from = text.len() - MOST;
            let from = (from..text.len()).find(|at| text.is_char_boundary(*at)).unwrap_or(text.len());
            text.get(from..).unwrap_or("")
        }
    };
    Some(kept.to_string())
}

/// Where a run's record is kept, by its id.
pub fn record_key(run: &str) -> String {
    format!("work/run/{run}")
}

/// Where the last run for a workspace is named, so `--resume` has something to
/// look up.
///
/// Keyed by the workspace rather than by the store, because one store holds
/// every project a person works on and resuming the wrong one is worse than
/// not resuming at all.
pub fn last_key(workspace: &Path) -> String {
    format!("work/last/{}", workspace.display())
}

/// The file the agent keeps its own plan in, workspace-relative.
///
/// Named here so [`Iteration::did_nothing`] can tell writing down an intention
/// from carrying one out.
const NOTES: &str = ".rook/plan.md";

/// What was asked for, and what bounds it.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The standing goal, carried into every iteration. Not the prompt — the
    /// prompt changes as the evaluation changes, and this does not.
    pub goal: String,
    /// Iterations at most. Zero is no ceiling, and a run with no ceiling wants
    /// one of the other two.
    pub most: u32,
    /// Tokens at most, across every iteration. Zero is no ceiling.
    ///
    /// The budget that matters over days: a step limit bounds one turn, and
    /// seventy of them bounded only by step limits is seventy times a number
    /// nobody chose with this in mind.
    pub tokens: u64,
    /// Stop as soon as every check passes, rather than carrying on looking for
    /// something else to do.
    ///
    /// On by default in the caller, because "the checks pass" is the ordinary
    /// meaning of done; a run that wants the agent to keep finding work turns
    /// it off and leans on `most` and `tokens` instead.
    pub until_clean: bool,
}

/// One turn and the evaluation that followed it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Iteration {
    /// Counting from one, the way a person says "the third iteration".
    pub at: u32,
    /// The session the turn ran in, so its whole transcript can be read after.
    pub session: String,
    pub reply: String,
    /// What the turn wrote, from the filesystem rather than from its account of
    /// itself.
    pub changed: Vec<String>,
    pub steps: u32,
    pub tokens: u64,
    pub report: Report,
}

impl Iteration {
    /// Whether this iteration left the workspace exactly as it found it.
    ///
    /// Its own plan does not count. An agent with a file it may write and
    /// nothing to do writes the file, and an iteration that rewrote its
    /// intentions and carried none of them out is the shape being watched for
    /// here — counting it as work would let a run spin for as long as the
    /// budget lasted, rewording the same list.
    fn did_nothing(&self) -> bool {
        !self.changed.iter().any(|path| path.replace('\\', "/") != NOTES)
    }
}

/// What happens after an iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Go again, with this as the prompt.
    Again(String),
    /// Stop, for this reason. The reason is the run's verdict and is what a
    /// person reads first.
    Stop(String),
}

/// Two iterations in a row that changed nothing is where a loop stops.
///
/// Not one: a turn that spends its steps reading before it writes is ordinary,
/// and stopping on it would end a run that was about to do the work. Two is a
/// model that has nothing left to try, and every further iteration costs the
/// same and finds the same.
const IDLE_BEFORE_STOPPING: usize = 2;

/// Whether to go again, and on what.
///
/// Reads only what the harness measured. A turn saying it is finished does not
/// end the loop and a turn saying it is stuck does not either — both are the
/// model's account of itself, and the whole point of the evaluation is that
/// there is something else to ask.
pub fn after(plan: &Plan, done: &[Iteration], notes: Option<&str>) -> Next {
    let Some(last) = done.last() else {
        return Next::Again(opening(plan, notes));
    };

    if plan.until_clean && last.report.clean() {
        return Next::Stop(format!(
            "every check passes after {} iteration{}",
            done.len(),
            match done.len() {
                1 => "",
                _ => "s",
            }
        ));
    }
    if plan.most > 0 && done.len() as u32 >= plan.most {
        return Next::Stop(format!(
            "{} iterations, which is the ceiling — {}",
            plan.most,
            last.report.summary()
        ));
    }
    let spent: u64 = done.iter().map(|i| i.tokens).sum();
    if plan.tokens > 0 && spent >= plan.tokens {
        return Next::Stop(format!("{spent} tokens, which is the budget — {}", last.report.summary()));
    }
    // Nothing changed twice running. A third iteration costs what the second
    // did and finds what the second found, and the run's own numbers are the
    // only place this is visible: the model does not report being stuck, it
    // reports having considered the matter.
    let idle = done.iter().rev().take_while(|i| i.did_nothing()).count();
    if idle >= IDLE_BEFORE_STOPPING {
        return Next::Stop(format!(
            "{idle} iterations in a row changed nothing, so this is as far as it goes — {}",
            last.report.summary()
        ));
    }

    Next::Again(again(plan, done, last, notes))
}

/// The first prompt: the goal, and what the checks say before anything is done.
fn opening(plan: &Plan, notes: Option<&str>) -> String {
    let mut said = format!(
        "{}\n\nThis is a standing goal worked at over several turns. What follows each turn is \
         an evaluation run by the harness, not by you — it is declared in \
         `.rook/evaluation.toml` and you cannot run it. Work in small steps that the checks can \
         see.",
        plan.goal.trim()
    );
    said.push_str(&keep_notes(notes));
    said
}

/// What to say about the plan file, which is the same in every iteration.
///
/// Its own paragraph and always present, because an instruction that appears
/// only once is one a fresh session does not have: every iteration is a new
/// session, so anything the agent has to keep doing has to be said every time.
fn keep_notes(notes: Option<&str>) -> String {
    let mut said = format!(
        "\n\nKeep `{NOTES}` current as you go — it is yours, it is the only thing that reaches \
         the next iteration besides the checks, and each iteration starts with no memory of the \
         last. Write what is done, what is worth doing next, and what you tried that did not \
         work, so the next iteration does not try it again."
    );
    match notes {
        Some(written) => said.push_str(&format!("\n\nWhat it says now:\n\n{written}\n")),
        // Said rather than left silent: a model that sees no file assumes
        // there is nowhere to write.
        None => said.push_str("\n\nThere is nothing in it yet."),
    }
    said
}

/// Every prompt after the first.
fn again(plan: &Plan, done: &[Iteration], last: &Iteration, notes: Option<&str>) -> String {
    let mut said = format!("{}\n\n", plan.goal.trim());
    said.push_str(&format!(
        "Iteration {} of this goal. The harness ran the checks after the last one:\n\n{}\n",
        done.len() + 1,
        scorecard(last)
    ));

    // Which way it moved, and said plainly: a model handed only the current
    // state cannot tell a repair from a regression it just caused.
    if let Some(previous) = done.len().checked_sub(2).and_then(|at| done.get(at)) {
        let (before, now) = (previous.report.passed(), last.report.passed());
        match now.cmp(&before) {
            std::cmp::Ordering::Less => said.push_str(&format!(
                "\nThat is worse than the iteration before, which had {before} passing. Something \
                 the last turn did broke it; find that before doing anything else.\n"
            )),
            std::cmp::Ordering::Greater => {
                said.push_str(&format!("\nBetter than the iteration before, which had {before} passing.\n"))
            }
            std::cmp::Ordering::Equal => {}
        }
    }

    // The thing the whole evaluation exists for, said to the model as well as
    // to the person: a check that went green because what it guards was
    // rewritten has not been made to pass.
    let moved: Vec<&str> =
        last.report.checks.iter().filter(|c| !c.touched.is_empty()).map(|c| c.name.as_str()).collect();
    if !moved.is_empty() {
        said.push_str(&format!(
            "\nWhat {} checks was changed in the same iteration, so those results are not \
             evidence yet. If the change to them was right, say why; if it was a way to make the \
             check pass, put it back.\n",
            moved.join(" and ")
        ));
    }
    if last.report.scorecard_changed {
        said.push_str(
            "\n`.rook/evaluation.toml` itself changed during that iteration. It is how the work \
             is judged and is not yours to edit — put it back unless the person asked for it.\n",
        );
    }

    if last.did_nothing() {
        said.push_str(
            "\nThe last iteration changed no files. If there is nothing left worth doing, say so \
             plainly rather than looking again; a run that changes nothing twice is stopped. \
             Rewriting the plan is not doing the work and does not count as a change.\n",
        );
    }
    said.push_str(&keep_notes(notes));
    said
}

/// The checks as a model should read them: verdict, name, and the end of what a
/// failing one printed.
fn scorecard(last: &Iteration) -> String {
    /// Enough of a failure to act on. A test suite prints megabytes and the
    /// part anybody reads is the end; the whole of it here would be the
    /// iteration's context spent on one check.
    const MOST_PER_CHECK: usize = 1_500;

    let mut said = String::new();
    for check in &last.report.checks {
        let mark = match check.passed {
            true => "ok  ",
            false => "FAIL",
        };
        let measured = match check.measured {
            Some(n) => format!(" ({} {n})", check.measures),
            None => String::new(),
        };
        said.push_str(&format!("{mark} {}{measured}\n", check.name));
        if !check.passed {
            let output = check.said.trim_end();
            let kept = match output.len() > MOST_PER_CHECK {
                false => output,
                true => {
                    let from = output.len() - MOST_PER_CHECK;
                    let from =
                        (from..output.len()).find(|at| output.is_char_boundary(*at)).unwrap_or(output.len());
                    output.get(from..).unwrap_or("")
                }
            };
            for line in kept.lines() {
                said.push_str(&format!("     {line}\n"));
            }
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::Scored;

    fn plan() -> Plan {
        Plan { goal: "make the tests pass".into(), most: 10, tokens: 0, until_clean: true }
    }

    fn scored(name: &str, passed: bool) -> Scored {
        Scored {
            name: name.into(),
            passed,
            status: Some(if passed { 0 } else { 1 }),
            took_ms: 1,
            measured: None,
            measures: String::new(),
            said: String::new(),
            touched: Vec::new(),
        }
    }

    fn iteration(at: u32, checks: Vec<Scored>, changed: &[&str]) -> Iteration {
        Iteration {
            at,
            session: format!("{at:032x}"),
            reply: String::new(),
            changed: changed.iter().map(|s| s.to_string()).collect(),
            steps: 5,
            tokens: 1_000,
            report: Report { checks, scorecard_changed: false },
        }
    }

    #[test]
    fn a_run_with_nothing_done_yet_starts_from_the_goal() {
        let Next::Again(prompt) = after(&plan(), &[], None) else { panic!("it has to start") };
        assert!(prompt.contains("make the tests pass"));
        assert!(prompt.contains("not by you"), "and says whose the evaluation is: {prompt}");
    }

    #[test]
    fn a_clean_evaluation_ends_the_run() {
        let done = vec![iteration(1, vec![scored("tests", true)], &["src/lib.rs"])];
        let Next::Stop(why) = after(&plan(), &done, None) else { panic!("clean is done") };
        assert!(why.contains("every check passes"), "{why}");
        assert!(why.contains("1 iteration"), "and says how long it took: {why}");
    }

    /// The thing the evaluation exists for, carried into the decision: a check
    /// that went green because what it guards was rewritten has not passed, and
    /// the loop must not treat it as done.
    #[test]
    fn a_check_that_passed_because_it_was_rewritten_does_not_end_the_run() {
        let mut green = scored("tests", true);
        green.touched = vec!["tests/one.rs".into()];
        let done = vec![iteration(1, vec![green], &["tests/one.rs"])];

        // The precondition: every check passes. Without it this would be
        // asserting that a failing run continues, which is not the claim.
        assert_eq!(done[0].report.passed(), 1, "the check does pass");

        let Next::Again(prompt) = after(&plan(), &done, None) else {
            panic!("a pass that arrived that way is not a pass")
        };
        assert!(prompt.contains("not evidence yet"), "and the model is told why: {prompt}");
        assert!(prompt.contains("put it back"), "{prompt}");
    }

    #[test]
    fn an_edited_scorecard_is_said_to_the_model_as_well_as_to_the_person() {
        let mut done = vec![iteration(1, vec![scored("tests", false)], &["src/lib.rs"])];
        done[0].report.scorecard_changed = true;
        let Next::Again(prompt) = after(&plan(), &done, None) else { panic!("not clean") };
        assert!(prompt.contains("not yours to edit"), "{prompt}");
    }

    /// A model handed only the current state cannot tell a repair from a
    /// regression it has just caused.
    #[test]
    fn a_run_that_went_backwards_says_so_before_anything_else() {
        let done = vec![
            iteration(1, vec![scored("a", true), scored("b", true)], &["src/one.rs"]),
            iteration(2, vec![scored("a", true), scored("b", false)], &["src/two.rs"]),
        ];
        let Next::Again(prompt) = after(&plan(), &done, None) else { panic!("not clean") };
        assert!(prompt.contains("worse than the iteration before"), "{prompt}");
        assert!(prompt.contains("2 passing"), "and what it was: {prompt}");
    }

    #[test]
    fn a_run_that_improved_is_told_so_too() {
        let done = vec![
            iteration(1, vec![scored("a", false), scored("b", false)], &["src/one.rs"]),
            iteration(2, vec![scored("a", true), scored("b", false)], &["src/two.rs"]),
        ];
        let Next::Again(prompt) = after(&plan(), &done, None) else { panic!("not clean") };
        assert!(prompt.contains("Better than the iteration before"), "{prompt}");
    }

    /// A loop that cannot tell it is stuck runs until the budget ends, which
    /// over days is the difference between a cost and a waste.
    #[test]
    fn two_iterations_that_change_nothing_end_the_run() {
        let one = iteration(1, vec![scored("tests", false)], &["src/lib.rs"]);
        let idle = iteration(2, vec![scored("tests", false)], &[]);

        // One idle iteration is not enough: a turn that spends its steps
        // reading before it writes is ordinary, and ending there would stop a
        // run about to do the work.
        let Next::Again(_) = after(&plan(), &[one.clone(), idle.clone()], None) else {
            panic!("one quiet iteration is not stuck")
        };

        let Next::Stop(why) = after(&plan(), &[one, idle.clone(), idle], None) else { panic!("two is") };
        assert!(why.contains("changed nothing"), "{why}");
        assert!(why.contains("0 of 1 checks pass"), "and carries where it got to: {why}");
    }

    #[test]
    fn a_quiet_iteration_tells_the_model_to_say_so_rather_than_look_again() {
        let done = vec![
            iteration(1, vec![scored("tests", false)], &["src/lib.rs"]),
            iteration(2, vec![scored("tests", false)], &[]),
        ];
        let Next::Again(prompt) = after(&plan(), &done, None) else { panic!("one quiet is not stuck") };
        assert!(prompt.contains("changed no files"), "{prompt}");
    }

    #[test]
    fn the_ceiling_on_iterations_is_the_ceiling() {
        let plan = Plan { most: 2, ..plan() };
        let done = vec![
            iteration(1, vec![scored("tests", false)], &["a.rs"]),
            iteration(2, vec![scored("tests", false)], &["b.rs"]),
        ];
        let Next::Stop(why) = after(&plan, &done, None) else { panic!("two of two") };
        assert!(why.contains("ceiling"), "{why}");
    }

    /// The budget that matters over days. A step limit bounds one turn, and
    /// seventy turns bounded only by step limits is seventy times a number
    /// nobody chose with this in mind.
    #[test]
    fn the_token_budget_is_counted_across_iterations_not_within_one() {
        let plan = Plan { most: 0, tokens: 2_500, ..plan() };
        let two = vec![
            iteration(1, vec![scored("tests", false)], &["a.rs"]),
            iteration(2, vec![scored("tests", false)], &["b.rs"]),
        ];
        // Two thousand of a budget of two and a half.
        let Next::Again(_) = after(&plan, &two, None) else { panic!("under budget") };

        let mut three = two;
        three.push(iteration(3, vec![scored("tests", false)], &["c.rs"]));
        let Next::Stop(why) = after(&plan, &three, None) else { panic!("over budget") };
        assert!(why.contains("3000 tokens"), "it says what was spent: {why}");
        assert!(why.contains("budget"), "{why}");
    }

    /// A run told to keep going does not stop because the checks are green;
    /// it stops on the ceilings. Otherwise "carry on improving it" is not
    /// expressible.
    #[test]
    fn a_run_that_was_not_asked_to_stop_when_clean_carries_on() {
        let plan = Plan { until_clean: false, ..plan() };
        let done = vec![iteration(1, vec![scored("tests", true)], &["src/lib.rs"])];
        let Next::Again(_) = after(&plan, &done, None) else { panic!("green is not the end here") };
    }

    /// What a failing check printed reaches the model — that is how it knows
    /// what to fix — and is cut, because a suite that fails everywhere would
    /// otherwise spend the whole iteration's context on one check.
    #[test]
    fn a_failing_checks_output_reaches_the_next_prompt_and_is_bounded() {
        let mut failing = scored("tests", false);
        failing.said = format!("{}\nthe last line that matters", "a line of noise\n".repeat(400));
        let done = vec![iteration(1, vec![failing], &["src/lib.rs"])];

        let Next::Again(prompt) = after(&plan(), &done, None) else { panic!("not clean") };
        assert!(prompt.contains("the last line that matters"), "the end is what is kept");
        assert!(prompt.len() < 4_000, "and it is bounded: {} bytes", prompt.len());
        // The precondition: there really was more than the cap to cut.
        assert!(done_said_more_than(&done), "the check printed more than is kept");
    }

    /// An agent with a file it may write and nothing to do writes the file.
    /// Counting that as work would let a run spin for as long as the budget
    /// lasted, rewording the same list — which is exactly the shape the idle
    /// stop exists to catch.
    #[test]
    fn rewriting_the_plan_is_not_doing_the_work() {
        let one = iteration(1, vec![scored("tests", false)], &["src/lib.rs"]);
        let only_notes = iteration(2, vec![scored("tests", false)], &[".rook/plan.md"]);

        // The precondition: it did change a file, so this is about which file
        // and not about an iteration that wrote nothing at all.
        assert!(!only_notes.changed.is_empty(), "it wrote something");

        let Next::Stop(why) = after(&plan(), &[one, only_notes.clone(), only_notes], None) else {
            panic!("two iterations of nothing but the plan is still two iterations of nothing")
        };
        assert!(why.contains("changed nothing"), "{why}");
    }

    /// Everything else in the workspace does count, or the rule above would
    /// swallow ordinary work that happened to touch the plan as well.
    #[test]
    fn a_plan_written_beside_real_work_is_work() {
        let done = vec![
            iteration(1, vec![scored("tests", false)], &["src/lib.rs", ".rook/plan.md"]),
            iteration(2, vec![scored("tests", false)], &["src/other.rs", ".rook/plan.md"]),
        ];
        let Next::Again(_) = after(&plan(), &done, None) else { panic!("that is two working iterations") };
    }

    /// Each iteration is a new session with no memory of the last, so the one
    /// thing that carries intent between them has to be in every prompt — and
    /// so does the instruction to keep it.
    #[test]
    fn the_plan_reaches_every_iteration_including_the_first() {
        let Next::Again(first) = after(&plan(), &[], Some("- [ ] the thing to do next")) else {
            panic!("it starts")
        };
        assert!(first.contains("the thing to do next"), "the first prompt carries it: {first}");
        assert!(first.contains("Keep `.rook/plan.md` current"), "{first}");

        let done = vec![iteration(1, vec![scored("tests", false)], &["src/lib.rs"])];
        let Next::Again(later) = after(&plan(), &done, Some("- [ ] the thing to do next")) else {
            panic!("not clean")
        };
        assert!(later.contains("the thing to do next"), "and so does every one after: {later}");
        assert!(later.contains("Keep `.rook/plan.md` current"), "{later}");
    }

    /// A model that sees no file assumes there is nowhere to write.
    #[test]
    fn an_empty_plan_is_said_rather_than_left_out() {
        let Next::Again(prompt) = after(&plan(), &[], None) else { panic!("it starts") };
        assert!(prompt.contains("nothing in it yet"), "{prompt}");
    }

    fn done_said_more_than(done: &[Iteration]) -> bool {
        done.iter().any(|i| i.report.checks.iter().any(|c| c.said.len() > 1_500))
    }
}
