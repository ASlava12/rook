//! Terminal attention signals never enter stdout or a redirected stream.
use std::io::{IsTerminal, Write};

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
