//! Reads are bound to a resolved skill and a content receipt, never ambient home access.
use crate::{CaptureLimits, CoreError, Result, Rook};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub name: String,
    pub identity: String,
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub identity: String,
    pub path: String,
    pub offset: usize,
    pub next: Option<usize>,
    pub text: String,
}

impl Rook {
    pub fn skill_resources(&self, name: &str) -> Result<Catalog> {
        let (_, files, identity) = snapshot(self, name)?;
        Ok(Catalog { name: name.into(), identity, files: files.into_keys().collect() })
    }

    pub fn read_skill_resource(
        &self,
        name: &str,
        identity: &str,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Page> {
        let (root, files, current) = snapshot(self, name)?;
        if current != identity {
            return Err(CoreError::Other("skill changed; load it again before reading resources".into()));
        }
        if path.len() > 4096 || !files.contains_key(path) {
            return Err(CoreError::Other("resource is not in this skill's captured file catalog".into()));
        }
        // No-follow traversal rejects links in every component, including the final file.
        let boundary = rook_contain::files::RecoveryFiles::open(&root, Path::new("."), false)
            .map_err(|source| CoreError::Io { path: root.clone(), source })?;
        let bytes = boundary
            .read(Path::new(path), CaptureLimits::for_skill().max_file_bytes as usize)
            .map_err(|source| CoreError::Io { path: root.join(path), source })?;
        if files.get(path) != Some(&hex::encode(Sha256::digest(&bytes))) {
            return Err(CoreError::Other("skill resource changed during read; reload it".into()));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| CoreError::Other("resource is binary; text reads only".into()))?;
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(CoreError::Other("offset must be a UTF-8 byte boundary within this resource".into()));
        }
        let mut end = offset.saturating_add(limit.clamp(1, 16_384)).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset && end < text.len() {
            end += text[offset..].chars().next().map(char::len_utf8).unwrap_or(0);
        }
        // Revalidation prevents returning a new revision under an old receipt.
        if self.skill_resources(name)?.identity != identity {
            return Err(CoreError::Other("skill changed during read; reload it".into()));
        }
        Ok(Page {
            identity: identity.into(),
            path: path.into(),
            offset,
            next: (end < text.len()).then_some(end),
            text: text[offset..end].into(),
        })
    }
}

fn snapshot(
    rook: &Rook,
    name: &str,
) -> Result<(std::path::PathBuf, std::collections::BTreeMap<String, String>, String)> {
    let skills = rook.skills();
    let resolved = skills.resolve(name, rook.env())?;
    let root = resolved
        .skill
        .dir
        .canonicalize()
        .map_err(|source| CoreError::Io { path: resolved.skill.dir.clone(), source })?;
    let files = resource_hashes(&root)?;
    let receipt = serde_json::to_vec(&(resolved.skill.id(), &root, &files))?;
    Ok((root, files, hex::encode(Sha256::digest(receipt))))
}

fn resource_hashes(root: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let boundary = rook_contain::files::RecoveryFiles::open(root, Path::new("."), false)
        .map_err(|source| CoreError::Io { path: root.into(), source })?;
    let limits = CaptureLimits::for_skill();
    let mut pending = vec![std::path::PathBuf::from(".")];
    let mut files = std::collections::BTreeMap::new();
    let mut visited = 0;
    let mut bytes = 0u64;
    while let Some(dir) = pending.pop() {
        let entries = boundary
            .entries(&dir, limits.max_files)
            .map_err(|source| CoreError::Io { path: root.join(&dir), source })?;
        for (name, is_file, is_dir) in entries {
            visited += 1;
            if visited > limits.max_files {
                return Err(CoreError::Other("skill resource catalog exceeds its entry limit".into()));
            }
            if (!is_file && !is_dir) || name == ".git" {
                continue;
            }
            let rel = if dir == Path::new(".") { std::path::PathBuf::from(name) } else { dir.join(name) };
            if is_dir {
                pending.push(rel);
                continue;
            }
            let content = boundary
                .read(&rel, limits.max_file_bytes.min(limits.max_total_bytes.saturating_sub(bytes)) as usize)
                .map_err(|source| CoreError::Io { path: root.join(&rel), source })?;
            bytes += content.len() as u64;
            if bytes > limits.max_total_bytes {
                return Err(CoreError::Other("skill resources exceed their total byte limit".into()));
            }
            files.insert(rel.to_string_lossy().replace('\\', "/"), hex::encode(Sha256::digest(content)));
        }
    }
    Ok(files)
}
