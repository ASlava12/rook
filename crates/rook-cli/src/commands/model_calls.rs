//! Publish an explicit, bounded local export without replacing a file.
use anyhow::{Context, Result, ensure};
use rook_core::model_calls::{Artifact, Format};
use std::{io::Write, path::Path};

pub(super) fn save(report: &Artifact, output: &Path) -> Result<()> {
    ensure!(!report.calls.is_empty(), "no recorded attempts in this selection; no file created");
    ensure!(
        report.format != Format::Dataset || !report.examples.is_empty(),
        "no eligible complete visible pairs; inspect the preview exclusion reasons; no file created"
    );
    let bytes = report.bytes()?;
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("cannot create an export beside {}", output.display()))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(output).map_err(|error| {
        anyhow::anyhow!(
            "cannot save {} without replacing an existing file: {}",
            output.display(),
            error.error
        )
    })?;
    Ok(())
}
