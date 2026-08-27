//! What the tool list costs on every request, in the shape the agent sends.
//!
//! It lived in `rook-tools`, which cannot see the tools the loop adds itself —
//! `delegate`, the memory three, the skill two — so it priced two thirds of the
//! list and missed the largest entry in it.

use std::sync::Arc;

use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_llm::{Provider, Request, Response};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

struct Silent;

#[async_trait::async_trait]
impl Provider for Silent {
    fn id(&self) -> &str {
        "none/none"
    }
    fn context_window(&self) -> usize {
        128_000
    }
    async fn complete(&self, _: Request) -> rook_llm::Result<Response> {
        Err(rook_llm::LlmError::Other("not called".into()))
    }
}

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let (skills, _) = SkillIndex::discover(&[]);
    let mut config = Config::default();
    let estimate = |t: &rook_llm::ToolSpec| {
        (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4)
    };

    for lazy in [false, true] {
        config.agent.lazy_tools = lazy;
        let rook = Rook::from_parts(
            Store::open(dir.path()).unwrap(),
            config.clone(),
            Environment::bare("linux", "x86_64", "0.1.0"),
            skills.clone(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("pricing").unwrap();
        let mut agent = AgentLoop::new(&rook, Arc::new(Silent), session);
        agent.ask_via(Arc::new(rook_tools::ask::NoOne));

        let mut priced: Vec<(String, usize)> =
            agent.tool_specs().iter().map(|t| (t.name.clone(), estimate(t))).collect();
        priced.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let total: usize = priced.iter().map(|(_, n)| n).sum();

        println!("{}:", if lazy { "stubs (lazy_tools = true)" } else { "full schemas" });
        for (name, cost) in &priced {
            println!("  {name:<14} ~{cost} tok");
        }
        println!("  {:-<26}\n  total          ~{total} tok/call\n", "");
    }
}
