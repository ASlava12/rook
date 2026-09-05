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
  with counts, binary reported without one, and a file past what is worth
  rendering answered by its hash — which is the id its capture is stored under,
  so whether it changed is decided without either copy being in memory. 5 tests.
- **Rewind and fork** — `rook session rewind` restores workspace files as well as
  the conversation, deleting files the turn created, and forks rather than
  truncating so the rewound-past turns stay readable.
- **Context visibility** — `rook session context` breaks the cost down by kind and
  separates what a fresh turn would carry from what is merely stored.
- **Tools** — paged `read_file`, `write_file`, batched and unambiguous `edit_file`,
  `list_dir`, regex `search`, `run_command` with timeout, output cap and deny
  list. Every edit to one file goes in one call and a refactor across several in
  one `files`, applied in order against the text as it then stands; a batch that
  cannot finish writes nothing, in any of the files it names. `search` caps the
  looking as well as the hits — `[sandbox] max_files_searched` — because a walk
  cannot tell a workspace from a home directory until it is in one. Every cap
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
  and to add context the model sees, bounded because that context is carried for
  the rest of the session. A failing `pre_tool` hook blocks; no hook
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
- **A daemon that can be looked at and stopped** — `rookd` is started by opening
  a window, and stopping it meant `pgrep` and `kill`: a process id to find for a
  program nothing had told you about. `rook daemon status` says where it is
  answering, how long it has been up and how many turns it is running; `stop`
  names the turns it would interrupt rather than dropping them, and takes
  `--force` for the decision made with that number in front of you; `restart`
  waits for the address file to go before starting the next one. And because an
  upgrade leaves the running daemon on the old code — every window keeps
  working, at the previous version — the health it already asks for on the way
  in now says whether the `rookd` on disk has changed since that process
  started, and a window that finds one says so in a line. A restart comes back
  on the port it left, because every window read the address once, when it
  attached, and a daemon that returns somewhere else has stranded all of them —
  which needed the daemon to release the store's lock *before* removing its
  address file. That ordering was already wrong for everyone else: the file
  says "there is a daemon", so a window seeing none opens the store itself, and
  a daemon still holding it met that window with the very error the file exists
  to prevent. 5 tests.
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
- **What the model worked out, carried back to it** — reasoning was logged and
  never replayed: the model was handed its own answers and its own calls and
  nothing it had worked out, so it worked it out again at every step, and a
  turn resumed tomorrow began from a blank mind. It is carried now, on the
  message it led to — two assistant messages in a row are not a conversation
  any dialect accepts — and bounded by `[agent] max_reasoning_tokens`, 800 by
  default: past that the middle goes and the marker says how many tokens went,
  because a thought's subject is its first lines and its conclusion is its
  last. Anthropic's signed blocks are left alone; a second copy as text would
  be the same thought twice. The bound is what lets `session context` price a
  thought from its stored size without reading it. 2 tests.
- **A turn says what it wrote** — "как будто не пишет на диск агент", asked two
  and a half hours into a turn that had indeed written nothing. What a turn
  changed on disk is the one thing that tells a working turn from a stuck one,
  and it was in none of what a turn reported: the CLI, the TUI, the browser and
  an editor over ACP all said steps and tokens. `files_changed` is collected
  where the writing happens — a tool that declares its paths and a command
  whose writes are discovered afterwards both land in one place — and named in
  all four, to a bound: five files by name, the rest counted.
- **A write answered with what it broke** — a model has to think to ask
  `diagnostics`; it never has to think to read the answer to the call it just
  made. A real turn broke an indent with `sed -i` and spent three steps, and
  three identical `py_compile` runs, finding out — with a language server
  running beside it that knew at once. A call that writes now carries the
  errors the server reports afterwards, minus the ones the file already had,
  which are somebody else's news and on every write are noise. It never starts
  a server: a cold `rust-analyzer` is tens of seconds of indexing and nobody
  asked it anything, so the write that finds one cold warms it in the
  background and the writes after that are answered. Bounded to three files and
  five problems, because a refactor across forty files must not answer with
  forty analyses. 1 test, against a mock server driven by the text it is given.
- **Arguments read as what they plainly are** — a real turn spent four steps
  on one call: `edit_file` handed `edits` as a *string* of JSON, and inside it
  the `files` shape rather than a list of edits. Both are now read — a field
  the tool's own schema declares an array or an object, arriving as a string
  that parses to one, is the value it parses to; and a list of `{path, edits}`
  wherever it turns up is a list of files with their edits. Against the schema
  and never blindly: `write_file` takes a file's contents as a string, and a
  JSON document on its way to disk arrives as the text it is. 2 tests.
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
  Writing followed reading: the tab that lists what the agent believes now
  corrects it where it is read — `a` adds a fact here, `A` everywhere, `d`
  forgets the selected one and `u` puts it back with its tags and its pinning,
  because the row a keystroke lands on is not always the one that was meant.
  The browser gained the same add, so all three front ends now do what the
  command line always could. 5 tests.
- **Fetching a language server this machine does not have** — `rook lsp install
  rust-analyzer` takes the latest release, checks the bytes as they arrive
  against the digest the release API lists for that asset, unpacks the one
  gzipped binary, and puts it under the state directory, where `lsp` looks
  before `PATH` and where removing the directory undoes all of it. What was
  checked and what was not is printed, in words: the download is intact; the
  release was not reviewed, and the digest comes from the same publisher as the
  file. Three shapes: a GitHub release checked byte for byte against the digest
  it lists (rust-analyzer); npm under a prefix of ours with install scripts off,
  where npm checks each tarball against the registry's integrity hash
  (typescript-language-server, pyright); and a build from source by the Go
  toolchain, which checks the module against the checksum database (gopls).
  Each says in words what that did and did not check. Zips open too — clangd
  ships one with its `lib/clang` beside the binary, which is kept whole, and
  rust-analyzer's Windows build is one — bounded on what they inflate to and
  refusing an entry that names a path outside the archive. The agent notices too:
  a language with files here and no server is offered once per session, and the
  stance decides what follows — at `assist` a person chooses between the state
  directory, the machine's own installer and not now, or with nobody there it is
  an open question; `autonomous` fetches into the state directory; `free` runs
  the machine's own installer and falls back to fetching. What is installed
  serves from the next session, because the pool of servers is built before the
  first turn — the same fact that keeps rust-analyzer from re-indexing every
  turn — and the report says so.
- **What a run has to tell the person, sorted by what it is** — a refusal
  nobody made is a different thing from one somebody made, and the end of a run
  one was not watching has to say which. `TurnOutcome` carries `decisions` (a
  person declined, a stance was granted) apart from `open_questions` (nobody was
  there to approve, a goal check could not settle), and all three front ends
  read them: `rook run` prints both, the browser lists them under the turn, an
  editor gets them as the last thing said. `Approval::Unanswered` is what makes
  the two tellable apart at the source. And the agent may ask for more latitude
  — the `stance` tool goes through the approval policy like anything else, so a
  person grants it for the rest of the run, a deny rule can forbid it outright,
  and nobody being there leaves it an open question; a stance is only ever
  asked up, never taken.
- **An autonomous turn is checked against its goal before it may end** —
  autonomy is a task and its boundaries, and the boundary has to be held by
  something other than the turn's own opinion of its work. With a goal set, a
  checker in a fresh session is asked two things before an autonomous turn that
  did anything ends: is the goal met, and was anything the person asked not to
  do done anyway. `fails` gives the turn one more go with the reason, so it puts
  it right and says what was wrong; both outcomes are on the record as a note.
  Finding it turned up a false alarm in the write fence: a checker's reading
  was recorded as touching the file, so the turn being checked was then refused
  the fix the check had just asked for. A loop with no writing tools records
  nothing now.
- **One idea where there were two** — an approval mode and a level of autonomy
  are the same question asked twice, so `Mode` became `Stance`: `readonly`,
  `assist`, `autonomous`. It is ordered, and the order carries weight — a
  sub-agent inherits the stance of the turn that started it and is never given
  more. The model is told what its stance means rather than learning it one
  refusal at a time: at `assist` a real fork goes to the person instead of being
  settled alone. `mode`, `ask` and `auto` are still read, through serde as well
  as by hand — the config is not parsed by the function that accepts them, and a
  `mode = "ask"` that stops deserializing takes the whole config with it.
- **A turn goes on while the sub-agents it started are running** — `delegate`
  waited, so by the time the parent could speak its children had finished, and a
  child going the wrong way went there to the end. `wait: false` answers at once
  and leaves them running; `subagents` says where each got to, passes one a
  remark it sees at its next step, and hands back their results. They advance
  during the parent's own model call, which is the long wait in a step. A turn
  does not end with work still out: anything the model did not collect is waited
  for and appended rather than dropped at the door.
- **A reply that changed writing system is asked for again** — small and
  quantised multilingual models sometimes finish a sentence in a script nobody
  used: a Russian answer with a Han word in the middle of it. The text cannot be
  repaired locally, because only the model knows what it meant, so the turn asks
  once for the whole answer again and names both scripts. Latin is never a slip
  — identifiers and command lines are Latin inside prose of every script — and
  anything in backticks is exempt, because a CJK string in a file the agent read
  is not the model changing language. `[agent] one_script = false` turns it off
  for work that mixes scripts on purpose.
- **Tools for an endpoint that cannot carry them** — llama.cpp's server and a
  few gateways refuse a request with `tools` in it at all, and
  `Provider::supports_tools` said the agent fell back to describing them in the
  prompt. Nothing read it and there was no such fallback: it survived the
  dead-API tests only because `Retrying` forwards it. Now `[agent]
  native_tools = false` puts the tools in the system block and reads a written
  call back out of the reply — parsed as JSON wherever it sits, because a
  scanner for `"tool":` finds one in the tool list and in the model explaining
  itself. Nothing above the provider can tell which kind of endpoint answered.
- **Background commands visible outside the terminal** — the chat REPL and the
  TUI have had `/jobs` since there were jobs; a browser could start a dev server
  through the agent and then neither see it nor stop it. A Jobs tab over a new
  `/api/jobs` lists them, reads one's output and stops it. The registry moved
  with it: it was built per websocket, so reloading the page killed every
  command the agent had left running, and re-indexed every language server on
  the way. It belongs to the project now.
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
  by a crash is ignored because nothing answers there. Writes route too, now all
  of them, and `doctor` and `models` stopped needing a store to answer at all
  ([ADR-0006](adr/0006-single-writer-store.md)). 7 tests.
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

   OpenClaw has shipped the second half of that and it is worth copying: a
   secret is bound to the exact destination hosts it may be substituted into,
   and an unbound one **fails closed** rather than going out in plaintext. That
   turns "what stops a tool from printing it" from a question about tools into
   one about destinations, which is a much smaller surface — `fetch` and the MCP
   client are the only two here that leave the machine, and `run_command` would
   simply never be given a value to print.

- **Deleting a file inside undo** — the loop checkpoints what a tool says it
  will touch, and `run_command` says nothing, so `rm` was the one change to a
  file that no rewind could reverse. Every other thing a command does leaves the
  content somewhere; a deletion leaves nothing. `delete_file` takes one file,
  shows what would go before it goes, and is captured like any other write.
  Adding it put the stubs — the number actually paid, since lazy loading is the
  default — two tokens over, and `write_skill`'s first sentence was the one worth
  shortening. 2 tests.
- **Surviving a model's own mistakes** — the closest thing to a live model this
  has: what happens when the answer is malformed rather than scripted-perfect.
  Arguments that will not parse are told apart from arguments that are absent,
  and refused before anyone is asked to approve running the empty string. Two
  calls given one id are replayed as two results carrying it, which every dialect
  rejects — so the model's mistake used to come back as an opaque provider error
  after the work had been done twice; the repeat is dropped from the message that
  asked as well as from the answers, and the result says which call went and why.
  A turn the model ended without saying anything reports that instead of nothing,
  since silence with no error is what every front end renders as a hang.
  And a verdict is read however the model dressed the line — bold, a bullet, a
  different case — because a check that ran the build and read the code is not
  "unchecked" for having written `**VERDICT: holds**`; a fourth word still is
  not a verdict, since reporting a hedge as one is what asking for three exists
  to prevent. 7 tests.
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
  its own readline and says so by not offering it. A remark typed while a
  delegation is out reaches every running sub-task rather than waiting for all of
  them to land. And said while the model is
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
- **A repository cannot start a process by being cloned** — a plugin vendored
  into a workspace declared `mcpServers`, and those were connected at session
  start along with the user's own: before a prompt, before an approval, before
  anybody had typed anything. Opening Rook in a cloned repository ran whatever it
  chose to declare. Its skills still load, because a skill is text; its servers
  do not, and each one is named on start with the line that would enable it. 2
  tests.
- **Deciding out loud** — the shipped `decision-matrix` skill, for a fork that is
  not obvious: the options, the requirements that eliminate one before it is
  scored, five to eight independent criteria weighted to 1.0, a scale whose
  middle is defined, and the check that matters most — move each weight by a
  fifth and see whether the winner moves, because a leader that does not survive
  that is resting on the weights and not on evidence. It ends at `ask`, where the
  person picks one or writes something else, and an answer that is not on the
  list is the most useful thing it can produce: it means the fork was framed
  wrongly. A skill rather than a tool for the reason
  [ADR-0010](adr/0010-no-todo-tool.md) gives about planning — this is a
  discipline, and a discipline needs no schema on every request.
- **A pause the conversation can see** — a session resumed a week later replayed
  as though it had paused for a moment, and what to do next often depends on
  which it was: "have you already run the tests" has a different answer across a
  week. A gap of an hour or more is marked where it happened. Marked rather than
  stamped per message, which is what OpenClaw does and what a hundred-message
  session cannot afford — a timestamp on every line pays tokens on every request
  to answer a question nobody asks except across a gap. 2 tests.
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

**The smoke job's model.** `cargo xtask smoke` now runs in CI against a
small model in a service container, and is deliberately not a gate: a model
that size fails for reasons that are not ours, and a job that goes red on a
coin-flip teaches everyone to stop reading it. Its first reading paid: of
three failures, two were ours — rustup's `rust-analyzer` shim was offered as a
server on the strength of the file, and `edit_file` told a model that had
written `path` three times that `path` was missing — and one was the model's,
a checker that narrated what it would run and stopped. All three are
answered: a server is probed before it is offered, the message names the shape
that was wrong, and a checker that ends without a verdict is asked once, in its
own session, to do what it described and commit. The second reading found
two more. A checker's `holds` was discounted for reaching for nothing, and the
report said so in its first line and quoted the `VERDICT: holds` in its last —
which is the line a small model reads, and the parent answered "verified as
true based on the recollection". The report now ends with the ruling and the
quoted line is gone. And every scenario began by downloading rust-analyzer:
the smoke runs autonomous in a Rust workspace with no server, which is exactly
when the offer fetches one. `[agent] install_servers = false` exists now, for
the runner and for anyone who would rather choose their own. What is left is
watching how often it is red and why, and moving to a larger tag if the small
one cannot carry the scenarios. The third reading settled that: all
four failed on the model's own account — a step narrated and not taken, a
`sed` for a port it never read, `quiet-heron-4417` reported as "token", and
a discounted recollection restated as true — so the tag is `qwen2.5-coder:3b`
now. Its first reading was the best one yet: it answered every scenario with a
tool call — written as text, `{"name": "read_file", "arguments": {...}}` as the
whole reply, with the tools offered natively — and the turn ended with nothing
called. The reading the prompt-tools mode already did of such an object is
done either way now, for an object that names a tool that was offered; JSON a
model was asked for names none. The call it wrote for the edit would then have
been refused twice over — `from`/`to` for the two strings, and a list of paths
beside one `edits` — so `edit_file` reads both now: the names other tools
taught a model, and a list of paths beside one `edits` as those edits in each
file. The fourth reading, with the object read back: every scenario called
real tools, and two of them ended the same new way — the model read the file,
found the number, and stopped without a word, which every front end renders as
a hang. A turn that did something and said nothing is asked once, in words,
what it found. The rest was the model's: `read_file` for a prompt that said
`cat`, a checker whose `fails` was right for the wrong reason, and a parent
that rewrote `lib.rs` until the claim came true. The fifth reading, with the
edit tool's readings in: the edit scenario passed, and the new delegation
scenario found the next shape — `delegate` handed a tool call in place of a
task, told "delegate needs a task", which it thought it had given. The
refusal names the shape now and shows the one that works, and an object with
the sentence under `task` is read for it. The sixth reading was the one the
fourth had hidden: a reply carrying tool calls arrived with `finish_reason:
"stop"`, Ollama's word for a turn that also spoke, and the OpenAI dialect read
the word rather than the calls — so the calls were logged as text and never
run, which is what the fenced retry in the fourth reading had been. A message
that carries tool calls is a tool use whatever the finish reason said, in the
dialect and in the loop. And a guess is answered with what is there: an edit
whose text is not in the file is refused with the nearest line shown, so a
`9001` written for a port never read is told about the `8443`. The seventh
reading passed two of the five for the first time, and the delegation
scenario found the next thing: the model gave its sub-agent `max_steps: 1`,
which reads the file and has no step left to say what it read, and the
report said "what it had done:" and nothing. A sub-task's budget is never
below a call, a look, and an answer now, and a child that ran out with a
call as its last word is reported by what it called. And a scenario has eight
steps now: left at the default of two hundred, one ran for an hour and took
the job past its timeout, with every other job's log held hostage behind it.
The eighth reading, with the calls deciding: the delegation scenario passed,
three of five now, and the one that still wandered showed why — the model
writes three calls in one reply, the reading took the first, and given that
one back it wrote the other two again, every step. Every object naming an
offered tool is a call now, in the order written. And with eight steps a
scenario, a turn that runs out of them with a call as its last word has done
work nobody was told about: it is asked once more, with nothing to reach
for, what it found — so it ends on the answer rather than on the limit. And
the limit is the model's, not the children's: a turn that ran out of steps
with sub-agents still out dropped them with the nursery, tokens spent and
answers unread, and waits for them now as a finished turn does. The ninth
reading passed three of five, and the claim scenario showed the shape of a
loop: the same `verify` with the same arguments, answered `fails` the same
way, five times over, until the sub-agent ceiling ended it and the parent
apologised for the ceiling instead of reporting the verdict. The same call
answered the same way twice is a loop, not a question: the third is refused
and pointed at the answer it has. Same result is the test, not same
arguments, and a call that changed something starts the count over — a
command run again after an edit is not caught by it. The tenth reading
failed all five, and the delegation scenario said why in its own words: the
parent started three readers without waiting, ended the turn, was asked what
it found, and made a number up — with the readers' answers appended below
where it never looked. What a turn's sub-agents brought back is handed to the
model once before the turn ends, so it answers from them; a turn out of steps
gets them in front of it for its last word. The eleventh reading, with the
repeat rule in, passed one of five, and every failure was the model's: a
`find_skill` for a skill named `rust` to answer a question it had already
read the answer to, an edit of `9001` written again after the nearest line
and the file itself had both shown `8443`, `read_file` for a prompt that said
`cat`, and a claim made true by rewriting the file under it. The readings
have run out of mechanism to fix at this size; what is left is the model, and
a larger one does not fit the runner's forty minutes. The twelfth, with the
hand-over in, passed three of five — reading, editing and delegating — and
the two it failed were the model's. Four more readings after the sandbox
landed ran one to two of five: the edit scenario now fails on a `9001`
written a second time after the nearest line and the file had both shown
`8443`, and the claim scenario on a file rewritten to fit the claim — both
the model's — and one delegation ended at `max_tokens` inside a fenced JSON
call, with nothing called. That last one is ours: a reply cut at the output
limit is neither an answer nor a call, and is asked once to go on, so a call
gets written whole and an answer gets finished. The reading after that ran
two of five with nothing of ours in the three: the readings have found what
they are going to find at this size. The job stays as it is, read rather than
rerun.

One more came out of a later reading, and it was a tool contradicting itself:
the model wrote a skill, was told where it went, handed that path to
`read_file` and was refused for being outside the workspace — which it is, and
correctly so, since skills live in the agent's own directory. It spent two
calls on the refusal and then said the agent was not installed. The result of
a write says how to read it back now, which is `load_skill` and not a path.
The other three failures in that reading were the model's: `read_file` for a
prompt that said `cat`, an edit of `9001` written twice against a file it had
just read as `8443`, and a `lib.rs` rewritten until the claim under test came
true.

The reading after it, with the edit scenario passing for the first time in a
while, found one more of the same kind. `find_skill` takes a query and a name
to install in one call, and a name no source offers threw the query away with
it: the model asked to install `rust` while searching for `config.rs`, was
told only that `rust` does not exist, asked the identical thing again and
gave up — holding, unread, the answer it had been given two steps earlier. A
failed install answers the search that came with it now. The other two
failures were the model's again.

The reading after *that* one caught a defect these readings had introduced.
The sentence about how to read a skill back named `find_skill`, which searches
the sources you could install from — where a skill just written by hand will
never appear. The model took the advice and was told `no source offers a skill
called "config_port"`, which is true and useless. `load_skill` reads what is
installed, and that is what the message says; the next reading had that
scenario passing. Two readings, two versions of one message wrong in a new
way, which is the argument for reading them rather than rerunning them. The
same transcript gave one more: a search with `[skill_sources]` emptied — how a
person says do not fetch skills from anywhere — was answered "no source offers
a skill matching …", which is a miss rather than the setting it actually is.

The edit scenario has now failed the same way four readings running, and the
last of them made the mechanism plain. The model writes `9001` for a port it
never read, `edit_file` refuses with "read it again, it may have changed", the
model reads the file — `8443`, right there — and writes the identical edit.
The advice is wrong for that case, because it has read it. The nearest-line
rule offers nothing to look at, because a bare literal shares no word with any
line, and that is exactly the shape a model gets wrong. A refusal for a bare
number now shows the lines that have numbers on them. The next reading had it
passing, and three of five with it — the best this tag has done.

That reading left two failures, and one of them was ours twice over. The
result of `verify` began `checked by 01M1N5…`, and ids here are all the same
shape, so the model read the checker's session id as a fact and handed it to
`forget` — twice, before going back to the claim. It says `checked in session`
now. And the checker itself keeps answering `fails` where its own words say
"cannot be proven": it ran `cargo run --example add` in a directory with no
`Cargo.toml`, and reported the claim as false rather than unsettled. The
instructions now separate the two in as many words — a command that would not
run tells you nothing about the claim.

And the reading after that caught the cost of the fix two readings earlier. A
failed install answers the search that came with it, and the search for `port`
answered with five skills about spreadsheets, documents and web testing, under
the line "install one by name". The model, holding the answer since its first
call, installed one and ran out of steps loading skills. The search was
matching substrings: `port` is inside `supports`. It asks by word now, through
the same `akin` that ranks memory — prefix matching from four characters, so
`ports` still finds `port` and `supports` does not.

The reading after that showed the search answering "no source offers a skill
matching \"port\"" and the wander gone, and left the next shape in plain
sight: an assistant reply of sixty bytes that stops mid-object — `{"name":
"find_skill", "arguments": {"install":"config.rs",` — followed by the model
writing its previous call again and then apologising that the tool kept
returning the same thing. A reply cut at the output limit is already asked
once to go on, but the tell was the stop reason, and Ollama sends `stop` for a
generation it truncated. The tell is the text now: an object that names a tool
that was offered and never closes is a call cut in half, whatever the reason
said. The reading after it had that scenario passing and three of five again.

What did not take was the wording: naming the checker's id as a session in the
`verify` result was supposed to stop a model handing it to `forget`, and the
next reading has it doing exactly that again. `forget` answers it now — an id
that names no fact but does name a session is told which it is, rather than
only that no fact matched. A message a model ignores is not a mechanism; the
tool that receives the mistake is. The reading after that has the model trying
it again and being told what the id is, which is the difference between an
answer and a dead end.

The checker's other half stays the model's, and the obvious mechanism for it
does not work. It reports `fails` from a `cargo run --lib` that never ran,
which is a statement about the tool rather than the claim — but "every one of
the checker's calls failed, so the verdict is unproven" would also discount the
checker who proves `the build is broken` by a build that fails. A command that
returned non-zero is evidence; a command that did not run is not, and telling
those apart is not something an exit code will do reliably. The instructions
say it in words instead.

**Autonomy is a task and its boundaries, and the task is what was asked.**
The goal check ran only for a session with a goal set, so `rook run` at
autonomous — the stance with nobody else to check — checked nothing. With no
goal set, the turn's prompt is the goal now. The front end's turn only: a
checker checking itself against the claim it was handed, or a sub-task
checked against its errand, is a checker per step at every depth. The smoke
job runs with `--yes`, which is the autonomous stance, so its thirteenth
reading exercised this with a real model: three of five again, one check
that held, and one that failed for the wrong reason and gave the turn a
second go it did not use well — the mechanism's first live turn, and the
model's usual share of it.

**A server fetched once is one somebody has to remember to update.** Now the
agent remembers: past `[agent] server_update_after_days` (thirty by default,
zero for never) the tag file's own age says so, and once per session the offer
follows the stance — an autonomous turn fetches again and says whether the tag
moved, a person is asked, and with nobody to ask it is an open question naming
`rook lsp update`. Nothing new is stored for it; the tag file's mtime was
already there.

**A command is contained by the platform, where the platform can.** The
boundary was text — a path check in the file tools and pattern rules over the
command line, which a command's own children never met. Now `run_command` and
a background job run under Seatbelt on macOS, Landlock on Linux, and a low
integrity level on Windows — through a launcher that is `rook` itself, lowered
— with the workspace and the temporary directory writable, everything else
read-only, the network a switch. What was applied is on every result, because
a sandbox that quietly did nothing is worse than none: Landlock before kernel
6.7 cannot restrain the network and never restrains UDP, an integrity level
never does, and FreeBSD has nothing yet — `auto` runs the command as it is
and says so, `required` refuses.
`[sandbox] writable` names the directories a build's caches live in, and a
failed contained command says it was contained and names that setting. The
tests run the same command contained and not, and CI's Linux runner passes
all of them under Landlock with the network restrained; FreeBSD passes by
skipping, which is what a platform with nothing to contain a command does.
The Windows runner passes them too, with `rook` itself as the launcher: the
write outside refused, the workspace and scratch written, reading everywhere,
and "Access is denied" reported as the containment it was. The first smoke
run under Landlock passed the same three of five as before — containment
cost the scenarios nothing — and a failed command's result carried the note. The note is for a failure that reads as a refusal, not for
every failure: a missing `Cargo.toml` is not the sandbox's doing.
[ADR-0011](adr/0011-containment-is-the-platforms.md) has the reasons: no
helper binary, no container, and the network open by default.

**The browser is a front end.** It was a viewer with a chat bolted on: a new
session per tab, no way to stop a turn, a stance select that spelled the three
old modes by hand, and the model's answer as raw text. Now the chat resumes any
session and reads it back, stops a turn, draws its selects from the engine's
own lists, renders the little of Markdown an answer uses, and asks — with
permission — to notify when a turn is waiting on a person, which is the case
for a notification. The sessions tab follows a transcript being written by a
terminal and hands it to the chat. The page split into hand-written ES modules
the browser loads as they are, still with no bundler, and the daemon's tests
hold the one invariant that matters there: every module loaded is embedded and
served as script. [ADR-0012](adr/0012-hand-written-modules-no-bundler.md)
supersedes the one-file part of ADR-0007 and keeps the rest.

**The checklist tool was measured here, and the decision holds.** Three arms,
six multi-step tasks, three runs each against a 35b mixture-of-experts: every
arm passed every task, and the tool cost 78% more tokens and 68% more steps to
do it. The finding worth keeping is the other one — told once in the system
prompt to keep a plan, the model ignored the tool entirely, nine calls on a
three-part task and not one a plan. It only became a real arm once the turn
carried a reminder every step, and that reminder is both what makes the tool
used and the whole of what it costs. What was not learned: anything about hard
work, because nothing failed. [ADR-0010](adr/0010-no-todo-tool.md) carries the
numbers.

**How that was measured.** [ADR-0010](adr/0010-no-todo-tool.md) declined a planning tool on
a benchmark that was k=3 on one harness, and said in as many words that a
future model is worth re-measuring rather than assuming. So: `cargo xtask
bench` runs three arms — the plan line, nothing at all, and a `plan` tool with
instructions to keep the list — across six multi-step tasks, three of them with
somewhere to go wrong, each scored by looking at the workspace afterwards and
never by reading what the model said about it. The tool exists behind
`[agent] todo_tool`, off, because an arm cannot be measured without building
it. Cost is recorded beside the pass, since cost is what the tool was declined
for.

**A turn spending itself on compaction says so.** Found by watching a real
model do long work: a task of three one-line fixes took an hour and a quarter
against a 27b model, and the transcript said why — seven auto-compactions in
seventy-four events, because the window had been set to 8000 tokens. Each
compaction is a summarisation call, so most of the turn went into bookkeeping,
and the only report of it was the count in the outcome, an hour later. At the
third compaction the turn now says what is happening and names the setting that
fixes it. The work itself was done correctly, which is the other half of that
observation.

**A contained command cannot read the agent's own store.** Reading was allowed
everywhere, the state directory included — every project's transcripts, every
checkpoint's content, everything the agent was told to remember — and with the
network open, reading is the whole of what an exfiltration needs. Seatbelt
denies it after the blanket read, where the last match wins; Landlock grants
and never denies, so everything-but-this is spelled as every sibling down the
excluded path. A Windows integrity level restricts writing and cannot do this,
and the result of every command says which it got.

**Who else can read the agent's history is a question `doctor` answers.** The
state directory is created for its owner alone and an existing one is left as
its owner made it — right, and silent: a `~/.rook` from an older build or a
`mkdir` keeps mode 755 and hands every transcript, and every file the agent
ever read, to every account on the machine. The check found exactly that here.
Reported with the mode and the `chmod`, not fixed: changing a mode its owner
chose is not this program's to do.

**The writes the daemon already serves go over it.** `session goal`, `session
rewind`, `memory rm` and `store maintain` refused while `rookd` was up, which
meant stopping the daemon to set a goal — and `store maintain` is what somebody
reaches for exactly when a long-running daemon has filled the disk. Each is now
the same call the daemon makes on its own store, so there is one implementation
and two ways in. Memory went the same way and was
the worse case: `memory add`, `memory search`, `memory history`, `memory diff`
and `memory since` all refused, which is the whole of what a person does with
memory while an agent is working. Scored search is `recall`'s question asked
differently, so it is one core function both call rather than the listing
endpoint filtered down, and a fact travels with the workspace it is scoped to
— the daemon's own project is not necessarily the one being asked about.

Then the rest of them, because "stop the daemon" is not an
answer a tool should give: `store gc`, `prune`, `verify` and `train`, `session
fork` and `rm`, every `skills` subcommand, and `checkpoint` create, list and
restore. Two of them — `skills sources` and `skills search` — turned out to
read no store at all and to have been refusing only for where they sat in the
match.

Three things came out of doing it rather than being the point of it. `store
gc` was assembling its own collection options and taking the library's
ten-minute default instead of the configured `gc_grace_secs`, so a store told
to hold new objects for an hour collected them after ten minutes when a person
asked rather than when the timer did; it is one core call now, and both ask it.
`rook skills why` was thirty lines of printing in the command line and nothing
anywhere else — it is a value the API serves too. And the tests that proved
routing were proving it by a refusal, which stops working the moment nothing
refuses: they check the line a routed command prints instead, so a command that
quietly opened the store itself fails them.

**What a command wrote is named, even though it cannot be undone.** A tool
that declares its paths is checkpointed and diffed exactly; a command declares
none, so `changes` — the CLI's `/diff`, the browser's table, the API — answered
"nothing changed" for a turn that had rewritten a file with `sed -i`. That is
not a gap in an answer but a wrong one. The workspace is now walked after a
command for what was written since it started, and those paths are named apart
from the diffable ones: no diff and no restore, because nothing holds what they
were, but no silence either. mtime is the question asked, so the word is
"written" rather than "changed", and a workspace too large to walk says so.
Capturing content instead was measured and refused: eighteen seconds a command
on this repository.

**A 401 from an MCP server says what to do, and a rate limit is waited out
as long as it asked.** Two ports from the thirtieth reference pass. An MCP
server answering 401 was reported as a transport error with the status and
nothing else — the `WWW-Authenticate` header, which is where the
authorisation server is named, went in the bin with the rest of the response;
it is its own error now, carrying every challenge and naming the `[[mcp]]`
table's `headers`. And the retry doubled from a second and ignored
`Retry-After`, so a limiter saying "thirty seconds" was asked again at one,
two and four and the turn died on a refusal it had already explained. The wait
is the longer of the two now, up to a two-minute ceiling.

**A capable model drove it, and everything worked.** Five scenarios against
`qwen3.8-27b` in LM Studio: five passes, first attempt, none of the nudges the
loop keeps for small models needed — no silence to break, no cut reply to
continue, no loop to refuse. That is the first evidence that the failures the
CI readings kept finding after the mechanism was fixed were the 3b model's own,
and it is what the README's largest admission was waiting for. What it does not
cover: the Anthropic thinking round trip, which is that dialect's and needs a
key, and work longer than five short scenarios.

**Thinking is carried across a tool call.** A turn that thought, called a
tool and went on was sending Anthropic an assistant message with the call and
not the signed thinking block that led to it — which that API refuses, so
every turn with a tool call against a thinking model would have failed on its
second request. The blocks are kept whole and replayed first, never parsed and
never rebuilt: a signature covers bytes, and a block reconstructed from parsed
fields is a different block. Unsigned blocks — a stream that did not finish —
are dropped rather than guessed at, and earlier turns carry none, which is what
the API expects. The test is the round trip, and it fails without the replay.

**A request refused for what the agent added is asked again without it.** The
three dialects decide by model name whether to send a reasoning effort, and a
gateway serving something else under that name is where a name is wrong: the
route answers 400 and a turn that has been running for minutes ends on a field
the user never set. `Retrying` drops the effort and asks once more, then stops
sending it for the life of the process. Tool definitions are the other thing
the agent adds; dropping those would leave an agent that cannot act and does
not say why, so the refusal names `[agent] native_tools = false` instead.

**The smoke job got slower for a reason.** Every autonomous turn now ends
with a goal check, which is a whole extra turn, and the job runs `--yes`. Runs
that took sixteen minutes take up to forty; the timeout is sixty now, sized
for the work rather than for a hang. Each scenario's verdict is printed as it
finishes, so a job that is killed still leaves the readings it had.

**A long store pass belongs on a thread, not on the runtime.** `rookd` ran a
prune, a collection and zstd dictionary training where they were awaited —
holding a runtime worker for all of it, which on a small machine is the thread
the chat socket needed. Both the handler and the hours-long timer run it on a
blocking thread now; the test runs on a single-threaded runtime and fails
without the change.

**A setting changed while the daemon runs takes effect at the next turn.** It
read `config.toml` once at start, so changing `[agent] model` — the setting
people change most — took a restart, and the restart was something a person had
to be told to do rather than something that happened. I told somebody exactly
that an hour after making every window share one daemon. The file's timestamp
is asked once per turn, which costs a `stat` and needs no watcher thread, and
the answer is said in the conversation when the model is not the one it was.

**What `effort` actually reaches.** Measured against the machine that reported
the slow turn, because the advice given there was wrong: the reasoning effort
is sent only to the families documented to take it — gpt-5, o1, o3, o4 — and
Anthropic's current models are asked for adaptive thinking instead. For a local
`qwen3.8-27b` on LM Studio, nothing is sent at all, so the `high` in the footer
changed nothing and the minutes of thinking were the model's own. Asked
directly, that endpoint accepts `reasoning_effort` and ignores it: `high` and
`minimal` produced 179 and 195 reasoning tokens for the same three-word answer,
and `enable_thinking: false` only brought it to 144. An `adaptive` rung of our
own would be a fourth thing that does not reach this model.

**An hour, measured.** A one-file change took sixty-nine minutes against a
local 27b, and the transcript says where they went. Forty percent was the model
thinking at `high` effort — twenty-two replies, gaps of forty-five to four
hundred and seventy-eight seconds each — and that is the model. The rest was
ours. Ten minutes: an approval nobody was there to answer, refused at the
ten-minute deadline. Four more: the retry after it, written as `{path,
replacement}` inside `edits` and told "missing field `old`", which is true and
says nothing about what the model plainly meant — an entry with the new text
and no `old` is a whole file, which is `write_file`, and it says so now.
Twenty-six and a half minutes: the goal check, which inherited the turn's own
two hundred steps, ran at `high` effort, and ended on a provider timeout with
no verdict at all. A checker judges rather than works: a dozen steps now. Half
the hour came after the edit was already on disk.

**A server we installed is not asked to prove itself.** Somebody was offered
`pyright-langserver` every session on a machine where it was installed, and
each yes reinstalled it. The offer was right about the question and wrong about
the answer: servers are probed with `--version` before being believed — which
exists because rustup leaves a `rust-analyzer` shim on `PATH` that is not a
server — and `pyright-langserver --version` refuses to run without `--stdio`
and answers with an error. The probe is for `PATH`; what `rook lsp install`
fetched has a digest on record and is taken as it is. The same reading found
its neighbour: `typescript` was installed unpinned, npm gave TypeScript 7 — the
native rewrite, with no `lib/tsserver.js` — and the server could not start.
Pinned to 5, with the pin respected rather than turned into `typescript@5@latest`.

**The window is asked for when it is about to matter, and believed when the
endpoint disagrees.** Three things, one question: a turn compacted five times
to fit 32768 on a machine serving 262144. The number is now asked of the
endpoint — but at the first compaction rather than at the start of every turn,
because that is the only moment it changes anything and a probe on every turn
changed the shape of every conversation three test fixtures had to answer,
which is how the cost became visible. A window the endpoint then refuses as too
long is not fatal either: the refusal is a fact where the setting was a guess,
so the turn believes it, budgets against it, summarises to fit and carries on —
and the smaller number is kept on the engine, which outlives the turn, so a
refusal is paid once rather than every turn.

**A window nobody could see was a quarter of what the machine had.** The
context length is guesswork for anything self-hosted — 32768 assumed for
Ollama and LM Studio — and `[agent] context_window` exists to correct it, but
`doctor`'s offer to name the right number never fired: it compares the
assumption against what the endpoint reports, and LM Studio's compatible
listing reports nothing. Its own does, and the model on the machine this was
found on serves 262144 — eight times what was assumed, on a turn that compacted
five times. Asked for now, when the compatible listing says nothing, and the
loaded length is preferred over the model's maximum because that is what will
actually be served.

**A reply cut at the output limit writes nothing, and said so to nobody.** A
turn ran for two and a half hours against a local 27b, read nine files, searched
nine times, compacted five times — and changed not one line, which is how it was
reported. The transcript says why in three events: the reply was cut at the
output limit, the loop asked it once to go on, and it was cut again, so the tool
call it was writing never arrived. That limit was a constant of 4096 in the
library, reachable from no config file, and a model that reasons out loud spends
it on reasoning. It is `[agent] max_output_tokens` now, and by
default it is the room left in the window after the prompt — the most that
could arrive anyway. What a model's own ceiling is appears in no API and
differs per model, so an endpoint that refuses a reply for being too large is
answered by asking for half and remembering what it accepted, the same way a
refused reasoning effort already is: learned from the endpoint rather than kept
in a table that rots. A turn that still ends on the limit says which number to
change, the way the compaction notice does.

**A window makes sure there is a daemon.** The first fix routed a window that
found `rookd` already running, which was not the case anybody had: two `rook
tui` and no daemon at all. The window that takes the store serves nobody, so
the second still died on the lock — and the error message, rewritten in that
same commit, said "`rook tui` works beside it", which is true only beside a
daemon. It starts one now, on a port the system picks rather than the default
one somebody else may be holding, and watches the child rather than waiting out
a fixed thirty seconds if it cannot start. `--alone` takes the store for one
window, which is what the tests that are about a window use.

**Two windows, one engine.** Someone installed it and worked in it, and the
first thing they hit was `rook tui` in a second project dying on the store's
lock. Every other command had learned to route through `rookd`; the TUI opened
the store as its first act. It reads through the daemon now — sessions,
transcripts, memory, skills, objects, stats — and runs its turns over the same
chat socket the browser uses, so the second window is another client of one
engine rather than a second copy that cannot exist. What stays local is the
slash commands, which read and write this process's store directly, and they
say so. The rest of that afternoon's report is in the commits: reasoning
streamed one word to a line, no wheel scrolling, a footer promising `j/k` where
`j` and `k` are letters in the message box, commands discoverable only from
`/help`, an approval that could be granted for one exact command and nothing
like it, and a two-minute turn that showed a fixed `working…` and read as a
hang.

## After that

**One live Anthropic turn, when there is a key.** The thinking round trip is
built and tested against a socket that replays the documented wire shape —
signed blocks in, the same bytes back, first in the assistant message — but
never against the API itself. It is the one dialect where a wrong guess fails
every turn with a tool call rather than degrading, so the confirmation is worth
having and is one command:

```sh
ANTHROPIC_API_KEY=… cargo xtask smoke --model anthropic/claude-sonnet-5
```

Any of the families in `takes_adaptive_thinking` will do; the scenarios that
edit a file and read a command's output both call a tool, so a refusal would be
the first thing they report. Until then the entry in the README says so.

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

## Not planned

- **A hosted service.** Local-first is the point; there is no server to send
  transcripts to, by design.
- **A bespoke skill format.** [ADR-0003](adr/0003-agent-skills-format.md).
- **Telemetry upload.** The config field exists so the answer is discoverable, not
  because it is going to become true.
- **An IDE extension per editor.** ACP is how that gets solved once.
