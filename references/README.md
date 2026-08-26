# References

Upstream agent sources, as shallow git submodules. They are here to be **read** —
when a problem has already been solved well somewhere, reading that solution beats
inventing a worse one.

They are **not** fetched by a normal `git clone`. Get them when you want them:

```sh
cargo xtask refs init          # clone all, shallow (~870 MB)
cargo xtask refs init codex    # or just one
```

| dir | upstream | license | why it is here |
|---|---|---|---|
| `acp/` | [agentclientprotocol/agent-client-protocol](https://github.com/agentclientprotocol/agent-client-protocol) | Apache-2.0 | The ACP spec and its Rust SDK. The reference for `rookd acp`. |
| `codex/` | [openai/codex](https://github.com/openai/codex) | Apache-2.0 | Closest architectural relative: a Rust agent with a TUI and an app-server. |
| `goose/` | [aaif-goose/goose](https://github.com/aaif-goose/goose) | Apache-2.0 | Rust agent with extensions, recipes and MCP. |
| `opencode/` | [anomalyco/opencode](https://github.com/anomalyco/opencode) | MIT | The best-regarded TUI, plus its skills and session model. |
| `cline/` | [cline/cline](https://github.com/cline/cline) | Apache-2.0 | Context-window management and diff-editing reliability. |
| `hermes/` | [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) | MIT | Memory, skills and local-model handling. |
| `openhands/` | [OpenHands/OpenHands](https://github.com/OpenHands/OpenHands) | MIT | Autonomous task execution and sandboxing. |

`agent0ai/agent-zero` is deliberately absent: its repository declares no license,
so there is nothing here that says reading it is fine.

## The rule

**Read for the idea, write our own code.** These projects are Apache-2.0 and MIT,
so copying is legally possible with attribution — but Rook's value is in being
smaller and clearer than what it learns from, and a pasted subsystem brings its
assumptions with it. If something genuinely should be reused verbatim, add it as a
dependency instead, and record it in [PORTED.md](PORTED.md) with its license.

## Keeping track of what has not been looked at

Each submodule is pinned at the commit we last read. Upstream moves; the pointer
does not, until someone advances it. That gap is the backlog.

```sh
cargo xtask refs status        # how far behind each pointer is, and what landed
cargo xtask refs advance codex # move one pointer, printing what came in
```

`advance` prints the incoming commit subjects precisely so they can be triaged —
anything worth acting on goes into [PORTED.md](PORTED.md) or the
[roadmap](../docs/roadmap.md), and anything not worth acting on is dismissed
deliberately rather than by never looking.
