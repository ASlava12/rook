# Local semantic memory retrieval experiment

Run from the repository with the local embedding service available:

```sh
python3 xtask/probes/semantic-memory/bench.py --output /tmp/rook-semantic-memory
```

The fixed synthetic corpus covers Russian/English queries, paraphrases, exact
identifiers and irrelevant requests. The lexical arm calls the production Rust
`memory::search` through `memory-retrieval`; it is not a Python reimplementation.
The candidate combines normalized lexical overlap and dense cosine at fixed
weights (0.2/0.8), a fixed 0.5 cosine floor and a literal-query boost. Weights and
thresholds are fixed before inspecting results. No real user memories are read.

The JSON report retains every ranking and score, input fingerprint, top-1,
top-3, reciprocal rank, irrelevant false hits, indexing time, per-query latency
and cache size. Single-query requests measure the latency a turn would pay,
without hiding it in a query batch. The cache is measured as f32 vectors; no
index cache or embedding retrieval is added to the product by this probe.

Nomic's document/query prefixes follow its
[model card](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5#usage).
BGE-M3 uses unprefixed input as documented in its
[model card](https://huggingface.co/BAAI/bge-m3).

Retrieval gains alone do not prove improved LLM task answers. Follow a positive
result with bounded product integration and a paired end-to-end benchmark on
the user's local LLM before adopting an optimization default. Failed or timed-out
models retain their errors; do not replace them with another run's results.

## Paired answer grounding

After a retrieval measurement, pass its unchanged report to the answer probe:

```sh
python3 xtask/probes/semantic-memory/answer_bench.py \
  --retrieval /tmp/rook-semantic-memory/report.json \
  --model YOUR_LOCAL_CHAT_MODEL --output /tmp/rook-semantic-memory-answers
```

Both arms supply their first three facts to the same local LLM with identical
generation settings. The first arm alternates across questions; there are no
automatic retries or tool calls. Corpus/report identities are checked before
sending requests, and every pair retains its answers, source ids, finish reason,
reported usage, duration and failures in an atomically replaced report.
Input/response/report bytes and corpus counts are bounded. Only the synthetic
fixture is sent; no operator memory, configuration or credentials are loaded.

The automatic score checks the exact supporting fact id, including null for
irrelevant questions. It is a source-grounding score, not a prose-quality grade.
Inspect the retained answer text for correctness. This probe uses precomputed
rankings and bypasses Rook's agent; it cannot close product integration, cache
invalidation/fallback checks or actual agent task completion evidence.

## Actual agent tasks

After building the current native CLI, run the fixed holdout fixture through
ordinary Rook turns:

```sh
python3 -B xtask/probes/semantic-memory/agent_bench.py \
  --binary /path/to/current/rook --output /tmp/rook-memory-agent-measurement
```

The destination must be new. Every arm has a separate scratch `ROOK_HOME` and
workspace, identical seeded facts and generation settings. Arm order alternates.
The agent must create `answer.json` with the exact supporting fact and its ID,
or nulls when no fact supports the question. It may use ordinary `recall`, write
and verify the requested file; shell execution, delegation, external lookup and
memory changes are disallowed by the task and rejected by the score.

The fixed fixture is separate from the original retrieval corpus. Retain all
native exits, model outcomes, file contents, source IDs, durable transcripts,
wall time and token usage. No automatic retries or hidden replacement runs.
Scoring can be verified from existing artifacts with `rescore_report`; this
function never calls a model. The score establishes execution of these bounded
grounded-file tasks, not general prose quality or translation accuracy.
