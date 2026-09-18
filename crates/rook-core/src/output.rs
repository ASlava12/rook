//! A requested artifact is written by the harness, after validating its content.
use std::path::{Component, Path, PathBuf};

use crate::{CoreError, Result};

pub(crate) const MAX_SCHEMA_BYTES: usize = 64 * 1024;

pub(crate) struct Contract {
    pub(crate) path: Option<PathBuf>,
    validator: Option<jsonschema::Validator>,
}

impl Contract {
    pub(crate) fn compile(options: &rook_proto::TurnOptions, workspace: &Path) -> Result<Self> {
        if options.schema_retries > 3 {
            return Err(CoreError::Other("schema_retries must be between 0 and 3".into()));
        }
        let path = options
            .output
            .as_deref()
            .map(|name| {
                let path = Path::new(name);
                let path = if path.is_absolute() {
                    path.strip_prefix(workspace)
                        .map_err(|_| CoreError::Other("output must be inside the session workspace".into()))?
                } else {
                    path
                };
                if path.as_os_str().is_empty()
                    || path.components().any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(CoreError::Other(
                        "output must name a file inside the session workspace".into(),
                    ));
                }
                rook_contain::files::validate(workspace, path)
                    .map_err(|e| CoreError::Other(format!("invalid output destination: {e}")))?;
                Ok(path.to_path_buf())
            })
            .transpose()?;
        let validator = options
            .output_schema
            .as_ref()
            .map(|schema| {
                if schema.to_string().len() > MAX_SCHEMA_BYTES {
                    return Err(CoreError::Other("output schema exceeds 64 KiB".into()));
                }
                // HTTP and filesystem resolution are disabled at compile time. A
                // schema can use local $defs, but cannot read files or call a URL.
                jsonschema::options()
                    .build(schema)
                    .map_err(|e| CoreError::Other(format!("invalid output schema: {e}")))
            })
            .transpose()?;
        Ok(Self { path, validator })
    }

    pub(crate) fn violation(&self, reply: &str) -> Option<String> {
        let validator = self.validator.as_ref()?;
        let value = match serde_json::from_str::<serde_json::Value>(reply) {
            Ok(value) => value,
            Err(error) => return Some(format!("not valid JSON: {error}")),
        };
        // Paths and keywords suffice for repair; quoting validation errors can
        // duplicate an arbitrarily large part of the answer.
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .take(8)
            .map(|e| format!("instance {} violates {}", e.instance_path(), e.schema_path()))
            .collect();
        (!errors.is_empty()).then(|| errors.join("; "))
    }
}
