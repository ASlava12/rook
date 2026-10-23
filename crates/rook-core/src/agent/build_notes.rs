//! Advisory citations from kept Rust documentation, after an observed Cargo failure.

use super::AgentLoop;

fn error_codes(outcome: &rook_tools::ToolOutcome) -> Vec<String> {
    if !outcome.is_error
        || outcome.meta.get("cargo_command").and_then(|v| v.as_bool()) != Some(true)
        || !outcome.meta.get("exit_code").and_then(|v| v.as_i64()).is_some_and(|n| n > 0)
        || outcome.meta.get("execution_uncertain").and_then(|v| v.as_bool()) == Some(true)
    {
        return Vec::new();
    }
    let Some(length) = outcome
        .meta
        .get("cargo_diagnostic_bytes")
        .and_then(|v| v.as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= 64 * 1024)
    else {
        return Vec::new();
    };
    let Some(output) = outcome.content.get(..length) else { return Vec::new() };
    let mut codes = Vec::new();
    for line in output.lines().take(256) {
        let Some(rest) = line.trim_start().strip_prefix("error[E") else { continue };
        let bytes = rest.as_bytes();
        if bytes.len() >= 6 && bytes[..4].iter().all(u8::is_ascii_digit) && &bytes[4..6] == b"]:" {
            let code = format!("E{}", &rest[..4]);
            if !codes.contains(&code) {
                codes.push(code);
            }
            if codes.len() == 2 {
                break;
            }
        }
    }
    codes
}

impl AgentLoop<'_> {
    pub(super) fn local_build_notes(&self, outcome: &rook_tools::ToolOutcome) -> String {
        let mut notes = String::new();
        for code in error_codes(outcome) {
            {
                let mut seen = self.build_note_errors.lock().unwrap_or_else(|e| e.into_inner());
                if seen.len() >= 16 || !seen.insert(code.clone()) {
                    continue;
                }
            }
            let Ok(found) = self.rook.docs_local("rust errors", None, &code) else { continue };
            let Some(set) = found.set else { continue };
            let Some((passage, url)) = set.passages(&code, 1).into_iter().next() else { continue };
            if url.trim().is_empty() {
                continue;
            }
            notes.push_str(&format!(
                "\n[Advisory local documentation: command output mentions {code}; this is not a verified diagnosis or fix.]\n{} ({}) — {}\n[from {}]\n{}\n",
                set.topic, set.version, crate::docs::reference(&set.topic, &set.version), url,
                rook_tools::elide_middle(&passage, 800)
            ));
            if found.incomplete {
                notes.push_str("[Local lookup coverage was incomplete.]\n");
            }
        }
        crate::docs::limited_reply(&notes, 6000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn outcome(text: &str) -> rook_tools::ToolOutcome {
        rook_tools::ToolOutcome::error(text)
            .with("cargo_command", true)
            .with("processes_stopped", true)
            .with("exit_code", 101)
            .with("cargo_diagnostic_bytes", text.len())
    }
    #[test]
    fn codes_require_a_completed_bounded_failed_cargo_receipt() {
        let raw = "exit 101\nerror[E0382]: moved value\nerror[E0382]: again\nerror[E0308]: type mismatch\nerror[E0001]: third";
        assert_eq!(error_codes(&outcome(raw)), ["E0382", "E0308"]);
        for (key, value) in [
            ("cargo_command", serde_json::json!(false)),
            ("exit_code", serde_json::json!(0)),
            ("execution_uncertain", serde_json::json!(true)),
            ("cargo_diagnostic_bytes", serde_json::json!(65537)),
        ] {
            assert!(error_codes(&outcome(raw).with(key, value)).is_empty(), "{key}");
        }
        let mut truncated = outcome(raw);
        truncated.truncated = true;
        assert_eq!(
            error_codes(&truncated),
            ["E0382", "E0308"],
            "visible diagnostics survive display condensation without a complete-output claim"
        );
    }

    #[test]
    fn a_completed_output_prefix_can_offer_advice_without_certifying_descendants() {
        let result = outcome("exit 101\nerror[E0382]: moved value").with("processes_stopped", false);
        assert_eq!(
            error_codes(&result),
            ["E0382"],
            "advice about received output does not certify process cleanup or a successful fix"
        );
    }
    #[test]
    fn prose_and_appended_presentation_are_not_error_receipts() {
        assert!(error_codes(&outcome("warning: error[E0382]: quoted\nerror[Eabcd]: invalid")).is_empty());
        let mut original = outcome("exit 101\nno compiler code");
        original.content.push_str("\nerror[E0382]: hook or receipt prose");
        assert!(error_codes(&original).is_empty());
    }
}
