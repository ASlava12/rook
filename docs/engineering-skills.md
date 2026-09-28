# Engineering skills

[English](engineering-skills.md) · [Русский](ru/engineering-skills.md)

The engineering skills live alongside the operational skills under `skills/` and
ship through the existing distribution packaging. Each directory contains a
self-contained `SKILL.md` with a short trigger and a procedure that ends in
observable evidence. Instructions use English to match the existing skill library;
the agent can still communicate in the user's language.

## Selecting skills

For a code change, the baseline is `problem-analysis` →
`repository-investigation` → implementation with `minimal-change` → `verification`.
Scale this to the task: a small correction needs a few facts and a direct check,
not a formal report at every stage. Reuse evidence already collected.

Load a specialist only when its trigger matches. For example:

- A recurring defect: `root-cause-analysis`, with `debugging-and-profiling` for
  diagnostic evidence and `testing` for meaningful regression coverage.
- A new API: `requirements-engineering`, `api-design`, then relevant implementation
  and compatibility checks.
- A schema transition under traffic: `database-engineering` and
  `migration-engineering`, with concurrency or distributed-system analysis only
  where the transition requires it.
- A user flow: `ui-ux-design`; use `accessibility` for a focused access audit or fix.
- A production rollout: `release-engineering` and `production-readiness`, using
  the project's platform-specific procedure such as `rust-release` where applicable.

This is a recommended workflow, not an enforced runtime sequence. Rook advertises
cards and the model decides when to call `load_skill`; adding files does not make
four skills mandatory system instructions. Enforcing such a sequence would require
a separate agent-policy change. A skill does not grant deployment, publication,
history-rewrite or communication permissions.

Repository Exploration and Repository Investigation are one skill;
Implementation Verification and Verification are one skill. Requirements describe
the requested behavior; problem analysis connects that behavior to the current
system. Testing creates useful checks; verification evaluates the completed task.
Refactoring changes structure; cleanup removes unnecessary remnants of the current
implementation. These distinctions avoid loading equivalent procedures twice.

## Catalog

### Analysis and control of the change

| Skill | Use it for |
|---|---|
| [problem-analysis](../skills/problem-analysis/SKILL.md) | Goal, constraints, current behavior, unknowns, risks and checks |
| [repository-investigation](../skills/repository-investigation/SKILL.md) | Entry points, related implementations, tests, conventions and dependency paths |
| [requirements-engineering](../skills/requirements-engineering/SKILL.md) | Behavioral requirements, non-goals and acceptance criteria |
| [solution-design](../skills/solution-design/SKILL.md) | Viable alternatives, tradeoffs, chosen approach and verification plan |
| [planning-and-decomposition](../skills/planning-and-decomposition/SKILL.md) | Ordered, independently verifiable implementation slices |
| [minimal-change](../skills/minimal-change/SKILL.md) | Scope control and justification of necessary wider changes |
| [root-cause-analysis](../skills/root-cause-analysis/SKILL.md) | Falsifiable hypotheses, distinguishing experiments and causal fixes |
| [verification](../skills/verification/SKILL.md) | Acceptance evidence, checks, final diff and honest completion status |
| [production-readiness](../skills/production-readiness/SKILL.md) | Release blockers, operational evidence, rollout and recovery |

### Implementation and review

| Skill | Use it for |
|---|---|
| [software-development](../skills/software-development/SKILL.md) | Idiomatic implementation, ownership, errors and bounded resources |
| [ui-ux-design](../skills/ui-ux-design/SKILL.md) | User flows, states, design systems and responsive interfaces |
| [software-architecture](../skills/software-architecture/SKILL.md) | Module/service boundaries, domain rules and dependency direction |
| [refactoring](../skills/refactoring/SKILL.md) | Incremental structural change with behavioral invariants |
| [debugging-and-profiling](../skills/debugging-and-profiling/SKILL.md) | Reproductions, debugger state, traces and CPU/memory/I/O evidence |
| [testing](../skills/testing/SKILL.md) | Unit, integration, contract, end-to-end, property and fuzz checks |
| [security-engineering](../skills/security-engineering/SKILL.md) | Threat boundaries, abuse cases and evidence-backed remediation |
| [code-review](../skills/code-review/SKILL.md) | Prioritized, actionable findings in a specific diff |
| [cleanup-simplification](../skills/cleanup-simplification/SKILL.md) | Temporary code, abandoned helpers and unnecessary complexity |
| [legacy-code](../skills/legacy-code/SKILL.md) | Characterization, implicit contracts and gradual replacement |
| [compatibility](../skills/compatibility/SKILL.md) | Supported platforms, clients, runtimes and old/new interoperability |
| [accessibility](../skills/accessibility/SKILL.md) | Keyboard, focus, semantics, screen readers, contrast and reflow |

### Contracts, state and performance

| Skill | Use it for |
|---|---|
| [api-design](../skills/api-design/SKILL.md) | REST/RPC/GraphQL contracts, errors, idempotency and pagination |
| [database-engineering](../skills/database-engineering/SKILL.md) | Schemas, SQL plans, indexes, transactions and isolation |
| [distributed-systems](../skills/distributed-systems/SKILL.md) | Partial failures, replication, retries and delivery semantics |
| [concurrency](../skills/concurrency/SKILL.md) | Ownership, races, deadlocks, task lifecycle and memory ordering |
| [performance-engineering](../skills/performance-engineering/SKILL.md) | Representative baselines, bottlenecks, caching and measured improvements |
| [networking](../skills/networking/SKILL.md) | DNS, transport, TLS, HTTP, proxy and load-balancer diagnosis |
| [data-engineering](../skills/data-engineering/SKILL.md) | Batch/stream pipelines, schema evolution, replay and reconciliation |
| [migration-engineering](../skills/migration-engineering/SKILL.md) | Expand/migrate/contract, resumable backfills and recovery |

### Operations and delivery

| Skill | Use it for |
|---|---|
| [observability](../skills/observability/SKILL.md) | Logs, metrics, traces, SLIs/SLOs and actionable alerts |
| [devops-infrastructure](../skills/devops-infrastructure/SKILL.md) | Containers, orchestration, host configuration and IaC |
| [ci-cd](../skills/ci-cd/SKILL.md) | Pipeline gates, reproducible inputs, artifacts and trust boundaries |
| [release-engineering](../skills/release-engineering/SKILL.md) | Versions, release notes, artifact validation and staged rollout |
| [dependency-management](../skills/dependency-management/SKILL.md) | Versions, lockfiles, advisories, provenance, SBOM and license questions |
| [reliability-engineering](../skills/reliability-engineering/SKILL.md) | Failure modes, bounded load, degradation, capacity and recovery |
| [incident-response](../skills/incident-response/SKILL.md) | Triage, stabilization, recovery evidence, timeline and follow-up |
| [documentation](../skills/documentation/SKILL.md) | READMEs, references, ADRs, architecture notes and runbooks |
| [git-scm](../skills/git-scm/SKILL.md) | Commits, branches, conflict resolution, bisect and requested rewrites |
| [build-systems](../skills/build-systems/SKILL.md) | Build graphs, toolchains, generated outputs and reproducibility |
| [cost-engineering](../skills/cost-engineering/SKILL.md) | Measured cost drivers, unit economics and verified savings |
| [technical-research](../skills/technical-research/SKILL.md) | Primary sources, versioned evidence, comparisons and bounded proofs of concept |

## Loading and validation

In a development checkout, use the existing builtin override:

```sh
ROOK_BUILTIN_SKILLS="$PWD/skills" cargo run -p rook-cli -- skills ls
ROOK_BUILTIN_SKILLS="$PWD/skills" cargo run -p rook-cli -- skills show problem-analysis
cargo test -p rook-skills --test builtin
```

Release packaging already includes `skills/`; no new registration mechanism is
needed. As with any builtin, a user/project skill with the same name can override
it. Extra installed skills may exceed `agent.max_skill_cards`; see
[progressive disclosure](skills.md#progressive-disclosure) for catalog limits.

The new skills use the portable required fields `name` and `description`. They
do not require particular external tools because their procedures can use the
facilities available in the target project. Concrete tool/version requirements
belong in a specialized skill's `requires` when that tool is genuinely mandatory.

The builtin tests check discovery, applicability, trigger wording and body/card
budgets. Those are structural checks; they do not establish how reliably a model
will choose or follow a skill in real tasks.
