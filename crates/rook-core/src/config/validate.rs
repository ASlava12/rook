//! Offline checks shared by the configuration editor and command line.
use super::Config;

impl Config {
    /// Structural errors only. This never opens the store, resolves credentials,
    /// spawns configured programs or contacts a provider. Loading stays separate
    /// so a broken file can still be inspected and repaired.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, value, low, high) in [
            ("max_requests", self.user_input.max_requests, 1, 4096),
            ("max_bytes", self.user_input.max_bytes, 4096, 32 * 1024 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("user_input.{name}: expected {low}..={high}"));
            }
        }
        if let Err(key_errors) = self.tui.bindings() {
            errors.extend(key_errors);
        }
        for (name, value, low, high) in [
            ("chat_replay_events", self.server.chat_replay_events, 1, 4096),
            ("chat_replay_bytes", self.server.chat_replay_bytes, 4096, 32 * 1024 * 1024),
            ("chat_queue_events", self.server.chat_queue_events, 1, 4096),
            ("chat_queue_bytes", self.server.chat_queue_bytes, 4096, 32 * 1024 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("server.{name}: expected {low}..={high}"));
            }
        }
        if let Some(error) = self.mcp_connections.error() {
            errors.push(error.into());
        }
        if self.mcp.len() > self.mcp_connections.max_servers {
            errors.push("mcp: too many servers for mcp_connections.max_servers".into());
        }
        if let Err(why) = crate::context::check_compact_at(self.agent.compact_at) {
            errors.push(why);
        }
        for (name, valid, range) in [
            (
                "max_bytes",
                (1024..=4 * 1024 * 1024).contains(&self.model_catalog.limits.max_bytes),
                "1024..=4194304",
            ),
            ("max_models", (1..=4096).contains(&self.model_catalog.limits.max_models), "1..=4096"),
            ("max_pages", (1..=64).contains(&self.model_catalog.limits.max_pages), "1..=64"),
            ("timeout_secs", (1..=60).contains(&self.model_catalog.limits.timeout_secs), "1..=60"),
        ] {
            if !valid {
                errors.push(format!("model_catalog.{name}: expected {range}"));
            }
        }
        for (name, valid, range) in [
            (
                "cache_max_bytes",
                (4096..=16 * 1024 * 1024).contains(&self.model_catalog.cache_max_bytes),
                "4096..=16777216",
            ),
            (
                "learned_window_entries",
                (1..=128).contains(&self.model_catalog.learned_window_entries),
                "1..=128",
            ),
            ("cache_max_entries", (1..=128).contains(&self.model_catalog.cache_max_entries), "1..=128"),
            ("cache_ttl_secs", self.model_catalog.cache_ttl_secs <= 86400, "0..=86400"),
        ] {
            if !valid {
                errors.push(format!("model_catalog.{name}: expected {range}"));
            }
        }
        if !(1024..=32 * 1024 * 1024).contains(&self.agent.max_provider_state_bytes) {
            errors.push("agent.max_provider_state_bytes: expected 1024..=33554432".into());
        }
        for (name, valid, range) in [
            ("max_bytes", (4096..=1048576).contains(&self.mcp_catalog.max_bytes), "4096..=1048576"),
            ("max_tools", (2..=64).contains(&self.mcp_catalog.max_tools), "2..=64"),
            ("max_server_bytes", self.mcp_catalog.max_server_bytes <= 262144, "0..=262144"),
            ("max_server_tools", self.mcp_catalog.max_server_tools <= 128, "0..=128"),
        ] {
            if !valid {
                errors.push(format!("mcp_catalog.{name}: expected {range}"));
            }
        }
        for (name, value, low, high) in [
            ("page_entries", self.transcript.page_entries, 1, 256),
            ("page_bytes", self.transcript.page_bytes, 4096, 1048576),
            ("body_bytes", self.transcript.body_bytes, 128, 65536),
            ("search_bytes", self.transcript.search_bytes, 4096, 16777216),
            ("search_events", self.transcript.search_events, 1, 4096),
            ("quote_bytes", self.transcript.quote_bytes, 128, 65536),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("transcript.{name}: expected {low}..={high}"));
            }
        }
        if !(1..=4096).contains(&self.work.followup_scan_sessions) {
            errors.push("work.followup_scan_sessions: expected 1..=4096".into());
        }
        errors.extend(self.branches.errors());
        errors.extend(crate::models::source_errors(self));
        let mut names = std::collections::BTreeSet::new();
        for (index, server) in self.mcp.iter().enumerate() {
            if let Some(error) = server.catalog_error().or_else(|| server.oauth.error()) {
                errors.push(format!("mcp[{index}]: {error}"));
            }
            if server.name.trim().is_empty()
                || server.name.len() > 256
                || server.name.chars().any(char::is_control)
            {
                errors.push(format!(
                    "mcp[{index}].name: use a nonempty name, at most 256 bytes, without control characters"
                ));
            }
            if !names.insert(&server.name) {
                errors.push(format!("duplicate MCP server name: {}", server.name));
            }
            if server.enabled
                && server.command.trim().is_empty()
                && server.url.as_deref().unwrap_or_default().trim().is_empty()
            {
                errors.push(format!("MCP {}: set command or url, or disable this server", server.name));
            }
        }
        errors
    }
}
