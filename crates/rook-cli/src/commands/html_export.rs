//! Local publication of a core-built export; never replaces an existing file.
use crate::source::Source;
use anyhow::{Context, Result, ensure};
use rook_core::conversation_export::{Export, Profile, Request};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) trait HistoryRead {
    fn export(&self, session: u128, request: &Request) -> Result<Export>;
}
impl HistoryRead for Source {
    fn export(&self, session: u128, request: &Request) -> Result<Export> {
        self.conversation_export(session, request)
    }
}
impl HistoryRead for rook_core::Rook {
    fn export(&self, session: u128, request: &Request) -> Result<Export> {
        Ok(self.conversation_export(session, request)?)
    }
}
pub(crate) type Report = Export;
pub(crate) fn save(
    source: &impl HistoryRead,
    session: u128,
    from: u64,
    through: Option<u64>,
    output: &Path,
    profile: Profile,
) -> Result<Report> {
    let report = source.export(session, &Request { from, through, profile })?;
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("cannot create an export beside {}", output.display()))?;
    temporary.write_all(report.html.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(output).map_err(|error| {
        anyhow::anyhow!(
            "cannot save {} without replacing an existing file: {}",
            output.display(),
            error.error
        )
    })?;
    Ok(report)
}
/// `NEW_FILE` may contain spaces. An optional `FROM..THROUGH` prefix selects
/// an inclusive range; all other text is the destination path.
pub(crate) fn slash_arguments(rest: &str) -> Result<(u64, Option<u64>, PathBuf, Profile)> {
    let rest = rest.trim();
    let (profile, rest) = match rest.split_once(char::is_whitespace) {
        Some((name, rest)) if matches!(name, "share" | "share-tools" | "archive") => {
            (Profile::parse(name).map_err(anyhow::Error::msg)?, rest.trim())
        }
        _ => (Profile::Share, rest),
    };
    ensure!(!rest.is_empty(), "use /export-html [FROM..THROUGH] NEW_FILE");
    let (first, path) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if let Some((from, through)) = first.split_once("..")
        && !from.is_empty()
        && !through.is_empty()
        && from.bytes().all(|byte| byte.is_ascii_digit())
        && through.bytes().all(|byte| byte.is_ascii_digit())
    {
        ensure!(!path.trim().is_empty(), "use /export-html [FROM..THROUGH] NEW_FILE");
        return Ok((from.parse()?, Some(through.parse()?), PathBuf::from(path.trim()), profile));
    }
    Ok((0, None, PathBuf::from(rest), profile))
}
