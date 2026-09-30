//! A bounded credential store, separate from config, transcripts and diagnostics.
use super::protocol::{Grant, Result};
use crate::mcp_connections::Settings;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

const FILE: &str = "credentials.json";
const LOCK: &str = "credentials.lock";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub key: String,
    pub server_name: String,
    pub epoch: String,
    pub grant: Grant,
    /// Written before a refresh exchange. An interrupted or lost rotation must
    /// never be retried with a refresh token that may already have been used.
    pub refreshing: bool,
}

#[derive(Clone)]
pub(super) struct Store {
    pub directory: PathBuf,
    pub limits: Settings,
}
impl Store {
    pub fn configured(limits: Settings) -> Self {
        Self { directory: crate::paths::home().join("mcp-auth"), limits }
    }
    pub fn read(&self) -> Result<Vec<Entry>> {
        let text = match rook_contain::files::read_text(
            &self.directory,
            Path::new(FILE),
            self.limits.oauth_max_bytes,
        ) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err("cannot read private MCP credentials within mcp_connections.oauth_max_bytes");
            }
        };
        let entries: Vec<Entry> = rook_llm::catalog::entries(&text, "entries", self.limits.oauth_max_entries)
            .map_err(|_| "invalid or oversized private MCP credential store; restore it or sign out")?;
        let mut keys = std::collections::BTreeSet::new();
        if entries.iter().any(|entry| {
            !keys.insert(&entry.key)
                || entry.key.len() != 64
                || entry.epoch.len() != 43
                || entry.server_name.len() > 256
                || !entry.grant.valid()
        }) {
            return Err("invalid private MCP credential entry; restore the credential store");
        }
        Ok(entries)
    }
    pub async fn lock(&self, patience: u64) -> Result<Guard> {
        crate::paths::private_dir(&self.directory)
            .map_err(|_| "cannot create private MCP credential directory")?;
        let file = rook_contain::files::lock_file(&self.directory, Path::new(LOCK))
            .map_err(|_| "cannot open MCP credential lock")?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(patience);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Guard(file)),
                Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(_) => {
                    return Err("MCP credentials are being changed elsewhere; try again after it finishes");
                }
            }
        }
    }
    pub fn write(&self, entries: &[Entry]) -> Result<()> {
        if entries.len() > self.limits.oauth_max_entries {
            return Err(
                "MCP credential store is full; sign out unused servers or increase mcp_connections.oauth_max_entries",
            );
        }
        #[derive(Serialize)]
        struct Entries<'a> {
            entries: &'a [Entry],
        }
        struct Bounded {
            bytes: Vec<u8>,
            limit: usize,
        }
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                    return Err(std::io::Error::other("credential byte limit"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut encoded = Bounded { bytes: Vec::new(), limit: self.limits.oauth_max_bytes };
        serde_json::to_writer(&mut encoded, &Entries { entries })
            .map_err(|_| "MCP credentials exceed mcp_connections.oauth_max_bytes")?;
        rook_contain::files::write_private(&self.directory, Path::new(FILE), &encoded.bytes)
            .map_err(|_| "cannot save private MCP credentials")
    }
    pub async fn save(&self, config: &rook_mcp::ServerConfig, grant: Grant) -> Result<()> {
        let _guard = self.lock(config.oauth.timeout_secs).await?;
        let key = key(config);
        let mut entries = self.read()?;
        entries.retain(|entry| entry.key != key);
        if entries.len() >= self.limits.oauth_max_entries {
            return Err("MCP credential store is full; sign out unused servers first");
        }
        entries.push(Entry {
            key,
            server_name: config.name.clone(),
            epoch: super::protocol::random()?,
            grant,
            refreshing: false,
        });
        self.write(&entries)
    }
}
pub(super) struct Guard(std::fs::File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) fn key(config: &rook_mcp::ServerConfig) -> String {
    let mut hash = Sha256::new();
    for text in [
        &config.name,
        config.url.as_deref().unwrap_or_default(),
        &config.oauth.client_id,
        &config.oauth.issuer,
    ]
    .into_iter()
    .chain(config.oauth.scopes.iter().map(String::as_str))
    {
        hash.update((text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    }
    hex::encode(hash.finalize())
}
