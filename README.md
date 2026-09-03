# Rook

An autonomous agent whose memory you can actually read.

Rook is a general-purpose local agent — coding, research, automation — written in
Rust and shipped as two static binaries with no runtime. It stores everything it
does in a compact, content-addressed store, and it treats *inspecting* that store
as a feature rather than a debugging afterthought: a CLI, a terminal browser and a
web UI, all views over the same engine.

> **Status: young, and honest about it.** The storage layer, the skill system,
> the inspection tools, the agent loop, streaming, MCP, LSP and ACP are
> implemented and under test — including whole turns driven over a real socket
> against a server that speaks the provider's dialect. What has never happened is
> a model with judgement driving any of it. Nothing below describes something
> that does not exist, and what is missing is listed under
> [what is not done](#what-is-not-done-yet).

## Why another one

Every design decision here traces to a specific, public failure in an agent people
actually run — an SQLite log that writes terabytes a year, a checkpoint feature
implemented as `git add .` over a 45 GB workspace, tool schemas that cost 5,000
tokens a turn, a context overflow with no recovery. The research is written up with
citations in **[docs/research/agent-landscape.md](docs/research/agent-landscape.md)**.

Three things follow from it.

**Memory is compact by construction.** Content addressing, zstd dictionaries
trained per object kind, and small objects inlined into the index. On a synthetic
transcript of 3,000 turns and 320 tool results over 64 distinct files:

```
logical bytes written by the agent :    23.31 MiB
  after dedup (distinct objects)   :     5.29 MiB
  cold store, standalone zstd      :     0.63 MiB   ratio  8.4x
  warm store, trained dictionaries :     0.14 MiB   ratio 37.1x
  on-disk total (index + objects)  :     1.07 MiB
  end-to-end (logical -> on disk)  :     21.9x
```

Reproduce it yourself: `cargo xtask compaction`.

**Memory is inspectable.** `rook store stat` tells you what your history costs and
where it went. `rook session show` prints any transcript by sequence number.
`rook store cat` prints any object. What the agent has learned about you is a list
you can read and delete from, because a fact nobody can remove is one that quietly
steers every later turn. The TUI and the web UI show the same data.

**Skills are versioned and environment-aware.** A skill declares the environment it
is valid in — OS, userland, arch, language and tool versions — and can carry
platform-specific bodies instead of forking into `deploy-linux` and
`deploy-windows`. Every edit can be captured, diffed and rolled back.

## Install

```sh
git clone https://github.com/ASlava12/rook && cd rook
cargo xtask dist               # builds, packages the built-in skills, prints the sizes
```

Two binaries, no runtime and no shared libraries — 5.4 MiB and 5.3 MiB at the
time of writing, which `dist` prints so the number here can be checked rather
than believed.

Requires a Rust toolchain and a C compiler (two dependencies vendor C — see
[docs/platforms.md](docs/platforms.md)). No Node, no Python, no Docker.

## Use

```sh
rook init                                  # create ~/.rook, config, store
rook doctor                                # environment, what contains a command, model reachability
rook models                                # what the configured provider serves

rook                                       # talk to it
rook run "summarise what changed in src/"  # one turn, streamed, for scripts
rook chat --session last                   # pick up where you left off here
rook session show last                     # `last` works wherever a session does
cargo test 2>&1 | rook run "why does this fail?"   # stdin joins the prompt
rook --json run "..." | jq .outcome.reply  # one object: reply, tokens, changes
                                           # exit 2 if the turn did not finish
rook tui                                   # full terminal UI: chat plus a store browser
rookd                                      # http://127.0.0.1:7717 — web UI + API
```

The browser is a front end, not a viewer: the chat resumes any session, stops a
turn, answers the agent's approvals and questions in place, and renders what
the model says; a session being written by a terminal is read live from the
sessions tab, which can hand it to the chat. The stance and effort selects are
the engine's own lists. With permission, a turn that stops to ask for a person
sends a notification, which is the case for one: an autonomous run in a
background tab. It is hand-written HTML and ES modules served by `rookd` with
no bundler and no toolchain — `cargo build` is the whole story
([ADR-0012](docs/adr/0012-hand-written-modules-no-bundler.md)).

In a conversation, slash commands reach the same engine the subcommands do:

```
/btw <question> ask about the work without joining the conversation
/goal [text]    what this session is for; the agent is told
/context        what this conversation costs, and of what
/skills [name]  what applies here, or one skill's body
/undo           rewind past the last exchange, files included
/rewind <seq>   rewind to a point in the transcript
/session  /mcp  /new  /help  /quit
```

`/btw` answers from what the agent already knows — no tools, one call — and its
answer never enters the context the agent carries forward, though it is still in
the transcript. Ctrl-C stops the turn in flight without leaving; whatever it
already did stays in the log.

Typing while a turn runs steers it rather than waiting for it: what you send
reaches the model at its next step, so a turn heading the wrong way can be
corrected without throwing away what it has already done — and if the turn has
work out with sub-tasks, every one of them hears it too. In the TUI and in the
browser, which are the two front ends that can take input while one is running.

### Reading what the agent remembers

```sh
rook store stat                # size, compression ratio, breakdown by kind
rook store ls --kind file      # objects, newest first
rook store cat 4f2a9b          # any object, by short hash
rook store gc --dry-run        # what is unreachable
rook store prune --dry-run     # what the retention policy would drop

rook search "the CRLF fix"            # across every transcript, ranked
rook session ls
rook session show 01JQ… --from 0 --limit 50
rook session show 01JQ… --json | jq '.[] | select(.kind=="tool-call")'
rook session context 01JQ…            # what the conversation costs, and of what
```

### Seeing what changed

The loop checkpoints every file a tool is about to touch, so the store already
holds each file as it was before the agent first touched it. That is a diff of a
whole session, computed without a repository and for files that were never under
version control:

```sh
rook session diff 01JQ…            # unified diff of everything it changed
rook session diff 01JQ… --stat     # names and counts only
```

A file the agent wrote back identically shows as unchanged, and its own
intermediate states are not counted — the baseline is what was there before it
started, not what it wrote last.

A session is bound to the project it started in, so resuming one by id from
somewhere else continues it there rather than here — its transcript names that
project's files and its checkpoints restore into it. `-C` overrides, because that
is the user deciding.

### Undoing a turn

The loop checkpoints every file a tool is about to modify, so a rewind puts the
workspace back as well as the conversation. `delete_file` exists for that reason:
`rm` through the shell declares no path, so nothing is captured and no rewind
brings it back — every other change a command makes leaves the content somewhere,
and a deletion leaves nothing — and forks rather than truncates, so
the turns you rewound past stay readable in the parent session.

```sh
rook session rewind 01JQ… --to 12               # conversation and files
rook session rewind 01JQ… --to 12 --keep-files  # conversation only
rook session fork 01JQ… --at 12                 # branch without touching files
```

Restoring is the one step that writes over something, and what it writes over may
be an edit made by hand that no checkpoint holds. So the state on disk is captured
first, onto the fork it just made; the command prints the rewind that puts it back.

### Something that keeps running

`run_command` waits, caps the output and kills at the timeout, which is right for
a build and wrong for a server. `background: true` starts it and answers at once
with an id; `job` reads what it has printed since, and stops it:

```
run_command  { "command": "npm run dev", "background": true }   → job001
job          { "id": "job001" }                                 → what it printed
job          { "id": "job001", "wait_secs": 60 }                → …when it ends
job          { "id": "job001", "stop": true }
```

`wait_secs` is what makes several commands one wait rather than many: three test
suites started together and then waited on cost four tool calls, where asking
again and again costs a whole turn each time. It is capped at the timeout a
command in the foreground would have been given — the same wait, whichever way it
was started.

`/jobs` in the chat and the TUI shows the same list without spending a turn on
it, and `rook mcp` offers the same pair, since a stdio session lasts as long as
the client keeps it open. The registry belongs to the front end rather than to a turn — one built per turn
would kill everything in it between one turn and the next — and it takes the
processes with it when it goes, because a dev server that outlived the agent that
started it is one nobody knows to stop. `[sandbox] max_background_jobs` caps how
many run at once and the refusal names which to stop; each keeps the last
`max_output_bytes` it printed, since a server's interesting line is its most
recent.

### Asking what a crate offers

```
crate_api { "crate": "semver", "entity": "VersionReq" }
```

A signature recalled is a signature guessed at, and one that compiles is worse
than one that does not. The answer is already on the machine: cargo unpacks
every dependency under its registry and `Cargo.lock` says which version this
project resolved to, so this reads the source rather than the network. Not
rustdoc JSON, which would be better and is nightly-only.

The scanner is not a parser: it finds declarations and attributes methods to the
`impl` they sit in. It does not see what a macro generates.

### Reading a page

Off by default, and off means the tool is never offered rather than the call
refused — a tool the model cannot see is one it cannot decide to try:

```toml
[web]
enabled = true
```

`web_fetch` reports its risk as the address it is going to, so an allow rule can
name a host: `allow = ["https://docs.rs/"]`. A redirect that stays on that host
is followed and one that leaves it is reported instead — an approval named an
address, and following it elsewhere would spend that approval somewhere nobody
agreed to. HTML arrives as prose, script and
style dropped. What comes back is somebody else's writing on its way into the
model's context — not a fact and not an instruction, which is why the answer
always says where it came from.

`web_search` needs an engine named as well, and there is no default because the
two differ on who sees the query:

```toml
[web]
enabled    = true
search     = "searxng"                  # your own instance: the query stays here
search_url = "http://127.0.0.1:8888"
# search   = "brave"                    # or hosted, with BRAVE_API_KEY in the environment
```

Its risk is the engine's address rather than the query, so allowing your own
instance does not also allow somebody else's. An engine named without the key it
needs is offered as nothing at all — a tool that fails on its first call teaches
the model to stop asking.

### Checking, rather than believing

A turn can hand a claim to an agent that did not make it:

```
verify { "claim": "the tests pass after the change", "settles": "cargo test -p rook-store" }
```

The checker runs in its own session with every tool that changes something
withheld — not asked not to use them, not given them — and must end with
`VERDICT: holds`, `fails` or `unproven`. A checker that stops without one — a
small model describes the command it would run and ends there — is asked once,
in its own session, to run it and commit. A reply that still will not is reported
as unchecked rather than as a pass, because "looks reasonable" is what gets said
after reading something and running nothing.

It is not a sandbox: `run_command` can still write. It is the difference between
a rule the model weighs and a tool it does not have.

The same tool checks a claim about the world, when `[web]` is on: find where it
is said, quote it with its address, and keep what a page states apart from what
its writer argues. And the rule that makes either of them worth anything — a
verdict from a checker that ran nothing and read nothing is reported as unproven
however sure it sounded. Reaching for nothing is what a fabricated check looks
like, and it is what asking a second agent was supposed to get past.

### When a small model wanders

The loop holds a few lines that a large model never meets and a small one meets
every turn, each found by running a real one in CI and reading the transcript.
A tool call written as text — `{"name": "read_file", "arguments": {...}}` as
the reply — is a call, every one of them in the order written, when it names a
tool that was offered. A turn that did the work and ended without a word is
asked once, in words, what it found; one that ran out of steps with a call as
its last word is asked the same with nothing left to reach for; one whose reply
was cut at the output limit is asked once to go on. The same call
answered the same way twice is a loop, not a question, and the third is refused
and pointed at the answer it has — unless something changed the workspace in
between, which starts the count over. A checker that stops without a verdict is
asked once to finish, and a sub-agent's step budget is never below the three a
task needs. None of it is a retry: each is asked once, and a second silence is
reported as one.

### More than one project at a time

The store is one per `~/.rook` and takes a single writer; a workspace is one per
project. Bound together, a second project meant a second process — and the second
process was the one that could not open the store. They are separate now:

```sh
rookd                                              # one daemon
ws://127.0.0.1:7717/api/chat?workspace=/path/to/a  # a conversation in one project
ws://127.0.0.1:7717/api/chat?workspace=/path/to/b  # and another, at the same time
```

Each connection gets its own engine, looking at its own project, sharing one
history, one memory and one search. How many are kept is
`[server] max_projects`, because how many a daemon is asked for is decided by
whoever connects; past it the least recently wanted is dropped and rebuilt when
it is next named.

Two connections naming the same workspace run at once as well. A call that is
about to write claims those paths for as long as it takes, and a second turn
reaching for one is refused and told which session is holding it — refused
rather than queued, because the useful answer to "somebody is writing that" is
to go and do something else. `edit_file` needed no help: it replaces exact
text, and text another turn has changed is not there to replace.

The slower race is the other one: a turn reads a file, another rewrites it, and
the first writes back what it read. A read records who looked, and `write_file` —
the only tool that replaces a file whole — is refused when somebody else looked
last, with `edit_file` offered instead. Working alone you are always the last to
have looked, so you never meet it.

The claim is released when the call returns, when it panics on the way out, and
when the turn holding it is aborted. What none of those cover is a call that
never returns at all, so a claim also expires; and the registry is readable,
because a lock nobody can look at cannot be debugged when it wedges:

```sh
curl 'http://127.0.0.1:7717/api/writing'   # path, session, how long it has been held
```

`rook run` in a second directory still opens the store directly and still meets
the lock ([ADR-0006](docs/adr/0006-single-writer-store.md)).

### Rook as a tool for something else

```sh
rook mcp serve          # Rook's own tools over stdio, for any MCP client
rook mcp serve --yes    # …without asking, for anything the deny list allows
```

The other direction from `[[mcp]]`: instead of calling somebody else's tools,
this offers the file tools, the search and the command runner to whatever speaks
the protocol — an editor, a local model host, another agent. The approval policy
is in front of every call, and with nobody at this end to ask, a write is refused
and the refusal says what would make it possible.

It does not open the store, so it runs beside `rookd` — which is the arrangement
you want if the web UI is up and an editor should reach the same tools.

All three front ends run turns, stream them, and ask for approvals the same way:
`rook chat`, `rook tui`, and the web UI at `rookd`. Nothing is reachable from one
that is not reachable from the others.

### Code intelligence

A machine without one can fetch one. `rook lsp install rust-analyzer` takes the
latest release, checks the bytes as they arrive against the digest the release
lists for that asset, and keeps the binary under the state directory, where the
agent looks before `PATH` and where deleting the directory undoes all of it. It
prints what was checked and what was not: the download is intact; the release
was not reviewed. `typescript-language-server`, `pyright` and `gopls` install the
same way by their own means — npm under a prefix of ours with install scripts
off, the Go toolchain building from source — and each says what its publisher's
check covered. clangd ships a zip with the tree its binary needs, kept whole.
The agent notices too: a language with files here and no server is offered once
per session, and the stance decides what follows — a person chooses at `assist`,
`autonomous` fetches into the state directory, `free` uses the machine's own
installer. What is installed serves from the next session.

When a language server is on `PATH`, the agent gets four more tools: what the
type checker thinks is wrong with a file, where a name is defined, what actually
refers to it, and where a symbol lives in the workspace. It asks by name — the
name it can read in the source — rather than by line and column:

```sh
rook lsp servers                              # what applies here
rook lsp diagnostics src/main.rs              # without running a build
rook lsp definition src/main.rs parse
rook lsp references src/main.rs parse
rook lsp symbol ObjectId
rook lsp install rust-analyzer                # fetched, checked, kept under ~/.rook
rook lsp update                               # fetch again what is in place; say what moved
```

rust-analyzer, gopls, clangd, typescript-language-server and pyright are detected
automatically; `[[lsp]]` in the config overrides that. Servers start lazily, on
the first question that needs one. Installing one is a different decision from
using one you already have, so it follows the stance: a workspace with Rust
files and no `rust-analyzer` is a question when assisting, a fetch when
autonomous, and an open question in the outcome when read-only. A server
fetched this way is offered again the same way once it is older than
`[agent] server_update_after_days` (thirty by default, zero for never) — a
server fetched once is otherwise one somebody has to remember to update.
`[agent] install_servers = false` turns both offers off.

### Hooks

Commands that run at points in a turn, so extending the agent does not mean
changing it:

```toml
[[hooks]]
event   = "post_tool"                    # what it prints is appended to the result
match   = "/^(write_file|edit_file)$/"   # plain substring, or /regex/
command = "cargo fmt --all 2>&1 | tail -3"

[[hooks]]
event   = "pre_tool"                     # may allow, ask, or deny
match   = "run_command"
command = "my-policy-check"              # {"decision":"deny","reason":"…"} on stdout
```

Five events: `session_start`, `prompt`, `pre_tool`, `post_tool`, `turn_end`. A
hook reads JSON on stdin and may answer with JSON; plain output is treated as
context for the model, so `echo` works. A `post_tool` hook is given what the
tool measured as well as what it said — `is_error`, `truncated`, `full_bytes`,
and a `meta` object carrying whichever facts the tool records, such as the MCP
server that answered or whether a command hit its timeout. A `pre_tool` hook that fails blocks the
call it was guarding — a guard that cannot run is not approval — and no hook can
unlock what the deny list forbids.

### Models

The `provider/model` in `config.toml` picks the wire dialect:

```toml
[agent]
model  = "anthropic/claude-opus-5"  # ANTHROPIC_API_KEY
effort = "high"                     # low | medium | high | xhigh | max
prompt_cache_ttl = "5m"             # 5m | 1h — see below
# model = "ollama/qwen3-coder:30b"  # a local endpoint, no key
# model = "openai/gpt-5.5"          # OPENAI_API_KEY
# model = "google/gemini-2.5-pro"   # GEMINI_API_KEY, or GOOGLE_API_KEY
```

Three dialects are spoken natively — Anthropic's Messages API, Google's
`generateContent` and OpenAI's chat completions — and the last of those covers
`lmstudio`, `ollama`, vLLM, llama.cpp and anything else that answers it. An
endpoint that refuses tool definitions gets them in the prompt instead
(`[agent] native_tools = false`) and the model's reply is read back for the
calls; the same reading applies with native tools, because a small model
handed them still answers with the JSON object some of the time — it is taken
as a call when it names a tool that was offered, and as an answer when not. A
refusal that names them says so, because the setting is the answer and nobody
finds it by reading provider JSON.

A request refused for something the agent added rather than you is asked again
without it. Whether a model takes a reasoning effort is decided by its name, and
a gateway serving something else under that name is where a name is wrong: the
route answers 400, and rather than ending the turn on a field you never set, the
effort is dropped and the request made once more — and not sent again for the
rest of the run.

`prompt_cache_ttl` is which side of a pause you pay on. A cache write costs more
for the hour and a hit costs a tenth either way, so `1h` pays off exactly when a
conversation outlives five minutes — a person thinking between turns. It is not
the default because a scripted `rook run` never reads the cache its one turn
wrote, and would simply pay more for it.

Keys come from the environment, never from the config file or the store.
`rook models` asks the endpoint what it serves. Effort applies where the provider
has the notion; sub-agents and `/btw` run at `low` regardless, since a bounded
errand does not need the depth the main turn does.

### From an editor

`rook acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com) on
stdio — the same protocol Zed, JetBrains and Neovim already use — so no plugin is
needed per editor. Streamed output becomes `session/update`, and the permission
policy becomes the editor's approval dialog: the same decision, reaching the same
rules, whichever front end asks.

### What it is allowed to do

By default the agent asks before anything that changes the machine, and refuses
outright what the deny list forbids — no approval can override a denial:

```toml
[sandbox]
stance = "assist"                  # readonly | assist | autonomous
allow = ["git status", '/^(ls|cat|rg)\b/']   # plain string, or /regex/
ask   = ["git push"]                          # prompts even when autonomous
deny  = ['/(^|[;&|]\s*)(sudo\s+)*rm\s+(-[a-zA-Z]+\s+)*\/(\s|\*|$)/']
allow_outside_workspace = false    # file tools stay inside, symlinks included
isolate  = "auto"                  # contain commands where the platform can: off | auto | required
network  = true                    # a contained command may reach the network
writable = []                      # directories it may write besides the workspace, e.g. "~/.cargo"
```

The stance is how much latitude the agent has, and it is one setting rather than
two: an approval mode and a level of autonomy are the same question asked twice.
`readonly` changes nothing; `assist` confirms anything not explicitly allowed and
puts a fork in the work to you rather than settling it alone; `autonomous` runs
anything not denied — and before a turn of it ends, a checker that did not do
the work asks whether the goal was met and nothing forbidden done: the session's
goal, or with none set what the turn was asked, and `fails` gives it one more
go. A sub-agent inherits the stance of the turn that started it and is never
given more. `mode`, and the names `ask` and `auto`, are still read.

It is changeable while you work: `/stance` in the chat, F2 in the TUI, a
select in the browser, and a session config option over ACP — all the same
policy. `rook --yes` skips the prompts for one run. Unattended runs with no `--yes`
refuse rather than improvise, and say what would have made it possible.

A prompt shows what it is asking about: a write or an edit comes with the diff it
would make, built by applying the very edits the call would apply to a copy
nothing writes. Indented under the terminal prompt, coloured in the TUI panel, in
the browser's dialog, and as content on the ACP permission request — an approval
that names only a path is one given blind.

A question put to a person is bounded by `[agent] answer_timeout_secs` (ten
minutes). A closed tab or an abandoned terminal would otherwise hold the turn —
and the store's single write lock with it — for as long as the process lives.

Logs go to stderr and to `$ROOK_HOME/logs/rook.log`, at `telemetry.log_level`
unless `ROOK_LOG` says otherwise, rotated once at `telemetry.max_log_bytes` so
they cost at most twice it. Nothing is uploaded anywhere; `telemetry.upload`
exists so that answer is findable rather than assumed.

A rule that will not compile is not applied, and which list it was in decides
what that costs. Dropping an `allow` only means being asked more often, so it is
reported and dropped. Dropping a `deny` would leave a boundary the user asked for
and did not get — so a deny rule that does not parse refuses everything that
changes the machine until it is fixed, and says so. Reading still works, so the
agent can open the file and tell you what is wrong with it. `rook doctor` lists
them.

A deny rule is anchored twice: to the argument, so `rm -rf /tmp/scratch` is not
`rm -rf /`, and to the command position, so `grep -r mkfs docs/` is not running
`mkfs`. Nothing overrides a denial, which is why a rule that fires on a harmless
command takes that command away for good.

An allow rule has to cover **every** part, not one of them: `ls && rm -rf ~` is
not `ls`, and a write touching `src/main.rs` and `/etc/passwd` is not a write
under `src`. A line the matcher cannot take apart — one with a `$(…)` in it — is
asked about rather than assumed. Against a path, a plain rule lines up with a
directory boundary, so `src/` is not `notsrc/`; a regular expression is left to
say what it says.

A tool from an MCP server is asked about like anything else. Rook cannot see
what one does, and the protocol's `readOnlyHint` is the claim of the very party
whose behaviour is in question, so the claim is repeated in the prompt and never
acted on — rules match the namespaced tool name, so `allow = ["gh__"]` trusts one
server and a deny rule can take a single tool away.

File tools are bounded by the workspace, and the boundary is where a path leads
rather than how it is spelled: a symlink inside the workspace that points out of
it is refused, and the refusal names where it really went.

A command is contained by the platform where the platform can. On macOS it runs
under Seatbelt, on Linux under Landlock, and on Windows at a low integrity
level, and in all three it — and everything it starts — may write only the
workspace and the temporary directory, and read anywhere; the network is a
switch, `[sandbox] network`, on by default because a build fetches its
dependencies. `[sandbox] isolate = "auto"` is the default and does this where it
can; where it cannot — FreeBSD, for now — the command runs as it is and the
tool's result says so, and `"required"` refuses instead. What was applied is
recorded on every command, not assumed: a sandbox that quietly did nothing is
worse than none. Landlock older than kernel 6.7 cannot restrain the network at
all, and never restrains UDP, so DNS; a Windows integrity level never restrains
it; the result says that too. On Windows the workspace, and a scratch directory
of rook's own under the temporary directory, are given a low integrity label so
the command may write them — a persistent mark on the directory, and only that. A command run in an editor's
terminal is the editor's, and the language-server installer runs uncontained by
design, after the person said to.

### Delegation

A turn can hand self-contained sub-tasks to fresh agents and get back only their
conclusions. Each sub-agent runs in its own session with an empty context, so a
wide search or a long file survey never enters the conversation that asked for it
— and its full transcript stays readable afterwards. Several at once run
concurrently, bounded by `agent.max_parallel_subagents`. The list of sub-tasks is
written by the model and one entry is a whole agent's worth of turns, so the total
a turn may start — counting the ones its own children start — is capped by
`agent.max_subagents_per_turn`, and a sub-agent's step budget can only be shorter
than its parent's, never longer — and never below the three a task needs, a call,
a look at what came back and an answer:

```sh
rook session ls              # sub-tasks appear under ↳, linked to their parent
rook session show <child>    # everything the sub-agent actually did
```

Nesting stops at two levels, because past that the token cost compounds faster
than the work gets done.

### Memory

The agent can remember things across sessions, and you can read, correct and
audit what it believes:

```sh
rook memory ls                        # what applies in this workspace
rook memory search "how do deploys work"   # ranked, with why each matched
rook memory add --pin --global "never force-push to main"
rook memory since 7                   # what it learned or forgot this week
rook memory history                   # every recorded state
rook memory diff <objA> <objB>
```

Each fact carries where it came from — the session and turn that produced it —
so a wrong memory is traceable back to the turn that formed it. Every change
writes a new version, which is what makes `since`, `diff` and rollback possible.
Facts are scoped global or per-workspace, and only what matches the current
prompt enters the context, under a token budget.

### External tools

Any MCP server becomes a tool the agent can call. Declare it in
`~/.rook/config.toml`:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[[mcp]]
name = "hosted"
url = "https://example.com/mcp"
headers = { Authorization = "Bearer …" }
```

A `command` is spoken to over its pipes; a `url` over HTTP, which may answer
either with JSON or with an event stream.

```sh
rook mcp ls                              # connect everything, report what it offers
rook mcp tools filesystem                # its tools and their arguments
rook mcp call filesystem read_file '{"path":"a.txt"}'   # no model in the loop
```

Servers connect concurrently and a failure is reported without stopping the turn —
one misconfigured server must not cost you the working ones. Tools are namespaced
`server__tool`.

### Skills

```sh
rook skills ls                 # what applies here, and what loading each would cost
rook skills why deploy         # which version was chosen, and why the others were not
rook skills new my-skill -d "…"
rook skills capture my-skill -m "first version"
rook skills history my-skill
rook skills diff <objA> <objB>
rook skills rollback my-skill <obj>
```

Skills arrive three ways: written by hand, written by the agent — `write_skill`
takes the files a procedure needs, so a python helper and the instructions that
call it land together — or installed from a source with `rook skills search` and
`rook skills install`. However one arrives, a request carries its one-line card
and not its body; `load_skill` fetches that when the model asks.

A skill is a directory with a `SKILL.md` — the
[Agent Skills](https://www.webfuse.com/agent-skills-cheat-sheet) format, so skills
written for other agents work unchanged. A directory under `~/.rook/plugins` with
a `plugin.json` is an Agent Plugin, and brings its `skills/` and its `mcpServers`
together; see [docs/skills.md](docs/skills.md). One vendored into a workspace
under `.rook/plugins` brings only its skills: a repository is not the person
running the agent, and an `mcpServers` entry is a command that would be spawned
at session start. The ones you want go under `[[mcp]]` in your own config, and
the skipped ones are named on start so you know which. Rook adds two optional blocks:

```yaml
---
name: in-place-edit
description: Edit files in place across platforms.
version: 1.2.0
requires:                        # gates the whole skill
  language: { rust: ">=1.85" }
  tool: { git: ">=2.30" }
variants:                        # swaps only the body
  - when: { userland: [bsd] }
    body: variants/bsd.md
  - when: { os: [windows] }
    body: variants/windows.md
---
```

`requires` is why `rook skills why` can tell you a skill is inert because you are
missing Docker 27, instead of leaving you to guess. See
[docs/skills.md](docs/skills.md).

### Standing instructions

A skill is loaded when it is wanted. What holds for every turn goes in an
`AGENTS.md` — the file codex, opencode and others already read — either in the
workspace or in `$ROOK_HOME` for what applies everywhere:

```
~/.rook/AGENTS.md      # yours, wherever you work
<workspace>/AGENTS.md  # this project's, and it has the last word
```

Both are read whole into the system prompt, most general first, capped at
`[agent] max_instructions_bytes` each — it is paid for on every request and the
project's copy is written by whoever sends the pull request. A cut is stated in
the prompt rather than made silently, since instructions that stop mid-sentence
read as instructions that end there. `rook doctor` lists what it found. The shipped `project-instructions` skill says
what belongs in one and what does not — a long one is worse than none, since it
crowds out the conversation on every request.

## Layout

```
crates/
  rook-store    content-addressed store: redb index, zstd dictionaries, gc, retention
  rook-skills   SKILL.md parsing, environment detection, version + variant resolution
  rook-core     the engine: config, agent loop, context budget, file captures
  rook-llm      provider trait, OpenAI-compatible HTTP and the Anthropic Messages API
  rook-tools    read/write/edit/list/search/run, with the guards that keep a turn survivable
  rook-mcp      Model Context Protocol client: stdio and HTTP transports
  rook-lsp      Language Server Protocol client: diagnostics and navigation
  rook-acp      Agent Client Protocol server, so editors can drive it
  rook-proto    wire types shared by daemon, CLI and web
  rookd         HTTP backend, chat websocket, embedded web UI
  rook-cli      `rook`: commands and the terminal browser
web/dist        the web UI: one hand-written HTML file, no build step
docs/           architecture, storage format, skills, platforms, ADRs, research
references/     upstream agent sources as shallow submodules, to read from
```

`references/` is not fetched by a normal clone. `cargo xtask refs status` shows how
far each pinned pointer has drifted from upstream — that gap is the backlog of
upstream work nobody has looked at yet. See [references/README.md](references/README.md).

## Platforms

Linux, macOS, Windows and FreeBSD. FreeBSD is built **and tested** in a real VM in
CI rather than cross-checked, because the two dependencies that vendor C are
exactly what a cross-check cannot exercise. Nine targets are claimed; `cargo xtask targets` prints which are tested, which
are only compiled, and which are best effort, and a test fails if a row claims
more than CI actually does. Details: [docs/platforms.md](docs/platforms.md).

## What is not done yet

Being explicit, because a roadmap presented as a feature list is how these projects
lose people's trust:

- **A capable model has driven it once, locally.** Five scenarios against a
  27-billion-parameter model in LM Studio pass on the first attempt — reading,
  editing, using what a command printed, delegating, and refusing to settle a
  false claim from memory — with none of the nudges the loop keeps for smaller
  models needed. CI runs the same five against a 3-billion-parameter model on
  every push, where they mostly fail for the model's own reasons; reading those
  transcripts has found a dozen defects here that no scripted answer would have.
  What is still unwatched is long work: five short scenarios are not an
  afternoon, and nothing here has run against a hosted model at all.
- **The CLI writes to the store directly**, so a command that changes it cannot
  run while `rookd` holds the lock. Every read routes over the daemon's API
  instead and prints the same thing; writes say where the lock is
  ([ADR-0006](docs/adr/0006-single-writer-store.md)).
- **Reasoning is carried across a tool call, and only for Anthropic.** It was
  not, and that was a turn Anthropic refuses outright: with extended thinking on,
  a tool call must come back beside the signed thinking block that led to it. The
  block is now kept whole — never parsed, never rebuilt, because a signature
  covers bytes — and replayed first in the assistant message for the rest of that
  turn. Earlier turns carry none, which is what the API expects. What it costs
  the other two dialects is nothing: they sign nothing and ask for nothing back.
  Whether a capable model reasons better for having its own thinking returned is
  still unmeasured here.
- **No structured plan state.** The agent is asked for a plan in prose and told
  not to keep a checklist — deliberately, on the strength of someone else's
  benchmark ([ADR-0010](docs/adr/0010-no-todo-tool.md)). There is nothing for a
  UI to render as progress.
- **Everything the agent reads and runs is stored in the clear.** A checkpoint
  keeps whatever was on disk, a `.env` included, and a tool result keeps whatever
  a command printed. Nothing leaves the machine, but nothing is encrypted either,
  and `rook store cat` prints any of it back. `rook search` finds where a secret
  ended up — it names the file and the capture — and `rook session rm` followed
  by `rook store gc` removes it.
- **Containment is real but partial.** A command the model asks for runs under
  Seatbelt on macOS, Landlock on Linux and a low integrity level on Windows: it
  writes the workspace and a scratch directory and nothing else, whatever it
  starts. It reads everywhere, this agent's own store included, and the network
  is open by default — so a command that is refused a write can still send what
  it read. FreeBSD has no containment at all and says so. The pattern rules over
  the command line are still pattern matching: `curl … | sh` is one obfuscation
  away from any rule, and they are what covers the platform that has nothing.
- **Undo covers what a tool declared, not what a command did.** A checkpoint
  holds the paths a tool said it would touch; a `run_command` says none, so what
  a shell command changes is outside `rook session rewind`. `delete_file` closes
  the case that cannot be recovered any other way — everything else a command
  writes leaves its content somewhere.

## Development

```sh
cargo xtask ci             # fmt + clippy + test, the gate CI runs
cargo xtask targets        # the supported target matrix
cargo xtask compaction     # measure the storage claims above
cargo xtask clean          # report what target/ costs and reclaim it
cargo xtask refs status    # how far the reference pointers have drifted
cargo xtask smoke --model … # four real turns against a real model
cargo test --workspace
```

A full debug build with tests is about 800 MB of `target/`. Debug info is
line-tables-only and dependencies carry none, because full DWARF for the
dependency graph costs several gigabytes and is never stepped through.

## License

MIT or Apache-2.0, at your option.
