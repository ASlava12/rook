//! Turn configuration shared by CLI arguments and REPL/TUI slash commands.
use anyhow::Result;
use rook_proto::TurnOptions;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub(crate) struct TurnArgs {
    /// Attach a local PNG, JPEG, WebP or GIF (repeatable, up to 2 MiB each).
    #[arg(long = "image")]
    images: Vec<PathBuf>,
    /// Embed a UTF-8 file as untrusted source context (repeatable).
    #[arg(long = "context")]
    contexts: Vec<PathBuf>,
    /// Run .rook/recipes/<name>.toml, or a workspace-relative recipe file.
    #[arg(long)]
    recipe: Option<String>,
    /// A recipe parameter, as NAME=VALUE. May be repeated.
    #[arg(long = "param", requires = "recipe")]
    parameters: Vec<String>,
    /// Save the final answer atomically inside the session workspace.
    #[arg(long)]
    output: Option<String>,
    /// Validate the answer against this local JSON Schema file.
    #[arg(long)]
    output_schema: Option<PathBuf>,
    /// Format-only correction attempts, without tools (0..3).
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=3))]
    schema_retries: u8,
}

impl TurnArgs {
    pub(crate) fn load(self) -> Result<rook_proto::TurnOptions> {
        let schema = self.output_schema.as_deref().map(read_schema).transpose()?;
        let recipe = self
            .recipe
            .map(|path| -> Result<rook_proto::RecipeInvocation> {
                let mut parameters = std::collections::BTreeMap::new();
                for parameter in self.parameters {
                    let (name, value) =
                        parameter.split_once('=').ok_or_else(|| anyhow::anyhow!("use --param NAME=VALUE"))?;
                    anyhow::ensure!(
                        parameters.insert(name.to_owned(), value.to_owned()).is_none(),
                        "duplicate recipe parameter {name:?}"
                    );
                }
                Ok(rook_proto::RecipeInvocation { path, parameters })
            })
            .transpose()?;
        anyhow::ensure!(
            self.images.len() + self.contexts.len() <= rook_core::attachments::MAX_ATTACHMENTS,
            "at most 4 attachments per turn"
        );
        let attachments = self
            .images
            .iter()
            .map(|p| (p, true))
            .chain(self.contexts.iter().map(|p| (p, false)))
            .map(|(p, image)| rook_core::attachments::from_file(p, image))
            .collect::<rook_core::Result<Vec<_>>>()?;
        Ok(rook_proto::TurnOptions {
            attachments,
            output: self.output,
            output_schema: schema,
            schema_retries: self.schema_retries,
            recipe,
        })
    }
}

/// Both entry paths enforce the same bound while reading, before parsing JSON.
fn read_schema(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_SCHEMA_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAX_SCHEMA_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= MAX_SCHEMA_BYTES as usize, "output schema exceeds 64 KiB");
    Ok(serde_json::from_slice(&bytes)?)
}

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
                options.output_schema = Some(read_schema(Path::new(value))?);
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

    fn arguments(path: &Path) -> TurnArgs {
        TurnArgs {
            images: Vec::new(),
            contexts: Vec::new(),
            recipe: None,
            parameters: Vec::new(),
            output: None,
            output_schema: Some(path.to_owned()),
            schema_retries: 2,
        }
    }

    #[test]
    fn cli_and_interactive_schema_loading_agree_at_the_size_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schema with spaces.json");
        let schema = serde_json::json!({"type": "object", "required": ["findings"]});
        let mut bytes = serde_json::to_vec(&schema).unwrap();
        bytes.resize(64 * 1024, b' ');
        std::fs::write(&path, &bytes).unwrap();

        let cli = arguments(&path).load().unwrap();
        let mut interactive = TurnOptions::default();
        configure(&format!("schema {}", path.display()), &mut interactive).unwrap().unwrap();
        assert_eq!(cli.output_schema, Some(schema));
        assert_eq!(cli.output_schema, interactive.output_schema);
    }

    #[test]
    fn rejected_schemas_leave_the_interactive_contract_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schema.json");
        let previous = serde_json::json!({"type": "object"});
        let mut interactive = TurnOptions { output_schema: Some(previous.clone()), ..Default::default() };
        let mut oversized = b"{}".to_vec();
        oversized.resize(64 * 1024 + 1, b' ');
        assert!(oversized.len() > 64 * 1024);
        // The oversized input is valid JSON, so its rejection proves the bound.
        assert!(serde_json::from_slice::<serde_json::Value>(&oversized).is_ok());
        for bytes in [oversized, b"{broken".to_vec()] {
            std::fs::write(&path, bytes).unwrap();
            let cli_error = arguments(&path).load().unwrap_err().to_string();
            let interactive_error = configure(&format!("schema {}", path.display()), &mut interactive)
                .unwrap()
                .unwrap_err()
                .to_string();
            assert_eq!(cli_error, interactive_error);
            assert_eq!(interactive.output_schema, Some(previous.clone()));
        }
    }

    #[test]
    fn submitting_attachments_consumes_them_but_keeps_the_output_contract() {
        let mut options = TurnOptions { output: Some("report.md".into()), ..Default::default() };
        options.attachments.push(rook_proto::Attachment::Text { name: "source".into(), text: "data".into() });
        assert_eq!(for_turn(&mut options).attachments.len(), 1);
        assert!(for_turn(&mut options).attachments.is_empty());
        assert_eq!(options.output.as_deref(), Some("report.md"));
    }
}
