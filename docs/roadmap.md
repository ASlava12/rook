# Roadmap

What exists, what is next, and what is deliberately not being built. Ordered by
what unblocks the most.

## Done

- **Content-addressed store** — blake3 addressing, per-kind trained zstd
  dictionaries, inlining, mark-and-sweep GC with a caller-supplied expander,
  retention policy with real defaults, integrity verification, format versioning.
  Measured at 20.5× end to end. 13 tests.
- **Skills** — Agent Skills `SKILL.md` parsing with unknown fields preserved,
  environment detection, `requires` gating, `variants` bodies, source-then-version
  resolution with full mismatch reporting, catalog cards for progressive
  disclosure. 13 tests.
- **Skill and workspace versioning** — capture, history, diff, undoable rollback,
  budgeted captures that refuse rather than thrash. 11 tests.
- **Agent loop** — tool dispatch, `load_skill`, pre-request context budgeting,
  mechanical compaction, automatic checkpointing before any mutating tool call,
  everything logged to the store. 12 tests against a scripted provider; not yet
  exercised against a live model in CI.
- **Session diffs** — what a session changed on disk, from its own checkpoints:
  no repository, no staging, and files that were never versioned. Text diffs
  with counts, binary reported without one. 4 tests.
- **Rewind and fork** — `rook session rewind` restores workspace files as well as
  the conversation, deleting files the turn created, and forks rather than
  truncating so the rewound-past turns stay readable.
- **Context visibility** — `rook session context` breaks the cost down by kind and
  separates what a fresh turn would carry from what is merely stored.
- **Tools** — paged `read_file`, `write_file`, batched and unambiguous `edit_file`,
  `list_dir`, regex `search`, `run_command` with timeout, output cap and deny
  list. Every edit to one file goes in one call, applied in order against the
  text as it then stands; a batch that cannot finish writes nothing. Every cap
  is enforced while output is read rather than after, both ends of a long output
  survive it — a compiler's first error is at the head and the reason for a
  failure is not the consequences at the tail — and a timeout kills the whole
  process group rather than the shell in front of it. 26 tests.
- **Asking the user** — `ask` puts a form of independent questions to whoever is
  driving: numbered choices in the CLI and TUI, radio and checkbox rows in the
  browser, the approval dialog over ACP. Typing past the options is always the
  answer, so no front end has to render an "Other" row. Registered only where
  someone can actually answer, so an unattended run neither advertises it nor
  pays for its schema. 21 tests.
- **Compaction** — the model summarises the span it replaces, into goal / done /
  open sections, and the summary is recorded in the log as a durable checkpoint:
  later turns and later processes start from it instead of replaying the span.
  A failed summary degrades to a marker rather than wedging the turn, and each
  compaction folds the previous summary into the next, so a session compacted
  many times still knows what it did at the start. 5 tests.
- **Permissions** — three modes and regex allow/ask/deny rules over what a call
  would actually do, defaulting to asking, with denial beating everything
  ([ADR-0009](adr/0009-ask-before-acting.md)). 12 tests, including the shipped
  deny rules checked in both directions.
- **Delegation** — a `delegate` pseudo-tool runs sub-tasks in child sessions with
  their own context and returns only their conclusions, so bulk stays out of the
  parent while remaining readable in the children. Several tasks run concurrently
  under a configured cap; one failing does not lose the rest. Depth-limited, with
  optional inheritance of the recent exchange. 7 tests.
- **Search** — over everything said, read and run, ranked by how much a line
  dwells on the query. Matching runs over distinct objects rather than events,
  so a file read twenty times is decompressed once. Bounded, and says when the
  scan stopped early. In the CLI, chat, the API and the web UI. 5 tests.
- **Memory** — durable facts with provenance, global or per-workspace scope,
  pinning, and a version per change so `memory since`, `memory diff` and rollback
  all work. Retrieval is term overlap with prefix matching, budgeted into the
  prompt; the agent edits it through `remember`/`forget`/`recall`. This is the
  shape [hermes-agent#12238](https://github.com/NousResearch/hermes-agent/issues/12238)
  asked for. 7 tests.
- **Planning** — one system-prompt line rather than a checklist tool, and a goal
  the user sets on a session. Chosen against the obvious design on the strength
  of goose's own A/B benchmark ([ADR-0010](adr/0010-no-todo-tool.md)). 3 tests.
- **Code intelligence** — an LSP client and four tools over it: diagnostics,
  definition, references and workspace symbols, addressed by symbol name rather
  than by position. Known servers are detected on `PATH` and started lazily, and
  a file edited since it was opened is re-sent before it is queried, so the
  answer describes the code as it is now. The pool belongs to the session, not
  the turn, so servers are not restarted between turns. 10 tests against a mock server, plus a
  real check-edit-check turn against clangd.
- **Hooks** — five events with matchers, able to allow / ask / deny a tool call
  and to add context the model sees. A failing `pre_tool` hook blocks; no hook
  overrides the deny list. 7 tests.
- **ACP server** — `rook acp` speaks v1 over stdio: initialize, session
  new/load/list/prompt/cancel, streamed `session/update`, approvals through
  `session/request_permission`, the editor's buffers through `fs/*`, its terminal
  through `terminal/*`, and its settings through modes and `configOptions`. Field
  names come from the schema in `references/acp`, not from memory. 21 tests over
  duplex streams, some driving whole turns against a scripted provider.
- **MCP client** — stdio and streamable-HTTP transports, written directly rather
  than via the SDK ([ADR-0008](adr/0008-hand-written-mcp-client.md)). An HTTP
  answer may be JSON or an event stream and both are handled; the session id from
  `initialize` is carried on later requests. Servers connect concurrently,
  failures are reported rather than fatal, and their tools join the toolbox
  namespaced `server__tool`. 16 tests against mock servers.
- **Providers** — OpenAI-compatible HTTP (Ollama, LM Studio, llama.cpp, vLLM,
  OpenAI, OpenRouter) and the Anthropic Messages API, which the OpenAI dialect
  cannot reach, with prompt caching: the system block and the conversation so
  far each carry a breakpoint, and anything that varies per turn is kept behind
  them, plus adaptive thinking with a visible summary and `output_config.effort`.
  18 tests over the wire shape, since that is where the dialects differ. `rook models` lists what an endpoint serves and `rook
  doctor` says whether the configured one is among them; the context window can
  be set explicitly rather than assumed from the provider's name. Streaming over
  SSE with a configurable idle watchdog, so a
  dropped connection surfaces as a stall instead of looking like deep thought.
  Tool calls are emitted only once whole. Providers that cannot stream get a
  one-shot fallback, so no caller branches on it.
- **Three front ends** — an interactive conversation with slash commands and
  interruptible turns, a CLI with `--json` throughout, a ratatui browser, and a
  web UI that both reads the store and drives a turn over a websocket. All three
  run turns, stream them and answer approvals the same way, over one engine.
- **Skills the agent writes** — `write_skill` records a procedure the turn had to
  work out, scoped by `requires` to where it actually holds, validated by reading
  it back, and captured as a version so a rewrite keeps the old one. The index
  reloads in place, so the next turn's catalog has it without a restart. 7 tests.
- **The TUI started for real** — on a pseudo-terminal, with the screen replayed
  out of the escape stream and read back a row at a time. Nothing else can see
  that it starts at all: it needs a tty to draw, so a panic on launch would
  leave every other test green.
- **The CLI tested as a user meets it** — 12 tests drive the real binary against
  a real store: every read command on an empty store, `--json` that parses,
  a skill scoped away and explained by `skills why`, capture and rollback, a
  broken config that fails loudly, and the daemon path where a read goes over
  HTTP and a write says where the lock is.
- **An allow rule that covers every path too** — a write names all the paths it
  will touch, and one matching path allowed the rest, so `src/main.rs` alongside
  `/etc/passwd` ran without asking. And a plain rule matched anywhere in a path,
  so `src/` covered `notsrc/`. Both closed; a regular expression is untouched.
  4 tests.
- **An allow rule that covers the whole line** — allowing `ls` also allowed
  `ls && rm -rf ~/important` and `cat notes.md; curl … | sh`, without asking,
  because only the start of the line was matched. Every command on the line must
  now be allowed, and a line carrying `$(…)` is asked about rather than taken
  apart wrongly. 2 tests.
- **A deny list that does not cry wolf** — the default rules matched a word
  anywhere in a command, so `grep -r mkfs docs/` and `echo 'never run mkfs'`
  were refused outright, and nothing overrides a denial. They are now anchored
  to command position as well as to their argument, allowing for `sudo` and
  friends. 3 tests, covering what must be refused and what must not.
- **The browser can set a goal and rewind** — the last two conversational
  actions it could not reach. A goal field on the session view, and a rewind on
  every entry in a transcript that asks whether to put the files back, since
  forking the conversation alone changes nothing on disk. All four front ends
  now reach every one of them. 3 tests.
- **A prompt too large to send is refused, not sent** — compaction summarises
  history and cannot make one message smaller, so a pasted build log went to the
  provider whole and came back as an error about a limit the user never saw. It
  now says how much the turn needs, how much fits, and that reading a file is
  paged. `LlmError::ContextOverflow` had been declared for exactly this and
  raised by nothing. 3 tests.
- **A session killed mid-tool-call can be resumed** — the replay paired calls
  with results by adjacency, which holds until a process dies between logging
  the two. Every provider refuses a request where an assistant asked for a tool
  and nothing answered, so such a session could never be used again — and only a
  real server would have said so, which is why the test is one of the ones that
  runs over a socket. The replay now answers an unfinished call by saying that
  is what it was. 2 tests.
- **A delegation that says how it is going** — three sub-tasks ran concurrently
  and the parent waited for all of them, so a front end showed `· delegate` and
  then minutes of silence that could not be told from a hang. Each one is now
  reported as it lands, counted, and named, in all five front ends. The report
  still comes back in the order the tasks were asked for rather than the order
  they finished. 2 tests.
- **A context figure that matches the request** — `session context` counted
  every event but checkpoints as live, so asides, errors and reasoning inflated
  the number the command exists to explain, and could report a session over the
  compaction threshold that the loop would not compact. All three places that
  ask which events reach the model now ask one function. 2 tests.
- **Compaction that summarises the conversation, not the bookkeeping** — it read
  the whole transcript while the replay that builds the model's messages uses
  five event kinds, so checkpoint manifests, asides and failed skill loads were
  summarised as if they had been said. Both now read the same thing.
- **Search that can find a file's contents** — captured files were scanned and
  could never be reported, because only an object that is the body of an event
  became a hit. A match in a file now names the path and the capture, including
  a checkpoint someone made by hand, which has no session at all. 4 tests.
- **One set of slash commands, not two** — the TUI's chat understood `/btw` and
  nothing else, so a TUI user could not set a goal, see what a turn costs, undo,
  or start a session. The commands now return text instead of printing it, and
  both the terminal chat and the TUI render the same answers from the same code.
- **Memory visible, and correctable, outside the CLI** — what the agent believes
  about you steers every later turn, and the only way to see or fix it was
  `rook memory`. Both the TUI and the browser now have a Memory view over a new
  `/api/memory`, and the browser can forget a fact: one nobody can remove is one
  that quietly steers everything. 4 tests, including the tab drawn on a pty.
- **What a session was for and what it did, in every view** — the goal and the
  files it changed were reachable only through `/goal` and `/diff` in the chat.
  The session browser in both the TUI and the web now leads with them, which is
  usually the question a transcript is being read to answer, over a new
  `/api/sessions/{id}/changes`.
- **Approvals and effort changeable everywhere** — `/mode` and `/effort` in the
  chat REPL, F2 and F3 in the TUI with both shown in its footer, two selects in
  the browser, and `configOptions` over ACP. All four reach the same policy;
  effort was previously only settable by editing `config.toml` and restarting.
- **Session settings the editor can offer** — approvals and reasoning effort go
  out as `configOptions`, which the protocol prefers to modes and will keep once
  modes are removed. Both are sent, so an older client still renders the modes
  and both routes reach the same policy. Effort was previously only settable in
  `config.toml`. 4 tests.
- **Commands in the editor's terminal** — a client that advertises `terminal`
  runs what the agent asks for in its own panel, so a build is something the
  user watches rather than something they are told about afterwards. Create,
  wait, read, release, in that order, with a kill on timeout; the tool reports
  the same exit code and truncation flag wherever it ran. 7 tests.
- **Approval mode from the editor** — the three modes are offered on
  `session/new` and `session/set_mode` changes the one in force, so a user can
  drop to read-only from the editor's menu instead of editing `config.toml` and
  restarting. The same policy the CLI and the config reach. 3 tests, one of them
  proving a mode set before a turn is the mode that turn obeys.
- **Skills that can carry scripts** — the format allows bundled files and the
  layout is documented, but a loaded skill named neither them nor its own
  directory, so a body saying `scripts/check.sh` could not be followed. Now it
  says both, and a skill that is only a `SKILL.md` still costs nothing.
- **A tool call that visibly finishes** — the provider's stream ends when the
  model stops asking for a tool, not when the tool has run, so every front end
  showed calls starting and none finishing. ACP had the `tool_call_update`
  message written and never sent. A turn now reports completion and whether it
  failed, and the editor, the browser, the TUI and both CLI paths all show it.
- **The editor's buffers, not the disk** — under ACP, a client that advertises
  `fs.readTextFile` now serves every file read and write, so the agent sees what
  the user is looking at rather than the last saved version and does not edit
  their unsaved work back. Asked only when advertised, as the protocol requires,
  and the workspace boundary still applies — it is not the editor's to widen.
  7 tests, including a turn driven end to end: editor to agent to tool and back.
- **The chat REPL under test** — its dozen slash commands were reachable only
  through an interactive prompt and checked by nobody. Driven from a pipe they
  need no model, so three tests now cover what each says with nothing to report,
  that a goal survives within a session and not into a new one, and that an
  unknown command is refused rather than sent to the provider.
- **`readonly` that is actually read-only** — the loop implements some tools
  itself and handled them before the policy gate, so `write_skill` wrote to disk
  in a mode whose whole promise is that nothing changes the machine. The gate
  now takes a supplied risk, so a pseudo-tool that writes answers to it too.
- **A whole turn over a real socket** — every other loop test hands the agent a
  scripted provider and skips the wire, so a mis-shaped tool schema or an
  unpaired `tool_call_id` would pass all of them and fail against the first real
  model. Four tests run turns against a server that speaks the OpenAI dialect
  and asserts on what it received. Confirmed to bite by dropping the pairing id
  and watching them fail.
- **The shipped skills actually shipping** — three built-in skills sat in the
  source tree and reached nobody: `cargo xtask dist` built binaries and left
  them behind, so a release had none and no dev build would ever notice. Now
  packaged next to the binary, with tests that each one parses, applies where
  its requirements are met, and names what is missing where they are not. 4
  tests.
- **A first failure that says what to do** — no model is reachable on a fresh
  machine, and the first command a new user runs used to answer with
  `transport error: error sending request for url (…)`. It now names the
  endpoint without the path, says whether anything is listening there, and gives
  the command that lists what an endpoint offers. An exported-but-blank API key
  reads as unset rather than as a 401. A server that is running but has not been
  given the model — the default spec names one that must be pulled first, so this
  is the second thing a new user hits — is asked which it does have, and the
  refusal names them. A base URL that serves nothing keeps its 404, because
  "the model is missing" would there be a guess. 9 tests.
- **What a tool measured reaches the hooks** — every tool records facts about
  its call (which MCP server answered, whether a command timed out, how much of
  a file came back) and every one of them was thrown away by the loop. A
  `post_tool` hook now gets the whole outcome, so it can act on the fact rather
  than parse it back out of prose written for a model.
- **Memory scoped where it was asked for** — a fact's identity is its text, so
  the same sentence learned globally and in a project is one fact. It now keeps
  the wider scope rather than the first one, and says so when neither scope
  contains the other instead of silently picking. 5 tests.
- **Memory that stops repeating itself** — recall spends its budget once on a
  fact said twice, and `remember` names an older fact that says nearly the same
  thing so the model can supersede it. Two thresholds, because the costs differ:
  suppression needs 0.95 and the advisory 0.55. Measured — "prefer tabs" against
  "prefer tabs in Makefiles" scores 0.80 and "port 7717" against "port 8080"
  scores 0.75, so a single threshold would have hidden the fact that says
  something new. 7 tests.
- **An API with tests** — `rookd` had none, though it is the surface the web UI
  and now the CLI both read through. 10 cover the paged endpoints, the typed
  error shape, a session id that is not one, and that the page is actually
  embedded in the binary — a rename in `web/dist` would otherwise ship a daemon
  that starts and serves nothing.
- **Maintenance from the browser** — the endpoint existed and no client called
  it, so the store tab now offers it, dry run first because deletion is not
  undoable.
- **The CLI reads through a running daemon** — `rookd` holds the store's single
  write lock, so a CLI started while it runs used to refuse. `store stat`,
  `session ls` and `skills ls` now go over its API instead and print the same
  thing; `--json` output is byte-identical either way. The daemon publishes its
  address on start and removes it on either signal, and a file left by a crash
  is ignored because nothing answers there. Commands that write still say
  plainly that the daemon holds the lock
  ([ADR-0006](adr/0006-single-writer-store.md)). 3 tests.
- **Logs that go somewhere and stop growing** — both binaries share one setup:
  stderr and `$ROOK_HOME/logs/rook.log`, at `telemetry.log_level` unless
  `ROOK_LOG` overrides it, rotated once at `max_log_bytes` so the logs cost at
  most twice it. 3 tests.
- **Scheduled maintenance** — `rookd` runs prune, collect and the size budget on
  `storage.maintenance_interval_hours`, which until now was a documented setting
  that did nothing.
- **A bounded skill catalog** — capped by `agent.max_skill_cards`; what does not
  fit is counted rather than hidden, and `load_skill` answers an unknown name
  with the skills that match it.
- **Asides** — `/btw` answers a question from the conversation without tools and
  without joining it, recorded as a note the history replay skips. 2 tests.
- **Conversation continuity** — every turn replays the session log, so `--session`
  and the chat both continue a conversation rather than starting one.
- **A workspace that cannot be left** — file tools resolve through symlinks
  before checking containment, so a link inside the workspace pointing out of it
  is refused rather than followed, and the refusal says where the path really
  led. Widened only by `sandbox.allow_outside_workspace`. 6 tests.
- **A pipe reaches the prompt** — `cargo test 2>&1 | rook run "why?"` is how a
  one-shot turn is usually reached, and the pipe was read by nobody: the model
  answered a question with none of the evidence. Piped text joins the prompt, or
  becomes it when there is no other. Bounded by what the model's window could
  hold, because reading a larger pipe only to be refused for it spends the memory
  and the time both — and the refusal names the window and says to pass a file,
  which is read in pages. 3 tests.
- **One answer to what a turn inherits** — the language-server pool and the MCP
  servers were handed to a new loop by four front ends writing the same three
  lines, and `rook run` had written two of the three: a one-shot turn could not
  ask the type checker anything the chat could. `agent::equip` is the one place
  now, so a part added there reaches all five rather than four. 1 test.
- **Plugins that package skills and servers together** — `SkillSource::Plugin`
  was declared, ranked against the other sources and given a label, and nothing
  ever constructed it: an API advertising a feature that was not there. A
  directory under `~/.rook/plugins` or `<workspace>/.rook/plugins` with a
  `plugin.json` now brings its `skills/` and its `mcpServers` — the ecosystem's
  layout, not Rook's, so a plugin written for another agent works unchanged.
  Servers are namespaced by plugin and run in it; a project's own skill still
  wins over one a plugin ships. 6 tests.
- **Google's own dialect** — reachable through the OpenAI shim, but not fully:
  `generateContent` has two roles and no system one, carries the system prompt
  beside the conversation, makes tool calls and their results parts of a message
  rather than a parallel array, sends a result back inside a *user* turn, and
  gives a call no id to pair a result with. It also reports `STOP` for a turn
  that ended in a tool call, so the calls are what says the turn is not over.
  8 tests against the wire.
- **A language server that is there and does not work** — detection asked
  whether the command was on `PATH`, and rustup installs a `rust-analyzer` shim
  whether or not the component is: `doctor` reported it, then every request
  failed. It starts each one and shuts it down again, which is the only check
  that is not a guess. A server the user disabled was skipped by the agent and
  called broken by doctor; both now ask `lsp::configured`. 3 tests.
- **A failure names its cause, not the url a second time** — `cannot reach
  http://127.0.0.1:11434: error sending request for url
  (http://127.0.0.1:11434/v1/models)` repeated the endpoint, put back the path
  that was deliberately stripped, and never said what went wrong. The innermost
  cause is reported instead — `Connection refused (os error 61)` — because
  refused, timed out and DNS are three different fixes. And when there are no
  skills at all, `doctor` says they are packaged beside the binary and a plain
  `cargo build` does not put them there, rather than reporting zero. 3 tests.
- **The machine is probed when a skill needs it, not when the store opens** —
  `Rook::open` detected sixteen toolchains, `java -version` among them, before
  running anything: about a third of a second warm and over a second cold, on
  every command. `session ls` went from 450 ms to 49 ms; the commands that
  resolve a skill pay it once, where it buys something. 1 test.
- **A server's tools are not trusted because the server says so** — an MCP tool
  fell through to the trait's default risk, which is read-only, and read-only
  returns before the deny list, before `readonly` mode and before every rule: any
  tool any connected server advertised ran unasked. They are now their own risk,
  matched by the namespaced name, and `readOnlyHint` is repeated to the user
  rather than believed. 6 tests.
- **Where a session was cut, without reading it** — the last compaction was found
  by scanning every event, at the start of every turn, and where a fork left its
  parent was legible only inside a title string. Both are now recorded beside the
  session — in the `kv` table, because `SessionMeta` is postcard and a new field
  would make every record already written unreadable — and shown by `session ls`,
  `session context`, the TUI and the web UI. A session written before the position
  existed still reads, and records what it found. Deleting a session takes those
  keys with it: retention deletes on a timer, so anything left behind was an
  accumulator with no bound. 5 tests.
- **Four platforms** — Linux, macOS and Windows tested on hosted runners, FreeBSD
  in a VM; three more targets compiled and two supported best-effort. Each row of
  the matrix names the CI job behind it and a test fails if that job is gone, so
  the table cannot outlive its evidence.

## Next

**Keep triaging the reference backlog.** `cargo xtask refs advance` moves a
pointer and prints what landed; the log in
[references/PORTED.md](../references/PORTED.md) says what was done with each
commit, so a dismissal is a decision. Every pass so far is dated there, and more
of them found a defect here than found something worth porting.

**A live-model smoke test in CI.** An Ollama service container running a small
model, so a turn is exercised against something with judgement. The wire itself
no longer needs it: `crates/rook-core/tests/over_http.rs` runs whole turns
against a server that speaks the OpenAI dialect and checks what it was sent.

## After that

**Talking to a sub-agent while it runs.** Each one now reports when it lands, so
a delegation is no longer silence — but the parent still cannot send anything to
a child mid-run, which is what codex's `spawn_agent`/`wait_agent` is for.

**Auto-installing language servers.** Detection is done; the other half of
codex #8745 asks for installation too, which means downloading and running a
binary on the user's behalf.

**Signed release binaries.** `cargo xtask dist` ships unsigned, so Windows
SmartScreen warns on every download and macOS Gatekeeper needs a right-click to
open. cline signs with Azure Trusted Signing; both need a certificate this
repository does not have.

**Real sandboxing.** Today's boundary is a workspace path check and pattern
rules ([ADR-0009](adr/0009-ask-before-acting.md)) — text matching, not containment.
Platform-native containment — seccomp, Seatbelt, Capsicum, Job Objects — is a
serious piece of work and platform-specific by nature.

**Interactive web UI.** Would trigger revisiting
[ADR-0007](adr/0007-no-js-build-step.md).

## Not planned

- **A hosted service.** Local-first is the point; there is no server to send
  transcripts to, by design.
- **A bespoke skill format.** [ADR-0003](adr/0003-agent-skills-format.md).
- **Telemetry upload.** The config field exists so the answer is discoverable, not
  because it is going to become true.
- **An IDE extension per editor.** ACP is how that gets solved once.
