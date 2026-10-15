# Optional workflow skills

This collection includes independent Rook adaptations of the workflows recorded
in [references/PORTED.md](../references/PORTED.md) and user-requested workflows.
They use Rook tools and the
existing skill installer. No foreign runtime, plugin or model download is
required to discover them. Install only the workflows wanted for the account.

| Skill | Purpose | Operation dependencies |
|---|---|---|
| [workspace-cleanup](workspace-cleanup/SKILL.md) | Reclaim build/cache space with ownership and before/after evidence | Host disk/cache tools |
| [spreadsheets](spreadsheets/SKILL.md) | XLSX creation, edits and CSV imports | Python 3.10+, `openpyxl`; optional spreadsheet engine for recalculation |
| [docx](docx/SKILL.md) | Word documents and template-preserving edits | Python 3.10+, `python-docx`; optional layout engine |
| [presentations](presentations/SKILL.md) | Editable PowerPoint decks | Python 3.10+, `python-pptx`; optional layout engine |
| [pdf-workflow](pdf-workflow/SKILL.md) | PDF creation, extraction, merging and forms | Rook 0.13+, Python 3.10+, `pypdf`; `reportlab` for creation, Poppler for rendering |
| [diagram-maker](diagram-maker/SKILL.md) | Standalone SVG and editable diagrams | XML/JSON tools; a renderer for visual verification |
| [document-action-items](document-action-items/SKILL.md) | Cited obligations, deadlines and proposed tasks | A reader for the input format |
| [experiment-reporting](experiment-reporting/SKILL.md) | Matched measured comparisons and evidence-based verdicts | Measurement files; plotting tools only when a graph is needed |
| [provider-setup](provider-setup/SKILL.md) | Model API setup, private credentials and default selection | Intended provider access; optional credential source |
| [document-translation](document-translation/SKILL.md) | Long-document translation with glossary and coverage ledger | A reader for the input format |
| [reverse-engineering](reverse-engineering/SKILL.md) | Decompilation, class/type/name recovery and buildable source reconstruction with evidence | Format-appropriate decompiler/debugger; matching symbols and toolchain when available |
| [clean-room-reimplementation](clean-room-reimplementation/SKILL.md) | Behavioral specification, controlled handoff and independently implemented compatible software | Reference observations; separate implementer context with enforced access boundaries |
| [ui-implementation](ui-implementation/SKILL.md) | Working graphical screens, components, responsive layout and state handling | Project's UI stack; renderer/browser for interaction checks |
| [tui-development](tui-development/SKILL.md) | Terminal rendering, input, lifecycle, Unicode and PTY verification | Project's terminal toolkit; PTY-capable test environment |
| [game-cheat-development](game-cheat-development/SKILL.md) | Game trainers/cheats/overlays and evidence-based network protocol analysis | Controlled game/test-server environment; suitable debugger and packet tools |

Python package requirements are checked before the relevant operation. Rook's
`requires` gates Python/toolchain/agent versions; it does not detect arbitrary
Python modules or enable optional browser/MCP equipment. Loading a skill installs
no packages and requests no secrets by itself. Missing or declined dependencies
produce a useful draft and an explicit unavailable check.

Reverse engineering distinguishes recovered original names from inferred semantic
names and checks a reconstruction against the original behavior. It cannot promise
original source text that compilation erased. UI implementation complements the
builtin `ui-ux-design` workflow with concrete state, integration and renderer
checks. Game tooling includes memory features and network analysis with explicit
target/build checks and reproducible tests in the established test environment.

Clean-room reimplementation separates reference research from the implementer's
inputs. It freezes a specification/fixture manifest and records provenance and
observed compatibility. A new branch or prompt alone does not establish isolation;
when the runtime cannot provide it, the workflow delivers a handoff and reports
independent implementation as unverified.

## Installation

Add the collection to the top-level `skill_sources` array of trusted user
`config.toml`, preserving existing entries. With a local checkout:

```toml
skill_sources = ["/absolute/path/to/rook/optional-skills"]
```

Then use the ordinary CLI, from any workspace:

```sh
rook skills search spreadsheets
rook skills install spreadsheets
rook skills show spreadsheets
rook skills why spreadsheets
rook skills history spreadsheets
```

For the development workflows, search using separate words and install by the
exact name from the table:

```sh
rook skills search 'ui implementation'
rook skills install reverse-engineering
rook skills install clean-room-reimplementation
rook skills install ui-implementation
rook skills install tui-development
rook skills install game-cheat-development
```

After these files are published in the repository, a git source can offer them
too: `https://github.com/ASlava12/rook.git`. It is fetched only by an explicit
search/install/update. A source is inspected recursively by the existing bounded
catalog, so no new registry or source index is necessary. Before publication,
use the local checkout; a git source cannot see uncommitted files.

Installation copies the full directory and records its
source and content history. `rook skills update` preserves local edits. Uninstalled
packages do not enter the model's catalog. Installed cards share the existing
`agent.max_skill_cards` bound; an omitted card remains reachable by `load_skill`
and name search. There is no need to increase the default or automatically enable
every package.

The three broadly useful workflows `github-workflow`, `browser-qa` and
`skill-authoring` live in the built-in `skills/` collection. Development runs can
use `ROOK_BUILTIN_SKILLS=/absolute/path/to/rook/skills`; the normal release
packager includes that collection beside the binaries.

## Verification

The Python examples in the skill bodies use small illustrative inputs,
explicit outputs and read-back assertions. Run them in an owned temporary
workspace with the listed libraries. Use a separate rendering check for layout:
reopening OOXML/PDF/XML proves structure/content, not appearance. No fixture
numbers or example obligations are real measurements or extracted user data.

The new bodies are self-contained on purpose. A listed resource path does not
grant file access outside the workspace, and contained commands cannot read
protected `ROOK_HOME`. Essential instructions and executable recipes therefore
arrive through `load_skill` itself. New personal helpers should be tested in an
owned workspace before saving their exact content with `write_skill`.
