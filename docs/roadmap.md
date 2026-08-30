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
  list. Every edit to one file goes in one call and a refactor across several in
  one `files`, applied in order against the text as it then stands; a batch that
  cannot finish writes nothing, in any of the files it names. Every cap
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
  write lock, so a CLI started while it runs used to refuse. Every read goes over
  its API instead and prints the same thing; `--json` output is byte-identical
  either way, which is asserted read by read. Getting there removed a second copy
  of each shape rather than adding one: the CLI and the API had each built their
  own JSON for an object listing, a ref and a skill, and had drifted. The daemon
  publishes its address on start and removes it on either signal, and a file left
  by a crash is ignored because nothing answers there. Commands that write still
  say plainly that the daemon holds the lock
  ([ADR-0006](adr/0006-single-writer-store.md)). 4 tests.
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
- **Garbage collection that cannot take a checkpoint in flight** — an object is
  unreachable between being written and the event that names it, and a checkpoint
  writes every captured file before the manifest holding them. The daemon runs
  maintenance on a timer while turns are running, so a sweep could land in that
  window and delete live data whose only fault was being new — and the checkpoint
  would then name an object that was not there, which is the undo failing exactly
  when it is needed. Anything written in the last ten minutes is left alone, and
  the report says how many, so a store that collects nothing explains why.
  1 test.
- **A compaction that fails still moves the session on** — when the summary could
  not be produced, the fallback was a compaction event whose body was a sentence
  rather than a position. Nothing could read a position out of it, so the span
  was never dropped: the next turn compacted again, and the one after that, each
  adding an event and freeing nothing, until the window filled anyway. The record
  now carries the position and a summary saying the span could not be
  summarised and that `session show` reads it back — compaction stops replaying
  events, it never deletes them. A session with nothing worth compacting records
  no compaction at all, where it used to record one and poison the position it
  was meant to describe. 2 tests.
- **A sub-task that says what it is doing, not only that it finished** — a
  delegation reported each child as it landed, so a child running for minutes
  left a counter that did not move, which reads the same as a hang. Each child's
  tool calls reach the parent named by their task. They run concurrently, so the
  reports arrive down a channel and are rendered from the one place that owns
  the callback; the channel carries a tool name per step the children are already
  bounded to. 1 test.
- **A chat pane that does not keep the whole afternoon** — the TUI kept every
  word of every turn for the life of the process, and the browser kept every
  block in the document. Both are scrollback rather than the record: the session
  holds all of it and the sessions view reads it back, so a bounded tail loses
  nothing that is not recoverable. 2 tests.
- **Pinning wins over relevance, not over the budget** — a pinned fact went into
  every request whatever it cost, so the bound on recall was however many facts
  somebody had pinned, which is not a bound. `remember` lets the model pin, so an
  agent pinning freely for a month would spend the window on its own memory
  before reading the prompt: two hundred pinned facts came to 2392 tokens against
  a budget of 100. Pinned facts are taken first and still counted, `memory ls`
  says when they have outgrown the budget, and the tool no longer promises
  "always". 4 tests.
- **What `doctor` could not say about approvals and hooks** — it reported the
  machine, the model and the skills, and nothing about the two things a user
  configures to control the agent. It lists the mode, the rules that will not
  compile, every hook with the event spelling from `config.toml`, and a `match`
  pattern that does not parse — which makes its hook fire on every subject
  rather than none, deliberately, and read as the hook misbehaving. Two Rust
  names that were reaching users are gone with it: `PreTool` for `pre_tool`, and
  the TUI answering an approval with `ForRun`. 3 tests.
- **A boundary that would not compile is not silently absent** — a sandbox rule
  the user mis-spelled was dropped with a line in the log file. In `allow` that
  fails safe: being asked more often is not a hazard. In `deny` it left exactly
  the boundary they had written and did not get, against a README that says
  nothing overrides a denial. A deny rule that does not parse now refuses
  everything that changes the machine, and says which rule and where; reads still
  work, so the agent can open the file and say what is wrong. `doctor` lists
  them. 3 tests.
- **An edit nothing could checkpoint says so** — the loop captures a file before
  every mutating call, and `session rewind` restores from those captures. A file
  past the capture limit got none, the failure went to the log file as a warning,
  and the call went ahead: the model and the user both went on believing the edit
  could be undone. The tool result says it cannot, and the session records it, so
  `session show` and the TUI say it too. The call still runs — refusing every
  edit to a large file would be a worse answer than telling the truth about one.
  1 test.
- **Reading and searching a file without holding it** — both read every file
  whole: `search` to look at it, and `read_file` to return two thousand lines of
  it, so a large file was the caller's problem in memory as well as in context.
  Both read a line at a time now, bounded by the longest line rather than the
  largest file, and the binary check looks at the first buffered chunk instead
  of reading the rest of a file to decide it was not worth reading. A line past a
  megabyte is stepped over, so the numbering still describes the file. 5 tests.
- **A command that writes to both streams does not hang** — stdout was drained
  to end-of-file before stderr was read at all, so a command that filled the
  stderr pipe blocked on the write, never finished stdout, and the drain never
  ended: any build with enough warnings hung until the timeout and then returned
  nothing at all. They are drained together, and a timeout now keeps what was
  printed before it — the part worth reading — and says that `timeout_secs` can
  be raised. The editor's terminal is the other way a command runs, and it threw
  the same output away and said neither thing; both report a timeout through one
  sentence now. 3 tests.
- **A tool call that misses says what it might have meant** — `unknown tool
  "read_fil"` told the model nothing it could act on, so the step that mistyped
  the name cost a second one finding out. The near misses are named, by edit
  distance over the registered names, and nothing is offered when nothing is
  close. `read_file` with `limit: 0` had the same shape: it returned no lines and
  a note to call again from where it stopped, which is where it started — and a
  tool that pages rather than refusing should answer, so a limit that cannot page
  gets the default one. 2 tests.
- **An edit that cannot mean anything is refused** — an empty `old` matches
  between every pair of characters: `replace_all` interleaved the replacement
  through the whole file and reported it as a success, and without it the count
  said "appears 412 times; add surrounding context or set replace_all" — an
  instruction to do the destructive thing. Both are refused, and the refusal
  names `write_file`, which is what replacing a file is for. An edit whose `old`
  and `new` are the same is refused too: a step that changed nothing must not
  read as progress. 2 tests.
- **A session named after what was asked of it** — each front end had a
  placeholder of its own — `chat`, `tui`, `web`, `acp <cwd>` — and two of the
  four already took the first line of the prompt instead, by writing out the
  same expression. Twenty sessions called `chat` is a list you have to open one
  at a time. Sessions start unnamed and the loop names them from the first
  prompt, which no front end can forget and a name the user chose survives.
  1 test.
- **A session list scoped to the project it belongs to** — `session ls` printed
  every session on the machine, which for anyone standing in a project is mostly
  someone else's work; it lists this workspace and says how many it left out and
  how to see them (`--all`). The TUI's sessions tab named no workspace at all, so
  another project's session read as one of this one's: it names the project
  instead of the model, which is the same for every row anyway. 2 tests.
- **Picking up where you left off** — continuing meant knowing a session id,
  twenty-six characters of base32 nobody remembers, so the usual way back to
  yesterday's work was to list every session and read. `--session last` is the
  most recent one in *this* workspace; another project's would carry its whole
  conversation into this one. Sessions of the same second used to come back in
  whichever order the index held them, so the listing every front end reads now
  breaks the tie by id, which is time-ordered anyway. Every command that takes a
  session takes it — `show`, `context`, `diff`, `fork`, `rewind`, `rm`, `goal` —
  because a vocabulary understood in two places out of nine is a worse answer
  than none. 4 tests.
- **A turn can be stopped from every front end** — the chat REPL cancelled on
  ctrl-c and the browser had a button, and the TUI could only be killed, taking
  the browsing state and any approval granted for the run with it. Ctrl-c stops
  the turn there now and quits only when there is none, the interruption is
  logged so the session says where it ends, and the pty test drives it against a
  socket that accepts and never answers. 1 test.
- **The storage claim measured on a store that exercises it** — the benchmark
  behind the README's ratios read 25 distinct files, and a dictionary needs 32
  samples of a kind before it is trained: the file blobs never got one, so the
  headline measured the message dictionary alone while the claim beside it is a
  dictionary per kind. At 64 files both are trained. End-to-end barely moved,
  20.5× to 21.9×; the split between dedup and compression did, and the numbers in
  `README.md`, `docs/storage.md` and `docs/platforms.md` follow the run. 1 test.
- **What a turn is spending, while it is spending it** — the totals were
  reported once, at the end, when the number can no longer change a decision;
  a turn running a dozen steps over several minutes said nothing. They are
  emitted after every reply and shown where there is somewhere to put them: the
  TUI footer beside the mode, and the browser beside the settings. The chat REPL
  and `rook run` stream into a scrolling terminal that has no status line, so
  they keep the summary at the end — a rendering decision, not a missing
  capability, and ACP has no slot for it in the protocol. Adapted from codex
  #41087. 3 tests.
- **A question to a person cannot wait forever, or lose its answer** — how long
  to wait was a number written out four times, twice as 300 seconds and twice as
  600, and configurable nowhere; it is `[agent] answer_timeout_secs` now. Over
  ACP there was no bound at all: an editor that showed the dialog and never
  answered held the turn, its language servers and the store's write lock for
  the life of the connection, and the unanswered request stayed in the map
  beside them. Both sides also resolved answers under `try_lock` and dropped the
  answer when it failed — rare, and not reproducible on demand, which is why the
  branch is gone rather than guarded. 3 tests.
- **A turn a script can read** — `rook run` is the command the README calls
  "for scripts" and the only one that ignored `--json`, so a caller had to parse
  a stream meant for a person. It emits one object: the reply, why the turn
  ended, steps, tokens, the tools called and what changed on disk. Two tests
  drive the binary through a whole turn against a socket, which nothing else in
  the CLI suite did — every other test stopped where a model would be needed.
  Why a turn ended had three spellings, `EndTurn` among them; it has one now.
  A turn that ran out of steps exited 0, so a script could not tell half-done
  work from finished: it exits 2 and says which limit and what to change. The
  code is decided inside the runtime and taken after the store is dropped, since
  exiting from within would skip closing it. 5 tests.
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

## Measured against a live model

Everything below was found by running Rook against a local 27B model in LM
Studio, which is the first time it has answered anything but a scripted provider
or a socket. The wire was already right; what was not was what a refusal reads
like to something that has to act on it.

- **A refusal a model cannot act on is one it works around** — told nothing could
  approve a write, the model tried another tool, then delegated the same task to
  a sub-agent, which is refused for the same reason. Nine steps, 32,185 input
  tokens and four minutes and twenty-two seconds, ending at the output cap. Every
  remedy the message offered — re-run interactively, pass `--yes`, add an allow
  rule — is something only the person can do. It now tells the model to stop and
  say what it was about to do, says that no other tool and no sub-agent gets past
  it, and addresses the remedies to the user. The same task: four steps, 8,498
  tokens, twenty-three seconds, and an answer worth reading. 1 test.
- **Two ways to get a skill that is not here** — writing one by hand was the
  only one. Now: the agent writes the tool and the instructions together
  (`write_skill` takes `files`, a shebang makes one runnable, `../` is refused),
  or it searches the configured sources and installs — `rook skills search` /
  `install`, and `find_skill` for the agent, where searching reads and
  installing is approved like any write. A source is a git repository or a
  directory with `SKILL.md` files in it: no index, no API. Nothing is fetched
  until one of those runs.
  Neither loads anything into a request: a `pdf` skill of 1,900 tokens across
  five files costs sixty as a card until `load_skill` asks for it. Ranking
  reuses `memory::terms_of`, because scoring on "a" and "the" ranks whichever
  description is longest — measured, on the first test written. 9 tests.
- **An MCP server that dies is restarted, a few times** — a subprocess that
  crashed took its tools out for the rest of the run: every later call returned
  the same transport error and nothing tried again. A transport failure — the
  pipe, not the server — now gets one restart and one retry per call, capped at
  three per server per run so one that dies on every call is not respawned on
  every call. An rpc error or an unparseable answer is the server working, and
  restarting it would only throw the answer away. Named in the seventeenth
  triage pass as considered and not done; hermes had reached it from two
  directions. 2 tests.
- **The tool list is priced whole, and the daemon tests build what they need** —
  the schema budget and the tool that prints it both lived in `rook-tools`,
  which cannot see the six tools the loop adds: they guarded 729 tokens of a
  list that costs 1,476, and the two largest entries were the ones they could
  not see. Both moved to `rook-core`. Three model-facing descriptions were also
  carrying runs of source indentation as literal spaces, which is 44 tokens a
  call on `delegate` alone.
  A clean-tree run then found the CLI's daemon tests assuming `rookd` was
  already linked — true of every incremental run, false of the clean build CI
  does every time. They build it if it is missing. 1 test moved, 1 fixed.
- **A turn stops compacting once it stops helping** — a span too small to
  summarise leaves the context exactly as full as it was, so the check at the
  top of the next step is true again, and the step after that: seven
  summarisation calls in one turn to stand still. It compacts once per turn that
  it achieves something. Converges with codex #41152, which fails closed on
  unbounded parent compactions. 1 test.
- **The pool a front end hands over is the one that answers** — `AgentLoop::new`
  built its own language-server pool from a third copy of the "configured or
  detected" expression, and registered the tools from it. `equip` then set the
  loop's pool and re-registered — but the tools already there held the pool they
  were made with, so what a front end handed over never answered. A loop is
  rebuilt every turn, which is precisely why it should not build this. It starts
  with none now.
  Once that was true, filtering bit: only servers with files they handle in this
  workspace are offered, so a Python project is not given rust-analyzer's four
  tool schemas — 1,401 input tokens for a question that cost 3,450.
  A sweep for the other expressions unified today found one more survivor:
  `rook lsp` built the same list its own way, under a comment claiming it could
  not drift from what a turn sees. It asks the same question now, and refuses
  with the reason none applies; `doctor` marks a server that starts but has no
  files here, since installed and working is not the same question as used.
  5 tests.
- **A language server that will not start is found out once** — a live turn in a
  Python project asked about a symbol, and the pool always offered the first
  configured server: a `rust-analyzer` shim, which rustup installs whether or
  not the component is. It tries each in turn now, and remembers which would not
  start, so a turn that asks three questions waits for one failure rather than
  three. 2 tests.
- **Context the parent writes out reaches the sub-task** — `delegate`'s `context`
  was an enum of two words, and a live model filled it with the file it had just
  read, expecting the child to start with it. The value was outside the enum, so
  it was dropped and the child read the same file the parent had already paid to
  read. Anything that is not one of the two words is now handed over verbatim,
  which is where a parent's working knowledge belongs. 2 tests.
- **Every sub-task ran twice** — `delegate` took `task` and `tasks` together, and
  a live model filled both fields of every call with the same instruction,
  differing only in whether the function name wore backticks. Three files became
  six sub-agents: twice the tokens and twice the wait, with one of each pair
  thrown away, and nothing said so. They are alternatives now, not a union.
  Judging sameness by meaning was measured and rejected: `memory::overlap` scores
  those two spellings 1.00 and two genuinely different sub-tasks — `a.py` against
  `b.py` — 0.94, against a threshold of 0.95, and a hundredth of a point between
  "one task said twice" and "two files to check" is not a distinction to spend
  real work on. The same request afterwards: 20,040 input tokens against 33,183,
  and ninety seconds against two minutes ten. 3 tests.
- **`session context` measured against a window nobody had** — it defaulted to
  128,000 tokens whatever model was configured, so a session at 55% of a 6k
  window read as 1% — the difference between "about to compact" and "nothing to
  think about". It uses the configured model's own window, and `--window` still
  asks about another. Found while watching compaction run against a live model.
  1 test.
- **A turn says what it changed about what it believes** — the live run that
  found the above also showed an agent deleting a fact the user had asked it to
  remember, and saying nothing about it in any front end. `facts_learned` was
  recorded on the outcome and read by nobody; there was no `facts_forgotten` at
  all. Both are reported now, beside what the turn changed on disk, in the CLI,
  the chat, the TUI and the browser — an agent quietly dropping what it was told
  to remember is the same failure as one quietly remembering something nobody
  can see. 1 test.
- **"What changed today" was a diff and is a story** — told to remember
  something, the model recalled it in the next session, decided it did not match
  the workspace and forgot it. `memory since 1` then answered that nothing had
  been learned or forgotten all day: a fact learned and forgotten between the
  ends of a window cancels out of a diff of those ends. Every recorded state in
  the window is folded in turn now, in the order it happened. The baseline was
  also the oldest state before the window rather than the newest, so a long
  history reported more than the window held. 3 tests.
- **A sub-task named by its whole instruction** — the progress line printed the
  entire delegated prompt on every step, two hundred characters of it, burying
  the step it was reporting.

## Asked for, in order

Seven things, sequenced by how much of the answer is already here. Each is
recorded so the design question survives the session that raised it.

1. **Rook as an MCP server** — *done*. `rook mcp serve` offers the file tools,
   the search and the command runner over stdio to anything that speaks the
   protocol, with the approval policy in front of every call and the unattended
   approver refusing a write rather than deciding for the user.
2. **Consecutive user turns folded into one** — *done*. The agent produces them
   honestly and a chat template on a self-hosted server often will not take
   them; `Request::new` folds them, in the one place every request goes through.
3. **Verification as a mechanism, not a habit** — *done for what can be run*.
   `verify` hands a claim to a session that did not make it, with every tool
   that changes something withheld — the writing tools taken out of the box, and
   the loop's own six (a skill, memory, `delegate`) neither advertised nor
   dispatched, because a checker that can start an agent with the writing tools
   has not been stopped from writing. It must end with `VERDICT: holds`, `fails`
   or `unproven`; anything else is reported as unchecked rather than passed,
   since "looks reasonable" is what a model says when it has read something and
   run nothing.

   It is not isolation — `run_command` can still write, and closing that is the
   sandbox below.

   The other half landed with (4): a claim whose evidence is a source rather than
   a command. The criterion is attribution — find where it is said, quote it with
   its address, and separate what a page states from what its writer argues,
   since two sources copying one another are one source. What makes that a
   mechanism rather than an instruction is the rule beneath it: a verdict from a
   checker that ran nothing and read nothing is reported as unproven whatever it
   said. Reaching for nothing is the shape a fabricated check takes, and it is
   also what asking a second agent was supposed to get past.
4. **Reaching the web** — *done*. Off unless
   `[web] enabled`, and off means the model is never shown the tool rather than
   the call being refused: a tool it cannot see is one it cannot decide to try.
   It reports `Risk::Network`, whose subject is the address, so an allow rule can
   name a host and mean it. HTML comes back as prose through a deliberately crude
   stripper — what a model needs off a page is the writing, and a real parser
   costs more than the rest of this binary.

   Search landed with both engines and no default: SearxNG for the case where
   the query does not leave the machine, Brave for the case where somebody has a
   key. Its risk is the engine's address, so allowing a local instance does not
   allow a hosted one, and an engine named without its key is offered as nothing
   rather than as a tool that fails once and teaches the model to stop asking.

   The tension with local-first stands and is the reason for the default. What
   comes back is somebody else's text arriving in the model's context: not a
   fact, not an instruction, and the input to (3) rather than an answer.
5. **Asking what a crate offers** — *done, and without the network after all*.
   The guess that it would need (4) was wrong. Cargo unpacks every dependency
   under its registry and `Cargo.lock` says which version this project resolved
   to, so the source is already on the machine: `crate_api` reads it. No rustdoc
   JSON, which would be the right answer and is nightly-only, and no docs.rs
   round trip for something already here.

   The scanner is not a parser, the same trade as the HTML one — what is wanted
   is the signature line. What it does not see is anything a macro generates, and
   a re-export reads as absent because the item is declared elsewhere.
6. **Several projects at once** — *done through the daemon*. The blocker was
   never the workspace but the store: one per `ROOK_HOME`, one writer, and bound
   to a workspace that is one per project — so a second project was a second
   process, and the second process was the one that could not open the store.
   The store is now shared (`Arc<Store>`) and `Rook::for_workspace` builds a
   sibling looking at another project, rediscovering the skills and plugins that
   are the workspace's own and sharing everything else. `rookd` keeps one engine
   per project and a chat connection names its own with `?workspace=`, so
   several projects run at once against one history, one memory and one search.

   What is not done is the CLI: `rook run` in a second directory still opens the
   store directly and still meets the lock. Routing it through a running daemon
   is the rest of [ADR-0006](adr/0006-single-writer-store.md), and it is the
   larger half — every command needs a client path as well as a direct one.
7. **Several agents in one project** — *done for the simultaneous write*. Two
   connections naming the same workspace share one engine and run concurrently.
   A mutating call now claims the paths it is about to write, for as long as the
   call takes — the checkpoint already resolved them, so it costs nothing to ask
   — and a second turn reaching for one of them is refused and told which
   session holds it. Refused rather than queued: the other turn is mid-write,
   and what this one wants is to be told so it can do something else.

   `edit_file` was already safe on its own: it replaces exact text, and text
   another turn has changed is not there to replace. `write_file` was the one
   that overwrote whole.

   A claim is released on return, on a panic unwinding out of the call, and when
   the turn holding it is aborted. None of those covers a call that never returns
   — `run_command` takes its timeout from the model — so a claim expires as well,
   and `/api/writing` says what is held and for how long. In release the profile
   aborts on panic, which skips every destructor; that costs nothing here because
   the registry is in the process that died.

   The slower race is closed too, and by a smaller thing than the hashes that
   were sketched here. What matters is not what a file contained but who looked
   at it last: an overwrite by a turn that is not the last to have seen a file is
   a turn writing over something it never read. So a read records the reader, a
   write records the writer, and `write_file` — the only tool that replaces a
   file whole — is refused when somebody else looked last, with `edit_file`
   offered instead. A session working alone is always the last to have looked, so
   it never meets the rule, which is what keeps a rule from being switched off.
8. **Secrets the agent can use and cannot leak.** At the concept stage. The
   shape that fits: a secret is named, never valued, in everything the model
   sees — it asks for `deploy_token` and the substitution happens at the edge,
   in the tool, on its way out. That keeps it out of the transcript, out of
   compaction, out of what a sub-agent inherits, and out of any provider
   request. What it needs settled first is where the values live (the store is
   readable by design, which is the wrong property here) and what stops a tool
   from being asked to print one.

- **A refactor that lands whole or not at all** — `edit_file` took one file, so
  renaming a symbol across five was five calls and a failure on the third left
  two already changed. `files` takes them together: every file is worked out
  before any is written, which is the rule the tool already kept within one file,
  dropped at the file boundary. The approval shows every diff and the checkpoint
  captures every path, because both ask the tool what it would touch. The
  advertised list went past its budget and two descriptions were trimmed before
  the number was raised, with what bought the increase recorded beside it.
  2 tests.
- **Steering a turn instead of stopping it** — what you typed while a turn ran
  was taken from the input box and dropped on the floor, so watching one head the
  wrong way left nothing to do but kill it and start again, losing everything it
  had done in order to say one sentence. It now reaches the model at the next
  step, which is the one place a user message may go: between an assistant's tool
  call and its result, no dialect accepts one. The TUI and the browser, being the
  front ends that can take input while a turn runs; the plain REPL is blocked on
  its own readline and says so by not offering it. And said while the model is
  writing its last answer, the turn goes on rather than ending on the model's
  word — otherwise it would sit in the queue until the next prompt and be folded
  into it, which is not where the person put it. 2 tests.
- **Commands that keep running** — `run_command` waits, caps and kills at a
  timeout, so a dev server or a watcher could only be run by not running it.
  `background: true` answers at once with an id and the `job` tool reads or stops
  it. The registry is the front end's, like the language-server pool, and takes
  its processes with it when it goes. Bounded three ways: how many run at once,
  how much each keeps, and how many finished ones are held for their output.
  Adding it put the advertised tool list fifteen tokens over
  `the_whole_advertised_tool_list_stays_within_a_budget`, which was the guard
  doing its job; two descriptions were trimmed rather than the number raised.
  `/jobs` shows the same list without spending a turn on it, and `wait_secs`
  turns polling into one wait — three suites started together are four tool
  calls, not one per check — capped at what a foreground command would have been
  given. 8 tests.
- **Seeing what an approval is for** — a write asked about by path is a write
  approved blind, which is most of the value of asking gone. `Tool::preview`
  gives the diff a write or an edit would make, built by applying the very edits
  the call would apply to a copy nothing writes, and shown in all four front
  ends: indented under the terminal prompt, coloured in the TUI panel, in the
  browser's dialog, and as a content block on the ACP permission request. Built
  at the moment of asking, since a hook can turn an allowed call into an asked
  one. 4 tests.
- **Standing instructions from `AGENTS.md`** — the convention codex, opencode
  and others already read, and this agent read none of them: a project's
  conventions had to be repeated every turn or hidden in a skill, which is
  loaded on demand and so is not standing instruction. `$ROOK_HOME/AGENTS.md`
  then the workspace's, most general first, each bounded by
  `[agent] max_instructions_bytes` and each saying when it was cut. Writing it
  found `floor_char_boundary` indexing one byte past the end of its input for
  any caller whose limit exceeded its data — nothing had one until now, and in
  release that is an abort. 3 tests.

- **The middle of a very large output** — `Ends` holds a head and a tail and
  discards what is between them as it streams, which is what makes a runaway
  command cost bounded memory and what made the four hundredth line of two
  thousand unreachable. The whole of it now goes to a file under `$ROOK_HOME`
  as it arrives — one file for both streams, in the order a terminal would have
  shown them — and the reply names it, so the model reaches the middle with the
  shell it already has rather than with a new tool. Only when something was
  actually left out: naming a file that holds what is already on screen sends it
  to read something it has. Bounded twice over, because the answer to a memory
  budget cannot be an unbounded disk one: `[sandbox] max_spill_bytes` caps each
  file and says when it stopped, and `store maintain` keeps the newest
  `[sandbox] max_output_files`. A timed-out command names it too — that is the
  run whose output is most worth having and the ends of it are the least of it.
  6 tests.

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

**Driving physical devices, if the Model Hardware Standard becomes something to
build against.** Anthropic's [research
preview](https://www.anthropic.com/news/model-hardware-standard-research-preview)
describes a shared way for an agent to operate lab and manufacturing hardware:
devices expose *states* and *procedures* behind a standard driver, reachable
over MCP, a command line, or a code API.

Most of the plumbing is already here and needs nothing. MHS names MCP as one of
its three interfaces, and `rook-mcp` speaks it over stdio and streamable HTTP,
namespaces each server's tools, restarts one that dies and puts every call
through the approval policy. A device that ships an MCP server is a device this
can drive today, without a line of new code.

What is not here is the part that matters, and it is not plumbing.
[ADR-0009](adr/0009-ask-before-acting.md) rests on two things: ask before
acting, and undo afterwards. `Rook::rewind` restores files from content-addressed
captures — **a pipette cannot be rewound.** `Risk::External` already says the
right thing about an MCP tool ("`readOnlyHint` is the claim of the very party
whose behaviour is in question"), and for a device that claim is about a robot
arm. So a physical procedure is a risk this codebase has no vocabulary for: not
`Write`, which is undoable, and not `Execute`, which is bounded by a timeout and
a workspace. Deciding what it *is* — and what a deny rule for it anchors to,
since [the shell rules anchor to command position](../references/PORTED.md) —
is the work, and it is worth doing carefully rather than early.

The skills half is a better fit than it looks. MHS generates reference files
describing a device's capabilities and its safety limits, which is a `SKILL.md`
with `requires:` almost exactly: the extension already scopes a skill by OS,
arch and tool version, and [ADR-0003](adr/0003-agent-skills-format.md) says
where a new predicate goes. A device predicate would let one skill carry the
procedure for the instrument in front of it and be invisible everywhere else.

Blocked, and honestly so: there is no public specification, no SDK and no
repository — the announcement links an application portal and nothing else — and
preview access is limited to scientific research labs and advanced
manufacturers, which this is not. Implementing against a description of a
standard is how you get a second, wrong one. Revisit when the specification is
published; until then the entry exists so the design question is recorded rather
than rediscovered.

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
