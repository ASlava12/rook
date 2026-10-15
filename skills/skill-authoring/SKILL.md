---
name: skill-authoring
description: Use when creating, importing or revising a Rook skill and its resources.
version: 1.0.0
license: MIT
---

# Skill authoring

Define the requests this skill should handle, its observable output and the
non-obvious procedure it teaches. Check the installed catalog first. Extend an
existing skill when its trigger and workflow already match.

Choose the requested destination. Repository skills live in `skills/` or an
optional collection; project skills under `<workspace>/.rook/skills/`. Personal
skills use `write_skill`, which saves and versions the complete bundle. Use
actual Rook tools, not another agent's commands or home directory.

Repository frontmatter needs a usable lowercase hyphenated `name`, a precise
`description`, semantic `version` and real environment constraints in `requires`.
Use a name directory, with version subdirectories underneath when needed. Use
`variants` for platform-specific bodies. Rook gates OS, architecture, userland,
agent version, detected languages and detected tools. Python packages and enabled
MCP equipment need operation-specific checks, not invented tool-version keys.

For personal authoring, `write_skill` takes a Markdown `body` without frontmatter.
Requirements are a separate tool argument; a sentence about Python in the body
does not populate `requires.language.python`. A complete small example is:

```json
{
  "name": "csv-report",
  "description": "Use when totaling a CSV amount column with exact decimals.",
  "body": "Use Python 3.10+ and the bundled scripts/report.py. Pass the UTF-8 CSV path as its first argument; the amount column is summed with Decimal. Verify totals against the source. Test the helper in the workspace before saving; a protected Rook state directory is not a shell working directory.",
  "requires": {"language": {"python": ">=3.10"}},
  "files": {"scripts/report.py": "import csv, sys\nfrom decimal import Decimal\nwith open(sys.argv[1], encoding='utf-8', newline='') as source:\n    total = sum((Decimal(row['amount']) for row in csv.DictReader(source)), Decimal('0'))\nprint(f'{total:.2f}')\n"}
}
```

Adapt the helper and error handling to the actual task. Test authored helper
content in an owned workspace first, then save the exact tested content. A shell
cannot read protected `ROOK_HOME`; do not relax that boundary to run a personal
bundle. Keep the essential procedure usable in the body, and use project-local
resources when the agent needs file access. The runtime lists resource paths but
that does not grant filesystem access to them.

Before saving, check the requirement object and the short discovery description.
After saving, `load_skill {"name":"csv-report"}` verifies that it resolves and
loads. `rook skills show` prints the body, and `rook skills why` explains
requirement mismatches. The operator can inspect the saved `SKILL.md` for exact
metadata; do not assume that CLI can read state from inside a contained shell.
`find_skill` searches configured sources, not authored personal history.

Keep the body short. Repository builtins have 50 cards / 1500 estimated catalog
tokens, 50 tokens per card and 1200 per body. Conditional references are useful
only when the target runtime can actually read them. `allowed-tools` is
informational in Rook and grants no execution rights.

Preserve the user's task and existing authorization. A missing or declined
credential leaves a useful draft and limitation rather than repeated questions.
Verify a realistic request, script output and failure behavior; loading alone
does not prove usefulness. `rook skills capture NAME -m 'reason'`, history and
rollback preserve operator-authored revisions.
