# Semantic memory retrieval measurement — 2026-10-11

A fixed synthetic corpus compares Rook's actual lexical `memory::search` with
local BGE-M3 and Nomic embedding endpoints on 192.168.1.46. No personal memory was
read. This is retrieval evidence, not evidence of improved agent task completion.

```sh
python3 xtask/probes/semantic-memory/bench.py --output /tmp/rook-semantic-memory
```

The corpus has 25 documents and 30 queries: 12 Russian cross-language queries,
12 English paraphrases, four exact identifiers and two irrelevant queries.
Input SHA-256: `f9c034582736f8f02b5faff938a1a766f127560a9be652246e3ed776f2c6534d`.
Hybrid weights were fixed before measurement: 0.8 cosine, 0.2 normalized lexical
overlap, a literal-query boost and a fixed 0.5 cosine floor. Nomic uses the task
prefixes in its [model card](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5#usage).
BGE-M3 uses unprefixed texts following its
[model card](https://huggingface.co/BAAI/bge-m3).

| Arm | Correct first result / 28 | Correct within first three / 28 | Exact IDs / 4 | False hits on irrelevant queries / 2 |
|---|---:|---:|---:|---:|
| Production lexical | 9 | 9 | 4 | 0 |
| BGE-M3 hybrid | 14 | 23 | 4 | 0 |
| BGE-M3 dense only | 21 | 23 | 3 | 0 |
| Nomic hybrid | 9 | 13 | 4 | 1 |
| Nomic dense only | 13 | 15 | 4 | 1 |

BGE hybrid improved five first-result cases without losing any successful lexical
case. Nomic hybrid gained one and lost one. Dense BGE's stronger first-result
ranking does not justify discarding lexical retrieval: one exact identifier
regressed. The current hybrid may be overly influenced by lexical overlap on
paraphrases; changing weights on this corpus would be tuning on the evaluation
set, so further candidates need a separate fixed holdout corpus.

| Cost | Production lexical | BGE-M3 | Nomic |
|---|---:|---:|---:|
| Median query time | 0.213 ms | 28.9 ms | 22.4 ms |
| Maximum embedding query time | — | 51.5 ms | 35.2 ms |
| Initial document embedding | — | 3.29 s | 2.51 s |
| f32 document vector bytes | — | 102400 | 76800 |

Embedding query latency uses individual network requests, not batches. Indexing
is batched. The measured cache size is vectors alone; serialized metadata and
protocol overhead would add cost. Local keyword scoring excludes CLI process
startup; embedding timing includes transport and response decoding. Model load
cost is not a steady-state query cost and must be handled separately in product
integration.

The full report at
`/Users/vart/.cache/rook-semantic-memory-eval/20261011/report.json` retains each
ranking, score and query time, as well as errors if an arm fails. The experiment
has not changed memory retrieval defaults. Next: bounded optional integration,
keyword fallback and cache invalidation checks, then a paired answer/task quality
benchmark against the user's local LLM.

## Paired LLM source-grounding measurement

`answer_bench.py` supplied each arm's first three precomputed results to
`huihui-ornith-1.5-35b-a3b-abliterated-i1` at the same LAN endpoint. The corpus and
retrieval report fingerprints were checked before requests. Both arms used the
same prompt, temperature 0 and 512-token output limit; arm order alternated per
question. Sixty physical requests ran sequentially without retries. No operator
memory, daemon configuration or real workspace was read or modified.

The probe exited 0 and retains every answer, supplied fact id, reported usage,
finish reason, parse failure and duration at
`/Users/vart/.cache/rook-semantic-memory-answer-eval/20261011/report.json`.

| Group | Questions | Lexical correct source | BGE-M3 hybrid correct source |
|---|---:|---:|---:|
| Russian cross-language | 12 | 0 | 11 |
| English paraphrase | 12 | 4 | 7 |
| Exact identifier | 4 | 4 | 4 |
| Irrelevant question: source null | 2 | 2 | 2 |
| Total | 30 | 10 | 24 |

Hybrid gained fourteen correct source selections and lost none of the baseline's
successful selections. This measures source grounding, not complete prose
correctness. Manual reading found an inaccurate Russian rendering of "preimages"
in the shell-undo answer even though its source id was correct. One query spent
all 512 tokens on reasoning and returned no answer in both arms. Hybrid also
returned the string `"null"` instead of JSON null on a different unsupported
query; the parser rejected it. All failures remain in the totals and report.
The same token limit was not increased after inspecting the results.

Median physical request time was 3.374 seconds for lexical and 4.552 seconds for
hybrid, about 35% slower. Reported aggregate prompt tokens were 4,391 versus
4,895; completion tokens, including reasoning, were 4,089 versus 5,025.
The sixty requests took 260.14 seconds in total. These timings exclude retrieval
and reuse a precomputed index; add the retrieval/index costs above when assessing
the candidate. They are synthetic single-pass observations, not confidence
intervals or product latency guarantees.

This supports continuing bounded optional BGE integration. It does not establish
default adoption or complete item 7: actual Rook-agent task checks, cache/version
bounds, fallback, workspace isolation and separate holdout evaluation remain.
No memory retrieval defaults changed.


## Optional product integration and holdout tasks

The intermediate step had an opt-in implementation (now archived and removed) using
scoped memory facts, bounded local HTTP and a workspace-owned cache. Nine focused
core checks cover actual dispatch, cache versions/expiry, source attribution,
workspace isolation, response/vector/index bounds, fallback, rendered context
budgeting and pending-lookup admission. CLI/daemon search parity and browser
checks pass. The integration gate exited 0; the final small budget/admission
refinements require their final full regression gate too.

`agent_bench.py` then ran a distinct fixed seven-task holdout through the native
CLI on the same LAN model, with eight synthetic facts and fresh independent
stores for both arms. The agent must create an exact grounded `answer.json`
through ordinary tools. It can reformulate its own `recall` query, unlike the
precomputed-source probe. This already occurred in the first baseline turn.
Native timeouts and empty final answers are retained as task failures. Task
correctness, context usage and latency have no final aggregate until all pairs
finish and scoring is checked against saved native transcripts. Receipts live
under `/Users/vart/.cache/rook-semantic-memory-agent-eval/20261011-product`.

The initial runner parsed `tool_call` instead of the native transcript's
`tool-call` spelling. This is a scoring bug, not a reason to rerun the model:
`rescore_report` repairs the tool labels and verifies retained child-checker
calls using the same artifacts. Preserve the initial report alongside the
corrected one. At that intermediate stage no optimization default had changed
and item 7 was partial. The terminal decision below supersedes that status.


## Final native task comparison and decision

The seven fixed pairs completed without model retries. The scoring bug was
repaired from saved `tool-call` events and child-checker transcripts. The
original report is retained as `report-before-score-fix.json` next to the
corrected `report.json` in `20261011-product`.

| Native agent outcome | Lexical | BGE-M3 hybrid |
|---|---:|---:|
| Correct task, including allowed tool usage | 3/7 | 1/7 |
| Native timeout | 0 | 5 |
| Median seconds | 108.663 | 240.007 |

Hybrid wins: 0; losses: 2. Exact parent artifacts alone do not prove an allowed,
completed task: timeouts and automatic checker command usage remain failures.
Recorded completed-outcome token totals omit uncompleted attempts and are not
an apples-to-apples measure of total cost. Baseline empty turns and all hybrid
failures remain; no timeout/step/output budget was increased to obtain a pass.

The actual end-to-end measurement does not justify keeping the optimization.
The optional product prototype described above was removed from the working
engine, CLI/API/frontends and configuration. Its source and a restoration patch
are retained under `20261011-product/prototype/`; retrieval and answer-only
research fixtures remain for reproducibility. The installed daemon and default
memory lookup were not altered. This decision follows the user's requirement
to retain an optimization only after useful improvement is demonstrated.
