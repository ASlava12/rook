//! Shared slash commands for the REPL and TUI, local or connected to rookd.
use rook_proto::TurnOptions;

pub(crate) fn configure(command: &str, options: &mut TurnOptions) -> Option<anyhow::Result<String>> {
    let (name, value) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
    if !matches!(
        name,
        "output" | "schema" | "schema-retries" | "recipe" | "attach-image" | "attach-context" | "attachments"
    ) {
        return None;
    }
    Some((|| {
        let value = value.trim();
        match name {
            "attachments" if value == "clear" => options.attachments.clear(),
            "attach-image" | "attach-context" if !value.is_empty() => {
                anyhow::ensure!(
                    options.attachments.len() < rook_core::attachments::MAX_ATTACHMENTS,
                    "at most 4 attachments per turn; /attachments clear removes them"
                );
                options.attachments.push(rook_core::attachments::from_file(
                    std::path::Path::new(value),
                    name == "attach-image",
                )?);
            }
            "recipe" if value == "off" => options.recipe = None,
            "recipe" if !value.is_empty() => {
                let (path, parameters) = value.split_once(char::is_whitespace).unwrap_or((value, "{}"));
                anyhow::ensure!(parameters.len() <= 64 * 1024, "recipe parameters exceed 64 KiB");
                let parameters = serde_json::from_str(parameters)?;
                options.recipe = Some(rook_proto::RecipeInvocation { path: path.into(), parameters });
            }
            "output" if !value.is_empty() => options.output = (value != "off").then(|| value.into()),
            "schema" if value == "off" => options.output_schema = None,
            "schema" if !value.is_empty() => {
                use std::io::Read;
                let mut bytes = Vec::new();
                std::fs::File::open(value)?.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= 64 * 1024, "output schema exceeds 64 KiB");
                options.output_schema = Some(serde_json::from_slice(&bytes)?);
            }
            "schema-retries" if !value.is_empty() => {
                let retries: u8 = value.parse()?;
                anyhow::ensure!(retries <= 3, "schema-retries must be between 0 and 3");
                options.schema_retries = retries;
            }
            _ => {}
        }
        Ok(format!(
            "attachments for next turn: {}; recipe: {}; output: {}; schema: {}; repair attempts: {} (next turns in this window)\n",
            options.attachments.len(),
            options.recipe.as_ref().map(|recipe| recipe.path.as_str()).unwrap_or("off"),
            options.output.as_deref().unwrap_or("off"),
            if options.output_schema.is_some() { "enabled" } else { "off" },
            options.schema_retries
        ))
    })())
}

/// Settings persist, but an attachment belongs only to the next submitted turn.
pub(crate) fn for_turn(options: &mut TurnOptions) -> TurnOptions {
    let submitted = options.clone();
    options.attachments.clear();
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn submitting_attachments_consumes_them_but_keeps_the_output_contract() {
        let mut options = TurnOptions { output: Some("report.md".into()), ..Default::default() };
        options.attachments.push(rook_proto::Attachment::Text { name: "source".into(), text: "data".into() });
        assert_eq!(for_turn(&mut options).attachments.len(), 1);
        assert!(for_turn(&mut options).attachments.is_empty());
        assert_eq!(options.output.as_deref(), Some("report.md"));
    }
}
