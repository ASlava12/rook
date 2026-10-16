//! Terminal attention signals never enter stdout or a redirected stream.
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Idle,
    Working,
    Done,
    Error,
    Permission,
    Question,
    Secret,
    Clear,
}
impl Status {
    fn frame(self) -> &'static [u8] {
        match self {
            Self::Idle => b"\x1b]7501;state=idle:app=rook\x1b\\",
            Self::Working => b"\x1b]7501;state=working:app=rook\x1b\\",
            Self::Done => b"\x1b]7501;state=done:app=rook\x1b\\",
            Self::Error => b"\x1b]7501;state=error:app=rook\x1b\\",
            Self::Permission => b"\x1b]7501;state=blocked:kind=permission:app=rook\x1b\\",
            Self::Question | Self::Secret => b"\x1b]7501;state=blocked:kind=question:app=rook\x1b\\",
            Self::Clear => b"\x1b]7501;state=clear:app=rook\x1b\\",
        }
    }
}
struct Reporter {
    enabled: bool,
    base: Status,
    last: Option<Status>,
    blocked: [usize; 3],
    view: [bool; 3],
    epoch: u64,
}
static REPORTER: Mutex<Reporter> = Mutex::new(Reporter {
    enabled: false,
    base: Status::Idle,
    last: None,
    blocked: [0; 3],
    view: [false; 3],
    epoch: 0,
});
impl Reporter {
    fn effective(&self) -> Status {
        if self.base == Status::Working {
            [Status::Permission, Status::Question, Status::Secret]
                .into_iter()
                .enumerate()
                .rev()
                .find(|(i, _)| self.blocked[*i] > 0 || self.view[*i])
                .map(|(_, state)| state)
                .unwrap_or(self.base)
        } else {
            self.base
        }
    }
    fn emit(&mut self) {
        let status = self.effective();
        if self.enabled && self.last != Some(status) {
            self.last = Some(status);
            let mut out = std::io::stderr().lock();
            let _ = out.write_all(status.frame());
            let _ = out.flush();
        }
    }
}
pub(crate) struct Session;
impl Session {
    pub(crate) fn start(text: bool) -> Self {
        let enabled = text
            && std::io::stdout().is_terminal()
            && std::io::stderr().is_terminal()
            && std::env::var("TERM").is_ok_and(|term| term != "dumb")
            && std::env::var("ROOK_PROGRAM_STATUS")
                .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "on"));
        REPORTER.lock().unwrap_or_else(|e| e.into_inner()).enabled = enabled;
        Self
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        status(Status::Clear);
    }
}
pub(crate) fn status(status: Status) {
    let mut reporter = REPORTER.lock().unwrap_or_else(|e| e.into_inner());
    match status {
        Status::Permission | Status::Question | Status::Secret => {
            reporter.base = Status::Working;
            let index = match status {
                Status::Permission => 0,
                Status::Secret => 2,
                _ => 1,
            };
            reporter.view[index] = true;
        }
        _ => {
            reporter.base = status;
            if status != Status::Working {
                reporter.blocked = [0; 3];
                reporter.view = [false; 3];
                reporter.epoch = reporter.epoch.wrapping_add(1);
            }
        }
    }
    reporter.emit();
}
pub(crate) fn event(event: &rook_proto::ChatEvent) {
    use rook_proto::ChatEvent;
    match event {
        ChatEvent::Inputs { approvals, questions } => {
            inputs(!approvals.is_empty(), !questions.is_empty(), false);
            return;
        }
        ChatEvent::Snapshot { running, approvals, questions, .. } => {
            status(if *running { Status::Working } else { Status::Idle });
            inputs(!approvals.is_empty(), !questions.is_empty(), false);
            return;
        }
        _ => {}
    }
    let next = match event {
        ChatEvent::Started { .. }
        | ChatEvent::Turn { .. }
        | ChatEvent::FollowUp { .. }
        | ChatEvent::ModelRequest { .. }
        | ChatEvent::Text { .. }
        | ChatEvent::Tool { .. } => Status::Working,
        ChatEvent::Ask { .. } => Status::Question,
        ChatEvent::Approval { .. } => Status::Permission,
        ChatEvent::SecretRequest { .. } => Status::Secret,
        ChatEvent::Done { .. } => Status::Done,
        ChatEvent::Failed { .. } | ChatEvent::Error { .. } => Status::Error,
        ChatEvent::Cancelled => Status::Idle,
        _ => return,
    };
    status(next);
}
pub(crate) fn inputs(permission: bool, question: bool, secret: bool) {
    let mut reporter = REPORTER.lock().unwrap_or_else(|e| e.into_inner());
    reporter.view = [permission, question, secret];
    reporter.emit();
}
pub(crate) struct Waiting {
    index: usize,
    epoch: u64,
}
pub(crate) fn waiting(kind: Status) -> Waiting {
    let mut reporter = REPORTER.lock().unwrap_or_else(|e| e.into_inner());
    let index = match kind {
        Status::Permission => 0,
        Status::Secret => 2,
        _ => 1,
    };
    reporter.base = Status::Working;
    reporter.blocked[index] = reporter.blocked[index].saturating_add(1);
    reporter.emit();
    Waiting { index, epoch: reporter.epoch }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        let mut reporter = REPORTER.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch == reporter.epoch {
            reporter.blocked[self.index] = reporter.blocked[self.index].saturating_sub(1);
            reporter.emit();
        }
    }
}

pub(crate) fn attention() {
    let enabled = std::env::var("ROOK_NOTIFY")
        .map_or(true, |value| !matches!(value.to_ascii_lowercase().as_str(), "off" | "0" | "false"));
    if enabled && std::io::stderr().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb") {
        let mut terminal = std::io::stderr().lock();
        let _ = terminal.write_all(b"\x07");
        let _ = terminal.flush();
    }
}

pub(crate) struct OnEnd;

impl Drop for OnEnd {
    fn drop(&mut self) {
        attention();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_progress_does_not_hide_a_pending_question_or_mark_the_root_done() {
        let mut reporter = Reporter {
            enabled: false,
            base: Status::Working,
            last: None,
            blocked: [0, 2, 0],
            view: [false; 3],
            epoch: 1,
        };
        assert_eq!(reporter.effective(), Status::Question);
        reporter.blocked[1] -= 1;
        assert_eq!(reporter.effective(), Status::Question);
        reporter.blocked[1] -= 1;
        assert_eq!(reporter.effective(), Status::Working);
        reporter.view[0] = true;
        assert_eq!(reporter.effective(), Status::Permission);
        reporter.base = Status::Error;
        assert_eq!(reporter.effective(), Status::Error);
        assert_eq!(
            Status::Secret.frame(),
            Status::Question.frame(),
            "no secret name/value enters the terminal frame"
        );
    }
}
