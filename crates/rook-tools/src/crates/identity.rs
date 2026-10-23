//! Select one exact registry package before consulting local source.
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

const LOCK_BYTES: usize = 4 * 1024 * 1024;
const REGISTRIES: usize = 256;

pub(super) struct Selected {
    pub version: String,
    pub checksum: String,
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-+.".contains(&b))
        && value != "."
        && value != ".."
}

pub(super) fn limited(path: &Path, maximum: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(std::io::Error::other("input exceeds read limit"));
    }
    Ok(bytes)
}

pub(super) fn resolve(from: &Path, wanted: &str, version: Option<&str>) -> Result<Selected, String> {
    if !component(wanted) || version.is_some_and(|v| !component(v)) {
        return Err("Use a Cargo.lock package name and an exact version, without path separators".into());
    }
    let mut here = Some(from);
    let bytes = loop {
        let Some(dir) = here else { return Err("No Cargo.lock found for this workspace".into()) };
        match limited(&dir.join("Cargo.lock"), LOCK_BYTES) {
            Ok(bytes) => break bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => here = dir.parent(),
            Err(_) => {
                return Err(
                    "The nearest Cargo.lock is unreadable or exceeds 4 MiB; no parent fallback".into()
                );
            }
        }
    };
    let lock: toml::Table = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or("The nearest Cargo.lock is invalid UTF-8/TOML; no parent fallback")?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("The nearest Cargo.lock has no package array")?;
    let named: Vec<_> = packages
        .iter()
        .filter_map(toml::Value::as_table)
        .filter(|p| p.get("name").and_then(toml::Value::as_str) == Some(wanted))
        .collect();
    if named.is_empty() {
        return Err(format!("{wanted:?} is not in the nearest Cargo.lock; use its package name"));
    }
    let mut versions: Vec<_> =
        named.iter().filter_map(|p| p.get("version").and_then(toml::Value::as_str)).collect();
    if versions.len() != named.len() || versions.iter().any(|v| !component(v)) {
        return Err("Cargo.lock contains an invalid package version".into());
    }
    versions.sort_unstable();
    versions.dedup();
    if version.is_none() && versions.len() > 1 {
        let shown = versions.iter().take(16).copied().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "Multiple locked versions for {wanted}: {shown}. Supply an exact version; no version was chosen."
        ));
    }
    let version = version.unwrap_or(versions[0]);
    let matches: Vec<_> = named
        .into_iter()
        .filter(|p| p.get("version").and_then(toml::Value::as_str) == Some(version))
        .collect();
    let [package] = matches.as_slice() else {
        return Err(if matches.is_empty() {
            "The requested version is not in Cargo.lock"
        } else {
            "Multiple locked source records for that version; no source was chosen"
        }
        .into());
    };
    let source = package.get("source").and_then(toml::Value::as_str).unwrap_or_default();
    if !source.starts_with("registry+") {
        return Err(
            "This locked package is a path/git dependency; crate_api currently reads registry packages only"
                .into(),
        );
    }
    let checksum = package
        .get("checksum")
        .and_then(toml::Value::as_str)
        .filter(|c| c.len() == 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("The locked registry package has no valid package checksum")?;
    Ok(Selected { version: version.into(), checksum: checksum.into() })
}

pub(super) fn unpacked(name: &str, version: &str, checksum: &str) -> Result<PathBuf, String> {
    let home =
        std::env::var_os("CARGO_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
            let user = ["HOME", "USERPROFILE"]
                .into_iter()
                .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()));
            user.map(PathBuf::from).unwrap_or_default().join(".cargo")
        });
    let entries = std::fs::read_dir(home.join("registry/src"))
        .map_err(|_| "No local cargo registry source; run cargo fetch explicitly if needed")?;
    let mut registries = Vec::new();
    for entry in entries.take(REGISTRIES + 1) {
        registries.push(entry.map_err(|_| "Cannot enumerate the local cargo registry")?);
    }
    if registries.len() > REGISTRIES {
        return Err("Local registry scan exceeds 256 entries".into());
    }
    registries.sort_by_key(|r| r.file_name());
    let mut remaining = 256 * 1024 * 1024usize;
    for registry in registries {
        if !registry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = registry.path().join(format!("{name}-{version}"));
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
            continue;
        }
        let archive =
            home.join("registry/cache").join(registry.file_name()).join(format!("{name}-{version}.crate"));
        let Ok(file) = std::fs::File::open(archive) else { continue };
        let maximum = (64 * 1024 * 1024).min(remaining.saturating_sub(1));
        if maximum == 0 {
            return Err("Local archive hash budget exhausted".into());
        }
        let mut input = file.take(maximum as u64 + 1);
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 16384];
        let mut count = 0;
        let mut failed = false;
        loop {
            let read = match input.read(&mut buffer) {
                Ok(n) => n,
                Err(_) => {
                    failed = true;
                    break;
                }
            };
            if read == 0 {
                break;
            }
            count += read;
            hash.update(&buffer[..read]);
        }
        remaining = remaining.saturating_sub(count);
        if !failed && count <= maximum && format!("{:x}", hash.finalize()) == checksum {
            return Ok(path);
        }
    }
    Err("No local registry copy has the locked package checksum; inspect the cache or run cargo fetch explicitly".into())
}
