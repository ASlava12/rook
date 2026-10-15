# Prompt cache experiment — 2026-10-15

Decision: do not add a caching parameter or change reasoning defaults. The
available Qwen endpoint already reuses its prefix. Explicit `cache_prompt=true`
provided no material improvement over Rook's current request, which omits it.
This is a conditional adoption item closed by a negative result, not an
unimplemented cache flag.

The deployed model was `qwen3.8-flash-next`, reported as owned by `llamacpp` on
`root@192.168.1.72`, port 8080, with four slots. The existing configured Rook
credential was supplied over SSH stdin. The experiment issued synthetic API
requests; it did not install files, alter configuration or restart services.
The server's exact build revision was not established.

llama.cpp documents prefix reuse with `cache_prompt` enabled by default and
supports its completion options on the chat-completions route. It also exposes
cached/processed token counts in timings and usage. These are the measured
fields here. [llama.cpp server API](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)

Three independent synthetic prefixes each contained 80 reference records.
Each pair shared exactly the same model, messages, `reasoning_effort=none`,
temperature zero and eight-token output ceiling. After a cold warmup, the pair
compared the current request (field absent) with explicit `cache_prompt=true`.
Order alternated A/B, B/A, A/B. All six scored answers were exactly `OK`.

| Median for warm requests | Current request | Explicit cache=true |
|---|---:|---:|
| First visible token, measured from the SSH host | 98.246 ms | 98.199 ms |
| Cached prompt tokens | 1407 | 1407 |
| Newly processed prompt tokens | 4 | 4 |
| Generated tokens | 2 | 2 |
| Correct responses | 3/3 | 3/3 |

The 0.047 ms first-token difference is about 0.05%, with identical processed
and cached token costs. This tiny synthetic sample cannot establish a useful
latency improvement. Cold warmups had no cache hits and took 3.817–3.883 seconds
to the first token, proving that the fixture actually reached the uncached path.
Money cost is unavailable for this self-hosted endpoint; no billing tariff was
invented. Token processing and generation are the available cost proxies.

A separate bounded effort probe held the final synthetic prefix fixed.
`low` was accepted, processed 1435 prompt tokens with no reuse, and emitted
reasoning without a final answer within its eight-token ceiling. That is a
truncation under the test budget, not proof of bad model quality. Returning to
`none` recovered 1407 cached tokens and a correct answer. One `high` trial
returned HTTP 500; the experiment did not retry it or infer a general support
claim from that one failure. The following `none` request succeeded again.
Changing effort therefore cannot be treated as a free cache optimization.

This result applies to the measured single llama.cpp model and these requests.
It does not establish results for another physical model or a paid OpenAI
Responses endpoint. `prompt_cache_key` is not documented by the measured
backend's API reference and was not sent as if OpenAI compatibility proved
support. No identity-key feature is adopted without a supported target and a
measured benefit. Server-native prefix reuse remains the current behavior.

The [JSONL observations](prompt-cache-20261015.jsonl) include cold requests,
paired warm requests, effort probes, correctness checks and the summary.
The [exact remote benchmark script](prompt-cache-20261015.py) expects a bounded
JSON credential on stdin and outputs metadata only. The credential is never a
command-line argument, fixture, report field or repository file. The source
uses only synthetic reference records, not a user's conversation.
