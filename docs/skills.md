# Skills

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

## What ships with Rook

Three skills come in the box, under `skills/` in the source tree:

| skill | what it is for |
|---|---|
| `in-place-edit` | Editing files from the shell across GNU, BSD and Windows userlands |
| `rust-release` | Cutting a release of a Rust workspace |
| `store-triage` | Working out why a store has grown, and reclaiming space safely |

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
roughly an order of magnitude slower to process than plain text. A card is tiny next to the body it
stands for — a test pins that at well under 100 tokens for a couple of them —
and the catalog as a whole is what `agent.max_skill_cards` bounds: fifty cards
cost roughly 770 tokens a request.

The catalog itself is bounded by `agent.max_skill_cards` (50), because it is paid
for on every request and a machine that has collected skills for a year would
otherwise pay for all of them. Skills past the cap are counted, not hidden:
`load_skill` answers a name it does not have with the ones that match it, so
describing what you need finds a skill the catalog did not name.

`rook skills ls` shows what loading each skill *would* cost:

```
   name          version  source   tokens  description
─────────────────────────────────────────────────────────────────
✓  in-place-edit  1.2.0   user      ~340   Edit files in place across platforms
·  deploy         2.0.0   project   ~1200  Deploy the service to staging
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
