# Skills

[English](skills.md) · [Русский](ru/skills.md)

A skill is a directory containing a `SKILL.md`: YAML frontmatter plus a Markdown
body. This is the [Agent Skills](https://www.webfuse.com/agent-skills-cheat-sheet)
format, so a skill written for another agent loads here unchanged, and a skill
written here stays valid elsewhere — the additions below live in keys the spec
leaves free.

```
skills/
  in-place-edit/
    SKILL.md
    variants/
      bsd.md
      windows.md
    references/
    scripts/
```

## Minimum

```yaml
---
name: pdf-forms
description: Fill in and flatten PDF forms.
---

# PDF forms
...
```

`name` and `description` are the only required fields. The description is what the
agent matches on when deciding whether to load the skill, so it should describe the
*trigger*, not the implementation.

`license`, `keywords` and `allowed-tools` are accepted because the Agent Skills
spec has them, and nothing here reads the first two. `allowed-tools` is
informational and does **not** grant or restrict anything: what a tool call may
do is the approval policy's decision, made per call against the workspace and
the stance, and a file inside the material being read is the last place that
should be able to widen it. A skill that sets it gets a line in the log saying
so, which is the only reason to mention it here — an unexplained warning is
worse than the field.

## Versions

```yaml
version: 1.3.0
```

Several versions can sit side by side:

```
skills/deploy/1.0.0/SKILL.md
skills/deploy/2.0.0/SKILL.md
```

Resolution picks by **source precedence first, then version**. A skill in
`<workspace>/.rook/skills` beats one in `~/.rook/skills`, which beats a builtin —
regardless of version number, because a skill vendored into a repository is there
deliberately. Within a source, the newest *applicable* version wins.

## Bundled files

A skill can carry more than its `SKILL.md` — scripts to run, references to read.
`load_skill` names them and the directory they are in, because a body that says
`scripts/check.sh` is not something the agent can act on otherwise. A skill that
is only a `SKILL.md`, which is most of them, adds nothing to the reply.

## Writing a card that earns its place

The description is not a summary. It is read for one decision — load the body or
not — and it is paid for on every request, by every skill in the catalog, whether
or not anything reaches for it. So it names the *situation*, not the subject:

```yaml
description: Use when a Rook store has grown and the space is wanted back — and
  before `gc` or `prune`, the two commands here that can lose history.
```

not

```yaml
description: Work out why a Rook store has grown, and reclaim space safely.
```

The second reads better and decides nothing. A model holding it has to load the
body to find out whether this was the moment, and a body costs between two
hundred and a thousand tokens.

Three rules follow from that, and the shipped skills are held to all three by
`crates/rook-skills/tests/builtin.rs`:

- **`Use when …`**, or `Use before …` where the moment is a command about to be
  run. One card is capped at 50 tokens and the shipped set at 220 together.
- **A "not for …" clause only where the confusion is real.** `in-place-edit` says
  it is not for the editing tools, because reaching for `sed` when `edit_file`
  is right is the mistake it exists to catch. `rust-release` says nothing of the
  kind, because "a release is not an ordinary commit" is not a mistake anybody
  makes, and it would cost every request to say so.
- **What the skill needs goes in `requires`**, not in the sentence. The code
  checks it, `rook skills why` explains it, and a card that is not applicable is
  never shown — a precondition written in prose is one the model has to evaluate
  and can get wrong.

A body is capped at 1,200 tokens. Past that the long part belongs in a bundled
file the body names: `load_skill` reports those, and they are read only if they
turn out to be needed.

## What ships with Rook

Five skills come in the box, under `skills/` in the source tree:

| skill | reach for it when |
|---|---|
| `decision-matrix` | A fork has several defensible answers and the reasoning will be questioned later |
| `in-place-edit` | A file is about to be changed with `sed`, `awk` or a shell redirect, on more than one platform |
| `project-instructions` | A project's `AGENTS.md` is being written or trimmed |
| `rust-release` | A release of a Rust workspace is being cut |
| `store-triage` | A store has grown, and `gc` or `prune` is about to be run on somebody's history |

`cargo xtask dist` packages them next to the binary, which is the first place
[`builtin_skills_dir`](../crates/rook-core/src/paths.rs) looks. A plain
`cargo build` does not, so a development binary finds none — point
`ROOK_BUILTIN_SKILLS` at `skills/` to work with them as a user would:

```sh
ROOK_BUILTIN_SKILLS=$PWD/skills cargo run -p rook-cli -- skills ls
```

## `requires` — gating on the environment

This is the part that does not exist in the base format, and the reason it is here:
a skill that shells out to `sed -i` is correct on GNU userland and wrong on BSD; one
that uses a 2024-vintage API needs a toolchain new enough to have it. Without a
declaration, the agent discovers this by failing.

```yaml
requires:
  os: [linux, macos, freebsd]     # linux | macos | windows | freebsd | …
  arch: [x86_64, aarch64]
  userland: [gnu]                 # gnu | bsd | msvc
  agent: ">=0.1.0"
  language:
    rust: ">=1.85, <2.0"
    python: ">=3.11"
  tool:
    git: ">=2.30"
    docker: ">=27"
```

Every constraint is optional; an absent field means no constraint. Version strings
are [semver requirements](https://docs.rs/semver). A malformed one **fails at load
time** rather than silently never matching — that failure mode is nearly impossible
to debug from the outside.

The environment is detected once at startup: OS and arch from the build target,
userland inferred from the OS, and language and tool versions by running
`--version` and parsing the banner (`rustc 1.97.1 (…)`, `go version go1.22.5 …`,
`v20.11.1` all work). `rook doctor` prints what was found.

When nothing applies, the failure is specific:

```
$ rook skills why deploy
environment: macos / aarch64 / bsd userland

  ✗ deploy@2.0.0 [user]
      needs docker >=27, found 24.0.7
  ✓ deploy@1.0.0 [user] applies

chosen: deploy@1.0.0 [user]
```

## `variants` — one skill, several platforms

`requires` gates the whole skill. `variants` swaps only the body, so platform
differences do not fork a skill into `deploy-linux` and `deploy-windows` that then
drift apart.

```yaml
variants:
  - when: { userland: [bsd] }
    body: variants/bsd.md
  - when: { os: [windows] }
    body: variants/windows.md
```

`when` takes the same predicate as `requires`. The **most specific** match wins,
measured by how many constraints it names; if none match, the default body is used.
`rook skills show <name>` prints which variant was selected.

## Skills the agent writes

`write_skill` lets a turn record a procedure it had to work out, so the next
session starts from it instead of rediscovering it. It takes the finished body
rather than scaffolding one, writes into the user skills directory, and captures
the result as a version — rewriting a skill keeps the old one, reachable through
`rook skills history` and `rook skills rollback`.

It answers to the permission policy like any other write — a skill changes how
every later session behaves, which is worth one approval — so `readonly` refuses
it outright and `auto` lets it through.

Two things are checked before it counts as written. The name has to be a
directory name, and the skill has to parse: it is read back from disk, and a
`SKILL.md` that does not load is reported rather than left for the next session
to silently lack. Parsing, not resolving — a skill whose `requires` excludes the
machine that wrote it is doing its job.

The agent can only write over its own. A skill that ships with the project or the
system is refused by name, with the suggestion to pick another.

```sh
rook skills history cross-compile-freebsd   # every version the agent wrote
rook skills rollback cross-compile-freebsd <object>
```

## Progressive disclosure

Skills are not injected into the prompt. The agent gets a *catalog* — one card per
name, carrying the name, version and description — and calls `load_skill` to pull a
body in when it decides it needs one.

This matters more than it sounds. Full bodies for a large library cost thousands of
tokens on every request, and on local models a tool-and-skill-heavy prompt is
roughly an order of magnitude slower to process than plain text. A card is small
next to the body it stands for: the ones Rook ships average about thirty-six tokens
each against bodies of two hundred to nine hundred, and a test caps one card at
fifty and the shipped set together at 220. Fifty cards of that size are around two
thousand tokens on every request, which is what `agent.max_skill_cards` bounds
— and why a card that describes its subject instead of naming its moment is
worth rewriting rather than tolerating.

The catalog itself is bounded by `agent.max_skill_cards` (50), because it is paid
for on every request and a machine that has collected skills for a year would
otherwise pay for all of them. Skills past the cap are counted, not hidden:
`load_skill` answers a name it does not have with the ones that match it, so
describing what you need finds a skill the catalog did not name.

`rook skills ls` shows what loading each skill *would* cost:

```
   name          version  source   tokens  description
─────────────────────────────────────────────────────────────────
✓  in-place-edit  1.2.0   user      ~340   Use before changing a file with sed…
·  deploy         2.0.0   project   ~1200  Use when a change is ready for staging…
```

The `·` means blocked here; `--all` shows those and `why` explains them.

## Versioning your skills

A skill's `version:` field is what its author declares. Its *history* is what Rook
records:

```sh
rook skills capture my-skill -m "handle the BSD case"
rook skills history my-skill
rook skills diff <objA> <objB>
rook skills rollback my-skill <obj>
```

Each capture stores every file in the skill directory by content hash and records a
manifest under `skill/<name>/h/<millis>-<short>`. Unchanged files across captures
are stored once. History keys carry milliseconds, not seconds — two captures in the
same second are ordinary, and ordering them by a colliding timestamp would make
"the previous version" a coin flip.

`rollback` captures the current state first, so a rollback is itself undoable. It
restores files; it does not delete, so it reports anything on disk that the capture
did not contain rather than leaving a silent hybrid of two versions.

## Authoring

```sh
rook skills new my-skill -d "What this is for"
```

writes a `SKILL.md` scaffold with the optional blocks commented out and the
detected OS filled in. Then edit, and:

```sh
rook skills ls                 # confirm it loads and applies
rook skills why my-skill       # if it does not
rook skills capture my-skill -m "first version"
```

## Where a skill comes from

Two ways, besides writing one by hand.

**The agent writes it, with the tool it needs.** `write_skill` takes `files`
alongside the instructions, so when no tool does the job the agent can write one
and document it in the same breath — a `report.py` beside the `SKILL.md` that
says how to call it. A file whose contents begin with a shebang is made
runnable; a template is not. Names are relative and inside the skill, so a
`../` is refused rather than written. The loaded skill lists what it carries and
where, because instructions referring to a script the model cannot locate are
instructions it cannot follow.

**Somebody else wrote it.** `[skill_sources]` in `config.toml` lists places to
look — a git repository or a directory — and defaults to the Agent Skills
repository, which is where the format's own examples live. That is a starting
point rather than a blessing: installing from anywhere means reading what you
installed, and `skills install` says so and prints the path.

```sh
rook skills sources                 # where it looks
rook skills search pdf              # what those places offer
rook skills install pdf             # the whole directory, scripts included
rook skills update                  # bring the installed ones up to what the source offers
```

Nothing is fetched until one of those runs. Opening the store, starting a turn
and listing what is installed touch no network. The agent has the same two in
one tool, `find_skill`: searching reads, installing is approved like any other
write, because it puts instructions on the machine that later sessions follow.

A source needs no index and no API — its skills are read from the `SKILL.md`
files in it, which is the format everything here already speaks.

**Updating them without losing what you changed.** `skills update` refreshes
only what came from a source, and only where nothing here has touched it since.
Two kinds of leaving alone, and both are said out loud rather than skipped in
silence:

```
  pdf      — updated from https://github.com/anthropics/skills
  brand    — changed here since it came from … 3 weeks ago; left as it is.
             `rook skills history brand` shows what it came as.
  deploy   — yours, not from a source
  legacy   — … no longer offers it; left as it is
```

A skill written here never came from a source. A skill that came from one and
has been edited since is somebody's work, and an update that discarded it would
be destroying the reason they installed it — so the new version is held back and
named, and `skills history` and `skills rollback` are how to take it after
looking. Whether it was edited is decided by comparing what is on disk against
the snapshot taken when it was installed — a map of path to content hash, so no
clock has to be right and a copied directory is not mistaken for an edit. A
source that will not answer is reported as unreachable rather than read as the
skill having been withdrawn: a network hiccup must not remove anybody's tools.
Every update captures what it replaced first, so `rook skills rollback` puts the
previous body back.

### None of this loads until it is needed

Installing twenty skills does not put twenty skills in a request. What a request
carries is one line each — name and description, the card — and the body arrives
only when the model calls `load_skill`. The `pdf` skill from the repository above
is about 1,900 tokens of instructions across five files; its card is sixty. The
catalog itself is capped by `agent.max_skill_cards`, and what does not fit is
still reachable, because `load_skill` answers an unknown name with the skills
that match it.

## Interoperability

Rook implements the Agent Skills format, and Agent Plugins packaging around it.
A plugin is one directory holding both halves of what an agent needs:

```
~/.rook/plugins/rust-pack/        or  <workspace>/.rook/plugins/rust-pack/
  plugin.json                     name, description, version, mcpServers
  skills/tidy/SKILL.md            ordinary skills, in the ordinary format
  .mcp.json                       servers, if you prefer them beside the manifest
```

`.claude-plugin/plugin.json` is read too, since that is where the specification
puts it. Nothing in the layout is Rook's own, so a plugin written for another
agent works here unchanged and a skill authored today packages without being
rewritten — the same argument as [ADR-0003](adr/0003-agent-skills-format.md).

A plugin's servers are namespaced by its name, so two plugins shipping a `docs`
server do not collide in the tool names the model sees, and each runs in its own
directory. A skill from a plugin ranks above a built-in one and below the user's
and the project's: something vendored into a workspace is there on purpose.

Only a plugin under `~/.rook/plugins` brings its servers. One vendored into a
workspace brings its skills and not its `mcpServers`: a skill is text the model
may or may not load, and a server is a command spawned at session start — before
a prompt, before an approval, before anyone has typed anything. Cloning a
repository would otherwise run whatever it chose to declare. The ones you want go
under `[[mcp]]` in your own `config.toml`, and each skipped one is named on
start.
