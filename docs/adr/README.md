# Architecture decision records

One file per decision that would be expensive to reverse. Each records what was
chosen, what was rejected, and what it costs — the last part being the one that
usually goes missing.

| # | Decision | Status |
|---|---|---|
| [0001](0001-rust-everywhere.md) | Rust for every component | accepted |
| [0002](0002-store-redb-cas-zstd.md) | redb + content addressing + zstd dictionaries, not SQLite | accepted |
| [0003](0003-agent-skills-format.md) | Adopt the Agent Skills format and extend it in free keys | accepted |
| [0004](0004-progressive-disclosure.md) | Skill cards and tool stubs by default | accepted |
| [0005](0005-captures-not-git.md) | Content-addressed captures instead of git snapshots | accepted |
| [0006](0006-single-writer-store.md) | One writer process; the CLI opens the store directly for now | accepted, revisit |
| [0007](0007-no-js-build-step.md) | The web UI is one hand-written file with no bundler | accepted |
| [0008](0008-hand-written-mcp-client.md) | A hand-written MCP client instead of the SDK | accepted |
