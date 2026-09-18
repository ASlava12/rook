//! Shared slash commands for the REPL and TUI, local or connected to rookd.
use rook_proto::TurnOptions;

pub(crate) fn configure(command: &str, options: &mut TurnOptions) -> Option<anyhow::Result<String>> {
    let (name, value) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
    if !matches!(name, "output" | "schema" | "schema-retries") {
        return None;
    }
    Some((|| {
        let value = value.trim();
        match name {
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
            "output: {}; schema: {}; repair attempts: {} (next turns in this window)\n",
            options.output.as_deref().unwrap_or("off"),
            if options.output_schema.is_some() { "enabled" } else { "off" },
            options.schema_retries
        ))
    })())
}
