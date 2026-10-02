# Real-model phase comparison

`bench.mjs` compares a fixed configured model with the same analysis source
followed by a separately chosen implementation source. It uses the actual local
CLI, fresh workspace/store pairs and real inference. The controlled HTTP server
in `bench.test.mjs` tests failure handling only; its usage is not benchmark data.

Build first, then check the oracle/process tests:

```powershell
cargo build -p rook-cli
node --test xtask/probes/phase-routing/bench.test.mjs
```

Choose two working sources from the user's configuration:

```powershell
node xtask/probes/phase-routing/bench.mjs --source analysis-source --implementation-source implementation-source --repeats 2
```

Alternatively, ask for another physical model on the analysis endpoint:

```powershell
node xtask/probes/phase-routing/bench.mjs --source analysis-source --implementation-model physical-model-id --repeats 2
```

For a first pair use `--tasks rename --repeats 1`. Other tasks are `ports` and
`money`; `--tasks` accepts their comma-separated names. Repetitions are 1..5;
the common context window defaults to 32768, configurable with `--window`
(16384..262144). `--output-dir` must name a new directory; existing evidence is
never replaced. Otherwise roots are fresh `target/phase-bench-UUID` directories
and `target/pi-phase-bench-root.txt` points to the latest real run.

## Protocol and quality

Each case has two turns. The first inspects the task and writes DESIGN.txt
without modifying the seeded code. That first successful write exercises Rook's
existing transition. The second resumes the same session and implements the
same task. Both arms have identical prompts, seed files, effort (`none`), context
window, output limit and step limit. This measures the explicit staged workflow,
not every possible one-prompt workflow or overall model capability. Arm order
alternates on successive repetitions. No mock usage enters a real comparison.

The independent oracle stays outside the model's workspace. It imports the
actual modules and checks public behavior, edge cases, preserved neighbouring
exports and exact migrated records. It neither reads a model's success claim
nor relies on an editable workspace test. The seeded defect must fail before
inference. Candidate files are admitted under 64 KiB before evaluation; the
scorer subprocess has a deadline and output limit. The protocol separately
records whether planning left the original code intact and whether the final
implementation receipt dispatched the requested target.

## Evidence and interpretation

Every stage records its actual exit status, stop reason, wall time, token
outcome and saved context. `report.json` is written before a stage starts and
after it ends; per-stage stdout/stderr files and the owned PID remain readable
while it runs. A final bounded journal retains main/auxiliary receipts.
`source_commit`, the binary version and a hash of the benchmark scripts identify
the runner. The report keeps original observations; it does not automatically
claim a percentage improvement.

A workspace quality pass and a successful agent completion are separate facts.
CLI exit 2 means incomplete execution even if the oracle passes. A fatal
provider reply can leave a saved session without final stdout JSON. The runner
recovers only an unambiguous session in that case's fresh store, inspects its
physical-attempt coverage, leaves missing turn totals null and stops the suite
before scheduling another case. Read that failure rather than repeating it.

Coverage snapshots are cumulative: use `final_coverage`, never add both stage
snapshots. Receipt and attempt estimates overlap and must not be added either.
Rates come only from the named configured sources. A new physical model cloned
onto the same endpoint does not inherit another model's prices. Missing rates,
missing outcomes, unconfirmed native usage and failed attempts remain unknown.
`receipt_tokens` is only the known reported subtotal; `journal_receipts_complete`
records whether the bounded journal includes all counted receipts. Native token
counts are not a verified USD bill or an energy measurement.

Compare paired tasks/repetitions with valid planning and verified implementation
dispatch. Retain failed/incomplete outcomes alongside quality and latency. A
failed model load is availability evidence, not a cheap or fast implementation.
Cold loading, cache state, endpoint queues and completion checks are included in
wall time. Small samples support only the stated workloads and environment.

## Bounds and configuration

Each turn has at most 8 steps, 1536 output tokens per generation and a 900-second
process deadline. Ordinary delegation is disabled with
`max_subagents_per_turn=0`; background jobs are disabled. Command execution and
capture have their own limits. Turn stdout and stderr have fixed 1/3 MiB buffers;
each incoming chunk is admitted before copying or saving. Overflow, deadline
and write failure stop only that owned child. Other CLI captures are capped at
4 MiB; journal reads request at most 512 rows and 4096 bytes per body.

The user's complete configuration is parsed in memory and is never printed or
copied into the report. The scratch config contains only the two chosen model
connections and benchmark settings; no MCP servers, hooks or fallback rotation
are imported. Keep its files private if those connections contain credentials.
Environment key references retain their normal meaning. A `secret:` reference
needs `ROOK_PHASE_BENCH_KEY` because the benchmark does not copy the user's secret
store. No installed daemon/store/configuration is changed and no artifact is
published. Real failures retain their workspace and store for diagnosis.
