//! Describe the installed catalog; never reconstruct equipment or probe it.
use super::{AgentLoop, CHANGES_THINGS, MAX_DEPTH};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) const TOOL: &str = "tool_status";
const PAGE: usize = 64;

// Names of optional engine capabilities, not promises that they are enabled.
const OPTIONAL: &[&str] = &[
    "ask",
    "request_secret",
    "plan",
    "docs",
    "find_skill",
    "load_skill",
    "write_skill",
    "remember",
    "forget",
    "recall",
    "delegate",
    "subagents",
    "stance",
    "verify",
    "worktree",
    "web_fetch",
    "web_search",
    "browser",
    "ssh_workspace",
    "job",
    "mcp_tools",
    "mcp_call",
    crate::composite_recipes::TOOL,
    "subrecipe_read",
    "inbound_read",
    TOOL,
    crate::work::managed::deferred::TOOL,
];

#[derive(Clone, Copy)]
struct Availability {
    status: &'static str,
    reason: Option<&'static str>,
}

impl AgentLoop<'_> {
    pub(super) fn tool_status(&self, args: &Value) -> Result<Value, String> {
        let fields = args.as_object().ok_or("tool_status expects an object")?;
        if fields.keys().any(|key| key != "query") {
            return Err("tool_status accepts only query: a tool name or the returned next token; omit it for the first page".into());
        }
        let query = fields
            .get("query")
            .map(|value| {
                value
                    .as_str()
                    .filter(|text| {
                        !text.is_empty() && text.len() <= 129 && !text.chars().any(char::is_control)
                    })
                    .ok_or("query must be a nonempty tool name or returned next token (at most 129 bytes)")
            })
            .transpose()?;
        let continuation = query
            .and_then(|query| query.split_once(':'))
            .filter(|(source, _)| source.len() == 64 && source.bytes().all(|b| b.is_ascii_hexdigit()));
        let (name, after, expected) = match continuation {
            Some((source, name)) if !name.is_empty() && name.len() <= 64 => (None, Some(name), Some(source)),
            _ if query.is_none_or(|name| name.len() <= 64) => (query, None, None),
            _ => {
                return Err(
                    "use a tool name (at most 64 bytes), or pass the unchanged next token as query".into()
                );
            }
        };
        let specs = self.tool_specs();
        let offered: BTreeSet<_> = specs.iter().map(|spec| spec.name.as_str()).collect();
        let standard = rook_tools::ToolBox::standard();
        let standard_names = standard.names();
        let installed = self.tools.names();
        let mcp: BTreeSet<_> = self.tools.mcp_tool_names().collect();
        let candidates: BTreeSet<_> = offered
            .iter()
            .copied()
            .chain(installed.iter().copied())
            .chain(standard_names.iter().copied())
            .chain(OPTIONAL.iter().copied())
            .chain(mcp.iter().copied())
            .collect();
        let availability = |name: &str| self.availability(name, &offered, &mcp, &installed);
        let mut hash = Sha256::new();
        hash.update(b"rook-tool-status-v1\0");
        for name in &candidates {
            let state = availability(name);
            for part in [*name, state.status, state.reason.unwrap_or("")] {
                hash.update(part.as_bytes());
                hash.update([0]);
            }
        }
        let source = format!("{:x}", hash.finalize());
        if expected.is_some_and(|value| value != source)
            || after.is_some_and(|cursor| expected.is_none() || !candidates.contains(cursor))
        {
            return Err(
                "tool catalog changed or continuation is unbound; request the first page again".into()
            );
        }
        let row = |name: &str| {
            let state = availability(name);
            json!({"name":name,"status":state.status,"reason":state.reason,
                "route":(state.status == "deferred").then_some("mcp_call")})
        };
        let (items, next) = if let Some(name) = name {
            (vec![row(name)], None)
        } else {
            let mut names = candidates.iter().copied().filter(|name| after.is_none_or(|after| *name > after));
            let page: Vec<_> = names.by_ref().take(PAGE).collect();
            let next = names.next().and_then(|_| page.last().map(|name| format!("{source}:{name}")));
            (page.into_iter().map(row).collect(), next)
        };
        Ok(json!({
            "scope":"current_session_catalog", "source":source,
            "items":items,"next":next,"total":candidates.len(),
            "mcp_catalog_dirty":self.tools.mcp_catalog_changed(),
            "notice":"Availability describes this session's installed catalog, not all Rook capabilities or authorization. Calls still require valid arguments, policy checks and any approval. No equipment was probed, installed or reconfigured."
            ,"usage":"Omit query for the first page; use a tool name for one result or pass next unchanged as query for the next page."
        }))
    }

    fn availability(
        &self,
        name: &str,
        offered: &BTreeSet<&str>,
        mcp: &BTreeSet<&str>,
        installed: &[&str],
    ) -> Availability {
        if offered.contains(name) {
            return Availability { status: "available", reason: None };
        }
        if mcp.contains(name) && offered.contains("mcp_call") {
            return Availability {
                status: "deferred",
                reason: Some(
                    "MCP schema deferred from the advertised prefix; inspect mcp_tools and call through mcp_call",
                ),
            };
        }
        let reason = if !self.equipment.allows(name) {
            "excluded by the current recipe's tool allowlist"
        } else if self.review_only {
            "retained-source review context exposes only its installed reader"
        } else if self.checking && CHANGES_THINGS.contains(&name) {
            "verification cannot expose tools that change the work"
        } else if self.rook.config.sandbox.network_policy.enabled
            && (["docs", "find_skill"].contains(&name)
                || (installed.contains(&name) && !rook_tools::network::local_tool(name)))
        {
            "excluded by domain network policy"
        } else if ["remember", "forget", "recall"].contains(&name) && !self.rook.config.memory.enabled {
            "memory is disabled in this session's configuration"
        } else if name == "plan" && !self.rook.config.agent.todo_tool {
            "the optional checklist tool is disabled"
        } else if name == crate::work::managed::deferred::TOOL {
            "only an admitted root managed stage can schedule its own continuation"
        } else if ["delegate", "subagents", "verify", "stance", "worktree"].contains(&name)
            && self.depth >= MAX_DEPTH
        {
            "the delegation depth limit has been reached"
        } else if name == "request_secret"
            && !self.asker.as_ref().is_some_and(|asker| asker.supports_private_input())
        {
            "this input channel does not support private input; missing input is not consent"
        } else if name == "ask" && self.asker.is_none() {
            "there is no interactive question channel for this turn"
        } else if name == "web_fetch" && !self.rook.config.web.enabled {
            "web access is disabled in this session's configuration"
        } else if name == "web_search"
            && (!self.rook.config.web.enabled || self.rook.config.web.search.is_empty())
        {
            "web search is disabled or has no configured engine"
        } else {
            "not loaded or unknown in this session; this does not establish a product limitation"
        };
        Availability { status: "unavailable", reason: Some(reason) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    struct Unused;
    #[async_trait::async_trait]
    impl rook_llm::Provider for Unused {
        fn id(&self) -> &str {
            "status/unused"
        }
        fn context_window(&self) -> usize {
            32768
        }
        async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            panic!("catalog inspection must never call the model")
        }
    }
    struct Named(String);
    #[async_trait::async_trait]
    impl rook_tools::Tool for Named {
        fn name(&self) -> &str {
            &self.0
        }
        fn spec(&self) -> rook_llm::ToolSpec {
            rook_llm::ToolSpec {
                name: self.0.clone(),
                description: "Read the fixture.".into(),
                parameters: json!({"type":"object"}),
            }
        }
        async fn call(
            &self,
            _: &rook_tools::ToolContext,
            _: &Value,
        ) -> rook_tools::Result<rook_tools::ToolOutcome> {
            panic!("catalog inspection must never execute a tool")
        }
    }
    fn engine(path: &std::path::Path) -> crate::Rook {
        let mut config = crate::Config::default();
        config.web.enabled = false;
        crate::Rook::from_parts(
            rook_store::Store::open(path.join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "0.17.0"),
            rook_skills::SkillIndex::default(),
            path.into(),
        )
    }
    #[test]
    fn catalog_pages_are_bounded_ordered_and_reject_stale_or_unbound_continuations() {
        let root = tempfile::tempdir().unwrap();
        let rook = engine(root.path());
        let session = rook.start_session("status").unwrap();
        let mut agent = AgentLoop::new(&rook, Arc::new(Unused), session);
        for n in 0..100 {
            agent.tools.register(Arc::new(Named(format!("fixture_{n:03}"))));
        }
        let first = agent.tool_status(&json!({})).unwrap();
        let mut names: Vec<_> = first["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names.len(), PAGE);
        assert!(first["total"].as_u64().unwrap() > PAGE as u64);
        let mut next = first["next"].clone();
        assert!(agent.tool_status(&json!({"after":next})).is_err());
        while next.is_string() {
            let page = agent.tool_status(&json!({"query":next})).unwrap();
            assert!(page["items"].as_array().unwrap().len() <= PAGE);
            names.extend(
                page["items"].as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_owned()),
            );
            next = page["next"].clone();
        }
        assert_eq!(names.len(), first["total"].as_u64().unwrap() as usize);
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(names.iter().any(|name| name == "run_subrecipes"));
        assert!(!names.iter().any(|name| name == "subrecipes"));
        agent.tools.register(Arc::new(Named("new_tool".into())));
        assert!(agent.tool_status(&json!({"query":first["next"]})).is_err());
        for invalid in [
            json!({"query":"x".repeat(65)}),
            json!({"query":"\n"}),
            json!({"query":[]}),
            json!({"after":"unknown","source":"wrong"}),
            json!({"query":"ask","after":"ask"}),
            json!({"secret":"private"}),
        ] {
            assert!(agent.tool_status(&invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn recipe_verification_and_network_limits_are_reported_without_granting_authority() {
        let root = tempfile::tempdir().unwrap();
        let mut rook = engine(root.path());
        rook.config.sandbox.network_policy.enabled = true;
        let session = rook.start_session("status").unwrap();
        let mut agent = AgentLoop::new(&rook, Arc::new(Unused), session);
        let docs = agent.tool_status(&json!({"query":"docs"})).unwrap();
        assert!(docs["items"][0]["reason"].as_str().unwrap().contains("network policy"));
        agent.checking = true;
        let write = agent.tool_status(&json!({"query":"remember"})).unwrap();
        assert!(write["items"][0]["reason"].as_str().unwrap().contains("verification"));
        agent.equipment.tools = Some([TOOL.to_owned()].into_iter().collect());
        let read = agent.tool_status(&json!({"query":"read_file"})).unwrap();
        assert!(read["items"][0]["reason"].as_str().unwrap().contains("recipe"));
        assert_eq!(agent.tool_specs().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), [TOOL]);
    }
}
