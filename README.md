# Rook

[English](README.md) · [Русский](README.ru.md)

An autonomous agent whose memory you can actually read.

Rook is a general-purpose local agent — coding, research, automation — written in
Rust and shipped as two static binaries with no runtime. It stores everything it
does in a compact, content-addressed store, and it treats *inspecting* that store
as a feature rather than a debugging afterthought: a CLI, a terminal browser and a
web UI, all views over the same engine.

> **Status: young, and honest about it.** The storage layer, the skill system,
> the inspection tools, the agent loop, streaming, MCP, LSP and ACP are
> implemented and under test — including whole turns driven over a real socket
> against a server that speaks the provider's dialect. A capable model has now
> driven it end to end: read the file, take the checkpoint, make the edit, run
> the compiler, and answer a goal check — one model, a handful of turns, which
> is a beginning and not a track record. Nothing below describes something that
> does not exist, and what is missing is listed under
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
  on-disk total (index + objects)  :     4.02 MiB
  end-to-end (logical -> on disk)  :     5.8x
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

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/ASlava12/rook/main/install.sh | sh
```

Windows:

```powershell
irm https://raw.githubusercontent.com/ASlava12/rook/main/install.ps1 | iex
```

The script downloads the build for this machine, checks it against the release's
`SHA256SUMS` and refuses to go on if they disagree, and copies two binaries and
the built-in skills under `~/.local` — nowhere else, and with no privileges. It
does not edit your shell's configuration; it tells you if the directory is not on
your `PATH`. `ROOK_PREFIX` puts it somewhere else, `ROOK_VERSION` pins a release,
and the script is a hundred readable lines in this repository if you would rather
look before piping it anywhere.

From source, which needs a Rust toolchain and a C compiler (two dependencies
vendor C — see [docs/platforms.md](docs/platforms.md)); no Node, no Python, no
Docker:

```sh
git clone https://github.com/ASlava12/rook && cd rook
cargo xtask dist               # builds, packages the built-in skills, prints the sizes
```

Two binaries, no runtime and no shared libraries — 8.1 MiB and 7.4 MiB at the
time of writing, which `dist` prints so the number here can be checked rather
than believed.

Everything it keeps — sessions, memory, skills, downloaded language servers —
goes in one directory, because that is what somebody backs up, copies to another
machine and inspects when they want to know what the agent knows. On unix it is
`~/.rook`; on Windows `%LOCALAPPDATA%\rook`, Local rather than Roaming because a
roaming profile is copied to a server at every logon and this is gigabytes. An
install that already has `%USERPROFILE%\.rook` from an earlier version keeps
using it rather than being moved, and `rook doctor` says which one is in use.
`ROOK_HOME` moves the lot; `ROOK_CONFIG_DIR` moves `config.toml` alone, for a
configuration kept in a dotfiles repository — `secrets.toml` does not follow it,
being nothing but credentials.

Settings are read in three layers, each winning the keys it names over the one
before: the machine's (`/etc/rook/config.toml`, `%PROGRAMDATA%\rook\config.toml`),
the person's, and the project's (`<workspace>/.rook/config.toml`). Merged key by
key, so a project that sets one thing keeps everything else you chose.

The project's layer is read with less authority than the other two, because it
arrives with the repository — whoever wrote it is not necessarily whoever is
running the agent, which is the same reason `<workspace>/.env` is not read at
all. It may say how to work in this codebase: `max_steps`, `effort`,
`plan_first`, the compaction and budget settings. It may not say what the agent
is allowed to do, where its secrets are, what it may start, or which model its
conversation goes to. `sandbox.deny` and `sandbox.ask` are the exception and are
*added to* rather than replaced, because adding to them can only narrow — a
project can forbid `make deploy` here and cannot unforbid `rm -rf /`. What a
project asked for and did not get is listed by `rook doctor` rather than dropped
in silence.

Releases carry `x86_64` and `aarch64` builds for Linux (static, musl — they run
on the distribution you have rather than the one the runner had), `x86_64` for
Windows, and both architectures for macOS. FreeBSD is a supported target that
builds and tests in CI and has no published binary yet: build it from a clone.

## Use

```sh
rook init                                  # create ~/.rook, config, store
rook doctor                                # environment, what contains a command, model reachability
rook config check                          # what the file says that is not a setting, and which endpoints answer
rook models                                # what the configured endpoint serves

rook                                       # talk to it
rook run "summarise what changed in src/"  # one turn, streamed, for scripts
rook chat --session last                   # pick up where you left off here
rook session show last                     # `last` works wherever a session does
cargo test 2>&1 | rook run "why does this fail?"   # stdin joins the prompt
rook --json run "..." | jq .outcome.reply  # one object: reply, tokens, changes
                                           # exit 2 if the turn did not finish
rook tui                                   # the conversation, with ^p for everything else
rook checkpoint create before-refactor     # or `c` in the TUI's Checkpoints tab
rook docs add redis                        # read its documentation once, answer from it after
rookd                                      # http://127.0.0.1:7717 — web UI + API
rook daemon status                         # where it is, and whether it is this build
rook daemon restart                        # after an upgrade; names any turn it ends
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
/model [name]   which endpoint from `[models]` the next turn runs on
/stance [name]  how much latitude: readonly, assist or autonomous
/context        what this conversation costs, and of what
/skills [name]  what applies here, or one skill's body
/undo           rewind past the last exchange, files included
/rewind <seq>   rewind to a point in the transcript
/session  /mcp  /jobs  /diff  /new  /help  /quit
```

`/btw` answers from what the agent already knows — no tools, one call — and its
answer never enters the context the agent carries forward, though it is still in
the transcript. Ctrl-C stops the turn in flight without leaving; whatever it
already did stays in the log.

Typing while a turn runs steers it rather than waiting for it: what you send
reaches the model at its next step, so a turn heading the wrong way can be
corrected without throwing away what it has already done — and if the turn has
work out with sub-tasks, every one of them hears it too. The line is marked
`✓ taken up` when the turn actually puts it in a request, because a request
already sent cannot be added to and how much of a step is left is not something
the window knows: without the tick the wait has no visible end. In the TUI and
in the browser, which are the two front ends that can take input while one is
running.

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
workspace back as well as the conversation. `delete_file` and `move_file` exist for
that reason: `rm` and `mv` through the shell declare no path, so nothing is
captured and no rewind brings it back — every other change a command makes leaves the content somewhere,
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

On by default, and off means the tool is never offered rather than the call
refused — a tool the model cannot see is one it cannot decide to try:

```toml
[web]
enabled = false     # and nothing here reaches the network at all
```

`web_fetch` reports its risk as the address it is going to, so an allow rule can
name a host: `allow = ["https://docs.rs/"]`. A redirect that stays on that host
is followed and one that leaves it is reported instead — an approval named an
address, and following it elsewhere would spend that approval somewhere nobody
agreed to. HTML arrives as prose, script and
style dropped. What comes back is somebody else's writing on its way into the
model's context — not a fact and not an instruction, which is why the answer
always says where it came from.

`web_search` answers through DuckDuckGo unless told otherwise, because that is
the one that works with nothing set up — no key, no account, no service to run.
What the default costs is worth saying plainly: the query goes to their host,
and the results are read out of a page rather than an API, so their markup can
break the reading. The two alternatives each need something to be true first:

```toml
[web]
search     = "duckduckgo"               # the default: nothing to set up
# search   = "searxng"                  # your own instance: the query stays here
# search_url = "http://127.0.0.1:8888"
# search   = "brave"                    # hosted, with BRAVE_API_KEY in the environment
# search   = ""                         # none: `web_search` is not offered at all
```

Its risk is the engine's address rather than the query, so allowing your own
instance does not also allow somebody else's. An engine named without the key it
needs is offered as nothing at all — a tool that fails on its first call teaches
the model to stop asking.

### Secrets it can use and cannot leak

A password the agent needs is named in everything it sees and valued only
inside the tool, on the way out:

```sh
rook secrets add ssh_prod                 # typed, not echoed, never an argument
rook secrets add npm --source env:NPM_TOKEN    # or say where it already lives
rook secrets ls                           # names, sources, whether they answer
```

```
run_command { "command": "sshpass -e ssh deploy@prod 'systemctl restart api'",
              "secrets": ["ssh_prod"] }
```

The value becomes `$ROOK_SECRET_SSH_PROD` in that command's environment and
nothing else: not the tool call, not the session log, not the store, not what
compaction summarises, not what a sub-agent inherits, and not the next request
to the provider. Whatever the command prints comes back with the value taken out
of it, at the one place a tool's answer becomes context. The approval names the
secret a call would spend, so what is granted is visible.

`ssh` reads a password from a terminal and nowhere else, so a command naming one
secret is also given `SSH_ASKPASS`, `GIT_ASKPASS` and `SUDO_ASKPASS` pointing at
a helper that prints the variable it inherits — the value is in no argument, and
the helper is gone with the command.

Values live in `~/.rook/secrets.toml`, 0600, in the clear — the same property as
an unencrypted SSH key or `~/.aws/credentials`. A value that already lives in a
password manager stays there: `env:`, `cmd:op read …` and `keychain:service/account`
name where to get it. **No call returns a value** — not the CLI, not the API, not
the browser. There is no `show`.

What it does not do is stop a model printing one on purpose: a command that can
use a secret can echo it in an encoding no redaction catches.
[ADR-0013](docs/adr/0013-secrets-are-named-never-valued.md) says so plainly —
this stops the way secrets actually reach transcripts, which is by accident.
The same secrets are `rook secrets`, `/secrets` in a chat, and a tab in the
browser.

### Documentation it keeps

A model asked about Redis answers from what it was trained on, which was a year
old the day it shipped and says so nowhere. `docs` is the alternative: it looks
for a copy kept on this machine, and when there is none it searches for the
official documentation, reads a few pages, and files the reading — not the page.

```
docs { "topic": "redis", "question": "how does persistence work" }
docs { "topic": "postgres", "version": "16" }
docs { "topic": "redis", "page": 2 }        # one of them whole
```

What is stored is the prose and, beside every passage, the address it was read
from — so an answer carries both: the local copy it was made of, and the page
anybody else can check it against. A copy is one per topic and version, `latest`
when none is named, and reading a topic again replaces it rather than growing a
second. The gathering is bounded like everything else that accumulates:

```toml
[web]
docs_pages = 5          # how many pages one topic may cost
docs_bytes = 200000     # and what a kept set may hold, applied as it arrives
```

A copy goes out of date when its source says so, not when it gets old: each page
is kept with the `ETag` and `Last-Modified` its server gave, and `--refresh`
asks for them back with those — a page that has not moved answers 304 with no
body, and only what changed is read again. A pinned version does not go stale by
getting older, and `latest` can be wrong the day after it was read; neither is a
question a timestamp can answer.

A topic with no set of its own is answered from a set beside it when there is
one — asking about `redis` finds the `redis persistence` gathered an hour ago
instead of fetching the same site again — and the answer says which set it came
from.

The same sets are `rook docs ls | show <topic> | add <topic> | rm <topic>`, the
Docs tab in the TUI, and a tab in the browser — the reading is the agent's, and
what it read is yours to see.

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

A claim that failed, and holds once the turn has written something, is reported
`unproven` too, naming the file that changed. Asked to verify that `add` returns
the sum of its arguments, a model read the file, was told it subtracts, edited
until it added, asked again, and called the claim verified — which answers a
different question from the one that was put. The loop remembers which claims
failed and what had been written when they did, so the second verdict is weighed
against the first rather than replacing it. A claim that failed because the
checker looked in the wrong place, and holds once it looks properly, is an
ordinary pass: what disqualifies the second answer is the turn having written
something in between.

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

Connections to the same project share an engine; different projects have their
own engines, sharing one history, one memory and one search. How many are kept is
`[server] max_projects`, because how many a daemon is asked for is decided by
whoever connects. Only idle engines are evicted; when all slots are held by
connections or turns, a new project is refused until one becomes idle.

The daemon accepts loopback `Host` authorities and checks browser `Origin` against
that authority. For a reverse proxy or remote address, explicitly list the exact
trusted `host:port` values in `[server] allowed_hosts = ["rook.example:8443"]` and
restart the daemon after changing this list.

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
# compaction_model = "ollama/qwen3:8b"   # condensing a span is not judgement
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

A local runtime does not have to be on this machine: `OLLAMA_HOST` and
`LMSTUDIO_HOST` point those two somewhere else — `LMSTUDIO_HOST=http://192.168.1.46:1234`
is a model on the desk next door — and `ROOK_LLM_BASE_URL` does the same for
`openai-compatible`. An endpoint on your own network is reached directly whatever
`http_proxy` says, because a proxy in the environment is a proxy to the internet:
sent through a VPN, a request to the next desk comes back as whatever the tunnel
makes of an address it cannot route to. A key is still refused over plain http to
anything but this machine, own network or not.

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

One variable per provider means one endpoint per provider, which is not how a
machine with a model on it and a paid API behind it is actually set up. Endpoints
can be named instead, and then `model` is a name from that table:

```toml
[agent]
model = "desk-large"               # a name below, or a provider/model spec

# Where to send a request. One server usually serves several models, so the
# address, the key and the queue are written once.
[endpoints.desk]
api = "openai"                     # openai | anthropic | google — the dialect
url = "http://192.168.1.100:8080/v1"
key = "secret:desk"                # secret:<name> | env:<VAR> | the value itself
parallel = 2                       # requests at this server at a time; 0 for no limit
key_in_the_clear = true            # this one is on our network, not on the internet

# What to ask it for.
[models.desk-large]
endpoint = "desk"
model = "qwen3-30b"
context_window = 120000            # guesswork for anything self-hosted
priority = 1                       # where it sits in the fall-through; absent means never

[models.desk-small]
endpoint = "desk"                  # the same queue: what interleaves is
model = "qwen3-4b"                 # requests at the process, not at a model
priority = 2

# A source may carry its own address instead, which is one server one model.
[models.laptop]
api = "openai"
url = "http://127.0.0.1:1234/v1"
model = "qwen3-coder:30b"
priority = 3
```

`parallel` is the reason the two halves are apart, and it defaults to one. A
hosted API serves whatever it is sent; llama.cpp and LM Studio hold one model and
serve a second request by interleaving it with the first, so two sub-agents
against one local server finish later than the same two run in turn — and there
is no status for that, only a request that takes minutes and reads as a hung
turn. It is counted across the process, so a sub-agent, a compaction and a second
window all queue in the same line.

An endpoint that cannot be reached is dropped from the rotation with a line in the
log and the next `priority` is used; `rook models --recheck` puts it back, for a
balance topped up or a server switched on. Home, work, and the machine at home
being off are three sets of reachable endpoints and one file — which is what
`priority` is for, and why a source without one is used only when it is named
outright: a paid gateway should not become what the agent reaches for because the
desk machine is asleep.

How long the agent waits for a model is one number: `[agent]
stream_idle_timeout_secs`, how long an endpoint may be silent. The wait for a
*first* token adds what reading the prompt should take, because a local model
filling a large context is silent for minutes by design, and nothing else bounds
it. Nothing else may: a deadline on the whole request is a clock on the answer
rather than on the silence, and the ten-minute one that used to be here cut a
reply that was arriving a token at a time and reported it as `operation timed
out` — which reads as the endpoint having gone away, and was read that way. A
reply that is still arriving is not late.

While it waits it says so, in every front end: `waiting on the model — 4m20s so
far, up to 21m30s`, after twenty seconds and every half minute after. That line
is the whole difference between a model reading a long prompt and a tunnel that
has dropped, which are otherwise the same blank screen.

Keys can stay out of the file. `secret:<name>` reads `rook secrets`, which keeps a
value at 0600 or refers it out to a keychain, a password manager or a command;
`env:<VAR>` reads this process's environment, which is where every key was before
this table existed. The value itself is allowed and is the last of the three on
purpose: `config.toml` is not `secrets.toml` — it is not 0600, it is the file
people paste into an issue, and it is the one that ends up in a dotfiles
repository. A `provider/model` spec still reads its key from the environment and
nothing about it has changed.

Some APIs are reachable only through a proxy, and on the same machine others
must not go near one, so it is written down per part rather than set once:

```toml
[proxy]
url = "socks5://127.0.0.1:1080"    # http:// | https:// | socks5:// | socks5h://

# Each part takes the line above unless it says otherwise. `direct` is how one
# part opts out of a proxy the others need.
models  = ""                       # the model APIs
web     = "direct"                 # web_fetch and web_search
mcp     = ""                       # MCP servers reached over http
install = ""                       # downloading a language server

[endpoints.paid]
url   = "https://api.example.com/v1"
proxy = "http://gateway:3128"      # this one endpoint, whatever `models` says
```

Empty is not "no proxy" — it is "nothing said here", and then `http_proxy` and
`no_proxy` decide, exactly as they did before any of this existed. `direct` is
how to mean no proxy.

Whatever any of it says, a request to this machine or this network goes
straight out: a proxy is a way to the internet, and sent through one a request
to the desk next door comes back as whatever the tunnel makes of an address it
cannot route to. An MCP server this machine starts and talks to over pipes has
no network between the two and never sees a proxy at all.

`rook config check` reads the file, names anything in it that is not a setting,
and asks every endpoint whether it is there. `rook config set agent.model
desk-small` changes one line and leaves the comments around it alone.

`rook models` asks the endpoint what it serves. Effort applies where the provider
has the notion; sub-agents and `/btw` run at `low` regardless, since a bounded
errand does not need the depth the main turn does.

### Working at one goal for longer than a turn

A turn ends, and something has to decide whether there is another one. If that
something is the model that just did the work, a long run is a model marking its
own homework until the budget is gone — so a project writes down what it is
judged by, and the harness runs it:

```toml
# .rook/evaluation.toml — the person's, not the agent's
[[check]]
name = "tests"
run  = "cargo test --workspace"
guards = ["crates/*/tests/**"]   # changing these is allowed, and is reported

[[check]]
name = "coverage"
run  = "cargo llvm-cov --json | jq .data[0].totals.lines.percent"
measures = "coverage"            # watched rather than gated on
```

```sh
rook eval                        # run them and say what they said
rook work "make the tests pass"  # turns at one goal until they do
rook work "keep the docs current" --keep-going --tokens 2000000
rook work --resume               # carry on the last run in this workspace
```

`rook eval` is a command and not a tool: the model cannot call it, because an
agent that could run its own evaluation could run it until it passed. Real
independence is not available to a coding agent — it has to be able to edit the
repository, and the tests are in the repository — so what the scorecard buys is
a witness. The guarded files and the scorecard itself are hashed before the work
and again after, and a check that went green in the same iteration that rewrote
it says so beside the pass instead of disappearing into it.

Between iterations the loop reads what the harness measured and what the
filesystem says changed, and never the turn's account of itself. It names a
regression before anything else — a model handed only the current state cannot
tell a repair from a break it has just caused — carries the end of a failing
check's output into the next prompt, and stops when two iterations in a row
change nothing. Each iteration is its own session: seventy turns in one
conversation is a context nobody can afford, and what has to survive between
them is the workspace, the checks, and `.rook/plan.md` — the agent's own file,
where it keeps what is done, what is next, and what it tried that did not work.
Rewriting that plan is not doing the work, and does not count as a change.

The active iteration, original goal, scorecard and limits are saved before work
starts. `rook work --resume` reuses that session, completed answer and evaluation
receipts, including when the first iteration was interrupted. New CLI budget
flags do not replace the saved plan. Completed checks are not rerun; changed
guarded files make a reused result unproven. A check selected before a crash but
without a saved result requires inspection and a new work run to evaluate again.
Known token usage includes pre-crash turns and delegated sessions.

### Images and embedded context

Select a vision-capable model, then attach local files explicitly:

```sh
rook run --image screenshot.png --context component.ts 'Explain this UI defect'
```

In the REPL or TUI, `/attach-image PATH` and `/attach-context PATH` queue files
for the next submitted turn; `/attachments` shows the count and `/attachments
clear` removes them. The browser chat has a multiple-file picker. Image input
uses OpenAI-compatible content parts, Anthropic image blocks or Gemini inline
data; a text-only model may reject the request, and Rook does not retry it with
the images removed.

Up to four attachments per turn: PNG, JPEG, WebP or GIF images up to 2 MiB each
and 4096 pixels per side; UTF-8 context up to 256 KiB combined. Unsupported
binary documents, audio and remote image URLs are rejected. File names, embedded
text and text within images are source data, never grants of permission. ACP
advertises `image` and `embeddedContext`, accepting image blocks, text resources,
image blobs and UTF-8 text blobs. Resource links remain references, without an
automatic network fetch. The WebSocket and ACP frame ceiling is 16 MiB.

Images are stored with their user message, so resume and forks replay the same
bytes without reopening the original path. Transcripts show names and context,
not base64. Context accounting includes image estimates. Compaction preserves
an attached image until the model first answers it and bounds a model request to four images; older images
are replaced by the text history and a notice that their pixels are no longer
in context. Reattach an old image when its visual details matter. Stored history
still retains its original bytes under normal session retention.

The chat API accepts `options.attachments`, for example:

```json
{"attachments":[{"type":"image","name":"screen.png","mime_type":"image/png","data":"BASE64_BYTES"},{"type":"text","name":"component.ts","text":"source code"}]}
```

### Run recipes

A recipe packages a repeatable task without changing the skill format. Save
`.rook/recipes/audit.toml`, then select it explicitly:

```toml
version = 1
prompt = "Audit {{scope}} for {{focus}}. Report evidence and unchecked areas."
# skill = "your-audit-skill"       # must already be installed and applicable
# model = "local-audit"           # current model or a configured [models] name
output = "{{report}}"
output_schema = { type = "object", required = ["findings"], properties = { findings = { type = "array" } } }

[parameters.scope]
description = "Directory or component to inspect"
[parameters.focus]
default = "correctness"
[parameters.report]
default = "report.json"

[limits]
steps = 40
tokens = 100000
seconds = 1800
```

```sh
rook run --recipe audit --param scope=crates --yes
rook run --recipe .rook/recipes/audit.toml --param scope=web "Focus on input handling"
```

`/recipe audit {"scope":"crates"}` selects it for subsequent chat/TUI turns;
`/recipe off` clears the selection. The browser exposes the same name and JSON
parameter fields. API clients set `options.recipe` to
`{"path":"audit","parameters":{"scope":"crates"}}` on a prompt.

Parameters without defaults are required. Missing or unknown parameters, invalid
skills, schemas or paths fail before the turn runs. Templates expand once, with
parameter values quoted in the procedure. Recipes and parameters remain source
data; selecting a recipe grants no new tool permissions. A recipe's output file
uses the normal write approval policy (`--yes` grants what the deny list permits);
an explicit `--output` overrides that destination. Explicit output/schema options
win over recipe defaults. Repair attempts use the ordinary `--schema-retries`.

Limits are positive and only tighten existing turn limits. Model names cannot
introduce endpoints or credentials: configure those in Rook first. Recipe files
and supplied parameters are each limited to 64 KiB, with at most 32 parameters.
Files must stay within the workspace; remote recipes and executable template
expressions are not supported. The additional prompt augments the selected task.

### Interrupted executions

Rook durably records an execution owner and each operation's intent before it
runs, then its result. On restart, unfinished operations that may have effects
are reported as **unknown**, not failed or successful. Commands are never
replayed automatically. Reading remains available when policy permits it; changes
in the affected session and its delegated family pause until inspection.

```sh
rook session recovery <session>
rook session recovery <session> --acknowledge <operation-id> --note "Inspected the destination and command output; ..."
```

In chat/TUI use `/recovery` and `/recovery <operation-id> <inspection note>`.
The browser's session view exposes the same receipts and inspection action.
API clients use GET/POST `/api/sessions/{id}/recovery`; POST accepts `operation`
and `note`. Acknowledgement records what the user inspected, clears that specific
pause, and neither claims success nor repeats an operation. Stale operation IDs
are rejected. Stop an active turn before acknowledging it.

Receipts distinguish pending operations, background jobs, completed operation
counts and the last result event. Full stored results remain available through
`read_result`. Hooks, installers and harness-owned output/evaluation writes
participate in recovery too. A background job left by a lost owner may still
have affected external state; inspect its destination before continuing.
Session forks retain uncertainty. Ordinary prune/maintenance protects active
executions, unknown results and unfinished work iterations; explicit session
deletion remains available after inspection.

This is a recovery journal, not a transaction with the outside world: a crash
between an effect and its durable receipt cannot prove whether the effect happened.
Existing sessions created before this feature have only their older logs.

### From an editor

`rook acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com) on
stdio — the same protocol Zed, JetBrains and Neovim already use — so no plugin is
needed per editor. Streamed output becomes `session/update`, and the permission
policy becomes the editor's approval dialog: the same decision, reaching the same
rules, whichever front end asks.

### What it is allowed to do

By default the agent asks before anything that changes the machine, and refuses
outright what the deny list forbids — no approval can override a denial:

An approval and a question are bounded differently, because being unanswered
means different things. An approval nobody answers is denied: the work stops
and nothing was changed, which is the safe end of a wait. A question is asked
because the decision is real, and a turn that stops on one throws away
everything it did to reach the point of asking — so after `[agent]
decide_alone_after_secs`, half an hour by default, the turn takes it back:
it weighs the options it offered against each other, chooses the one that best
serves the goal, and says in its reply which it chose and what it weighed, so
whoever was away can see what was decided for them and change it. It does not
put the same question again; one turn once spent a hundred and ninety-four
steps doing that.

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
they cost at most twice it. A spawned daemon's own stderr goes beside it, to
`rook-stderr.log`, and that file is empty unless something wrote where `tracing`
could not — a panic aborts the release build without a word, so the reason is
there or nowhere. Nothing is uploaded anywhere; `telemetry.upload` exists so
that answer is findable rather than assumed.

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
under `src`. Automatic allow-rules do not cover shell lines containing `$`,
backticks, redirects (`<` or `>`), or backslashes, even inside quotes. Options
that execute other commands or write output, such as `find -exec` and `rg --pre`,
also prevent automatic matching. In `assist` these commands need approval; the
prompt explains why the allow-rule did not apply. In `autonomous`, the stance
still applies, subject to deny and ask rules. Against a path, a plain rule lines up with a
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

Where `[models]` names more than one endpoint, an errand goes to whichever is
free rather than to the one the turn was pointed at. A turn wants the endpoint
it started on — moving it part-way throws away the cached prefix of every request
it has sent — and an errand is bounded work with no conversation worth caching,
so a sub-agent queued behind its own parent on a server that takes one request at
a time is waiting for the turn it was started to get ahead of. `delegate` also
takes a `model`, for the sub-task that wants the large one or must not go through
the paid one; the field is offered only where there is a choice to make, and a
name that is not configured is refused with the real ones listed.

By default a sub-agent works in the same directory as the turn that started it, and two
sessions are already refused a write to the same file at the same time — the
second is told which session holds it. What a shared directory does not protect
is the branch: a child that commits carries its parent's unfinished work along
with its own, and one that checks out or resets changes what the parent is
editing while it edits. So a sub-agent is refused `commit`, `checkout`,
`switch`, `push`, `reset`, `rebase`, `merge`, `stash` and their neighbours, and
told what to do instead. Reading is untouched, since that is what most errands
are for — `status`, `diff`, `log`, `show`, `blame`, and the listing halves of
the ones that have two, `git branch` and `git stash list`. Adapted from
[OpenResearch](references/README.md).

For independent implementations, the model can call
`delegate({"tasks":["implement an alternative"],"isolation":"worktree"})`.
Each child gets a detached Git worktree from `HEAD`, its own local tools and
language-server pool. Both waiting and `wait:false` delegation support this.
The source workspace must be the clean repository root, including untracked
files; unsaved editor buffers and parent MCP/editor connections are not copied.
Ignored files, build artifacts and initialized submodules are not copied either.
Builds may therefore need separate setup. An isolated child’s background jobs stop
when that child ends. Worktrees isolate file changes; they
are not an additional security sandbox.

The child report names its session and retained path under Git's common directory,
in `rook-worktrees/`. The parent's `worktree` tool offers `status`, `diff`,
`read` (with `path`, line `offset` and `limit`), and `remove`. New files appear
in the diff response's `untracked` list. Review the alternative, read the files,
and transfer selected edits with the ordinary editing tools; no automatic merge
or commit occurs. Removal refuses dirty worktrees unless `discard:true` explicitly
requests discarding them, and still obeys the write policy. Parent rewind leaves
isolated children intact. `[agent] max_worktrees = 8` bounds retained copies per
repository; zero disables creation. Clean up reviewed trees explicitly. After a
process crash, `git worktree list` and `git worktree remove <path>` provide recovery.

### Old tool results and full output

Old results can leave the model's context while remaining in the session log.
By default Rook protects the latest eight results and at least 8,000 recent result
tokens, then clears an older batch only when the estimated saving reaches 8,000
tokens. Human answers and loaded skills are protected. Each batch changes the
cached conversation prefix once; its persistent watermark keeps subsequent
requests stable. This runs before summarizing compaction and makes no model call.

```toml
[agent]
prune_tool_results_min_tokens = 8000  # 0 disables further pruning
prune_tool_results_keep_tokens = 8000
max_replayed_result_tokens = 1000   # replayed result head/tail view; 0 keeps it whole
max_worktrees = 8
```

Results carry a numeric `result_id`. The model can use
`read_result({"result_id":42,"offset":0,"limit":16384})` for the complete
stored answer, or add `"source":"output"` for a command's captured stdout/stderr.
Pages return byte offsets and `next_offset`; omitting `result_id` lists recorded
result IDs, including before compaction (listing offsets are event numbers).
Recovered pages are not shortened again before the model sees them.
An optional `session` selects a direct child, so the parent can inspect its command results.

Full command capture is bounded by `[sandbox] max_spill_bytes` (64 MiB by default),
and maintenance retains `max_output_files` (100 by default). Known resolved
secrets are redacted before spill bytes reach disk, including values split across
pipe reads. A capture that reaches its limit is explicitly incomplete. Background jobs also capture output; read `job` first to obtain its result ID,
then request `source: "output"`. Running or stopped jobs report incomplete capture.
Editor-owned terminals retain only what their runner captures; the pager cannot
recover bytes the editor discarded. Context pruning itself deletes
neither recorded results nor command output files.

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

A skill is loaded when needed. Project conventions can be kept in
`$ROOK_HOME/AGENTS.md` or `<workspace>/AGENTS.md`. Both are read as **reference
data**, outside the system message, capped at `[agent] max_instructions_bytes`
each. Truncation is explicit. `rook doctor` shows their canonical paths,
content hashes and current trust status.

Files, comments, web pages, command and MCP results, skill catalogs, hook context,
memory and summaries are not new user instructions. The harness supplies JSON
`rook_source` records with provenance and authority; embedded role labels or
nested records do not change that authority. Tool permissions are enforced
separately. Actual answers through the built-in `ask` tool are distinguished
from the echoed question and timeout notices.

To explicitly trust reviewed project or skill instructions, add a content pin
to your **user configuration** (`$ROOK_HOME/config.toml`):

```toml
[agent.trusted_sources]
"/canonical/absolute/project/AGENTS.md" = "<body_blake3 from rook doctor>"
```

The default map is empty. Pins match the canonical absolute path and BLAKE3 of
the exact rendered instruction body. For skills, use `origin` and `body_blake3`
from the `skill` event's `rook_source` record in `rook session show`:
that hash includes the rendered skill identity and bundled resource listing,
so it is **not** the hash of raw `SKILL.md` bytes. Review the content before
adding a pin. A changed or truncated body loses trust; removing a pin also
revokes trust when history is replayed. Trust is scoped to the workspace where
the source is presented and never grants tool permissions or overrides the
user's request. Older skill history without provenance remains data.

This reduces prompt-injection exposure; it does not make an LLM immune to it.
For defensive analysis, instructions found inside suspicious material are
evidence to inspect. A refusal or blocker is reported as `blocked` (incomplete),
not a successful audit. Completion classification is model-based and can err.

## Layout

```
crates/
  rook-contain  platform and capability filesystem operations; no internal dependencies
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

- **A capable model has driven it once, locally.** Eight scenarios against a
  capable model in LM Studio pass — reading, editing, using what a command
  printed, delegating, refusing to settle a false claim from memory, moving a
  file rather than retyping it, writing a skill and loading it in a later
  session, and remembering a fact into one. The first five passed on the first
  attempt with none of the nudges the loop keeps for smaller models needed, and
  the three added since are the only live evidence that skills and memory
  survive the session that wrote them. CI runs the same eight against a
  3-billion-parameter model on every push, where they mostly fail for the
  model's own reasons; reading those transcripts has found a dozen defects here
  that no scripted answer would have. What is still unwatched is long work:
  eight short scenarios are not an afternoon, and nothing here has run against a
  hosted model at all.
- **A turn needs the store to itself, and `acp` is the one that still says so.**
  `rookd` holds the store's single write lock, and every `store`, `session`,
  `skills`, `memory` and `checkpoint` subcommand routes over its API rather than
  refusing — the same call the daemon makes on its own store, so there is one
  implementation and two ways in. A turn cannot route the same way, because it
  writes as it runs; `run` and `chat` go to the daemon's chat socket instead,
  which is the same engine and the same conversation from the other side, and
  say which daemon they are using. `rook acp` is what is left: an editor driving
  it while `rookd` is up meets the lock and is told so
  ([ADR-0006](docs/adr/0006-single-writer-store.md)).
- **Reasoning is carried across a tool call, and only for Anthropic.** It was
  not, and that was a turn Anthropic refuses outright: with extended thinking on,
  a tool call must come back beside the signed thinking block that led to it. The
  block is now kept whole — never parsed, never rebuilt, because a signature
  covers bytes — and replayed first in the assistant message for the rest of that
  turn. Earlier turns carry none, which is what the API expects. What it costs
  the other two dialects is nothing: they sign nothing and ask for nothing back.
  Checked against a socket replaying that API's documented shape, and against
  the live API never: nobody here has a key. So the shape is right by
  construction and unconfirmed in practice, which is worth saying plainly
  because it is the one dialect where getting it wrong fails every turn.
- **No structured plan state.** The agent is asked for a plan in prose and told
  not to keep a checklist — deliberately, on the strength of someone else's
  benchmark ([ADR-0010](docs/adr/0010-no-todo-tool.md)). There is nothing for a
  UI to render as progress.
- **Everything the agent reads and runs is stored in the clear.** A checkpoint
  keeps whatever was on disk, a `.env` included, and a tool result keeps whatever
  a command printed. Nothing leaves the machine, but nothing is encrypted either,
  and `rook store cat` prints any of it back. `rook search` finds where a secret
  ended up — it names the file and the capture — and `rook session rm` followed
  by `rook store gc` removes it. What protects it is the state directory's mode:
  created for the owner alone, and left as it is if it already existed, which is
  how a `~/.rook` made by an older build or a shell stays readable by every
  account on the machine. `rook doctor` says which directories those are and the
  `chmod` that closes them.
- **Containment is real but partial.** A command the model asks for runs under
  Seatbelt on macOS, Landlock on Linux and a low integrity level on Windows: it
  writes the workspace and a scratch directory and nothing else, whatever it
  starts. It reads everywhere else, because a build needs its toolchain — with
  one exception, the agent's own state directory, which holds every project's
  transcripts and checkpoints and is kept out of reach on the two platforms that
  can; an integrity level restricts writing, so on Windows it is not, and the
  result says which. The network is open by default, so a command refused a
  write can still send what it did read. FreeBSD has no containment at all and
  says so. The pattern rules over the command line are still pattern matching:
  `curl … | sh` is one obfuscation away from any rule, and they are what covers
  the platform that has nothing.
- **Undo covers what a tool declared, not what a command did — and now says
  which.** A checkpoint holds the paths a tool said it would touch; a
  `run_command` says none, so nothing holds what its files were before and
  `rook session rewind` cannot put them back. What it no longer does is answer
  in silence: after a command the workspace is walked for what was written
  since it started, and those paths are named in `session changes`, in `/diff`
  and in the browser, apart from the diffable ones and counted in the total. A
  turn that ran `sed -i` used to report no files changed at all. mtime is what
  that walk reads, so a command that rewrites a file with the same bytes is
  listed too, and a workspace too large to walk says so rather than reporting
  nothing.

## Documentation

Everything here is in this repository; nothing is on a website that can rot
separately from the code.

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | the crates, what each owns, and which way the dependencies run |
| [docs/storage.md](docs/storage.md) | the store: addressing, dictionaries, retention, garbage collection |
| [docs/skills.md](docs/skills.md) | writing a skill, the frontmatter, what gates one |
| [docs/platforms.md](docs/platforms.md) | supported targets, and the two C dependencies that constrain them |
| [docs/roadmap.md](docs/roadmap.md) | what exists, what is next, what is deliberately not being built |
| [docs/adr/](docs/adr/) | thirteen decisions, each with the failure that prompted it |
| [docs/research/agent-landscape.md](docs/research/agent-landscape.md) | the public failures the design answers, with citations |
| [references/PORTED.md](references/PORTED.md) | every pass over another agent's history: what was taken, and why the rest was not |

The ADRs are the ones to read if you want to know *why* rather than *what*:
[0003](docs/adr/0003-agent-skills-format.md) on using somebody else's skill
format, [0009](docs/adr/0009-ask-before-acting.md) on ask-before-acting,
[0010](docs/adr/0010-no-todo-tool.md) on the planning tool that measurement
declined, [0011](docs/adr/0011-containment-is-the-platforms.md) on containment
being the platform's job, and
[0013](docs/adr/0013-secrets-are-named-never-valued.md) on secrets.

## Development

```sh
cargo xtask ci             # fmt + clippy + test, the gate CI runs; prints what each took
cargo xtask dist           # release binaries, the skills beside them, and the sizes
cargo xtask targets        # the supported target matrix
cargo xtask compaction     # measure the storage claims above
cargo xtask load           # time the per-turn work; --scale 2 asks whether it is linear
cargo xtask clean          # report what target/ costs and reclaim it
cargo xtask refs status    # how far the reference pointers have drifted
cargo xtask smoke --model … # eight real scenarios against a real model
cargo xtask bench --model … # arms that differ by one variable, scored from the workspace
cargo test --workspace
```

The gate prints how long fmt, clippy and the tests each took, whether it passes
or fails: a run that died after four minutes of building and one second of
clippy is a different morning from one that died at once. A run far off the
usual is a question rather than a day to sit through — twenty-minute runs here
turned out to be cold ones, with over a million files accumulated in
`target/debug/deps`, which is what `cargo xtask clean --all` exists for.

`cargo xtask load` is the answer to "this feels slow": it times the parts a turn
pays for again every step — appending to the log, replaying it, the catalog, the
prompt, a search — against a synthetic session, and prints a cost per unit beside
each. Its first run found appending one event costing nine milliseconds, of which
eight were a flush to disk; nothing in the code says that, and reading it would
not have.

A full debug build with tests is about 800 MB of `target/`. Debug info is
line-tables-only and dependencies carry none, because full DWARF for the
dependency graph costs several gigabytes and is never stepped through.

## License

MIT or Apache-2.0, at your option.


### Final-answer artifacts and structured output

`rook run --output report.md "Audit this project"` writes the last answer itself,
without relying on a model tool call. The path is inside the session workspace;
the existing parent directories must not contain symlinks. Writes are atomic,
checkpointed for `session rewind`, and reported as changes. A failed write exits
with an error. An interrupted turn can save its available answer, but still
exits as incomplete.

`--output-schema schema.json` validates that answer locally against JSON Schema.
`--schema-retries 2` allows two format-only repair requests (0–3); those requests
have no tools and consume the same turn token/time budget. Exhaustion or invalid
output is an error and does not replace an existing output file. Schemas are at
most 64 KiB; local `$defs` references work, network/file references are disabled.
This checks structure, not the truth of the report. Streamed drafts can precede
the validated final answer; the output file contains only the final answer.

In chat/TUI use `/output report.md`, `/schema schema.json`, and
`/schema-retries 2`; `/output off` and `/schema off` clear them. These choices apply
to subsequent turns in that window. The browser exposes the same controls under
“Result format and file”. API clients pass an `options` object on their `prompt`
message, with `output`, `output_schema` (a JSON value), and `schema_retries`.
`done.reply` carries the final answer separately from streamed intermediate text.

The CLI and TUI ring the terminal bell when a turn finishes or stops, and when
interactive approval or input is required. `ROOK_NOTIFY=off` disables the bell.
Only a terminal on stderr receives it: redirected streams and JSON stdout stay
free of notification escapes. Whether the bell is audible or visible depends
on the terminal settings.
