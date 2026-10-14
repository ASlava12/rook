# Code Mode feasibility probes

The original runtime and answer probes are research, **not** an exposed Rook tool. They establish whether
a small QuickJS runtime can support injected calls with enforceable resource
limits, and compare script/ordinary calling on fixed read-only fixtures. They
do not certify Rook approvals, checkpoints, execution journals, recovery or
frontend delivery. Row 3 in the adoption tracker records the unadopted experiment.

The optional engine prototype was removed after its native comparison failed
to establish an adoption benefit. Sources and a selective restoration patch
remain in the external archive recorded in the
[research decision](../../../docs/research/code-mode-20261011.md).
`native-bench.py` requires a build of that prototype; the current production
engine does not expose Code Mode. The harness measures
genuine Rook turns with ordinary tools available in both arms and Code Mode
additionally advertised in the candidate. It keeps separate stores, fixed
inputs, answer artifacts and parent/child transcripts. Native completion and
independently correct answers are distinct; retained failures are never retried.

```sh
python3 xtask/probes/code-mode/native-bench.py \
  --binary /absolute/path/rook \
  --runtime-dir /absolute/path/rook-code-mode-runtime/node_modules/quickjs-wasi \
  --output /absolute/path/new-native-comparison
python3 xtask/probes/code-mode/analyse-native.py /absolute/path/new-native-comparison
```

Run the derived analysis after the native comparison ends. It counts actual
tool calls from program child transcripts, so calculator-only scripts do not
establish batching benefits. Artifacts and summaries alone do not certify all
physical token usage or monetary savings. The later harness retains a private
copy of the supplied executable before any arm; the original frozen harness and
measurement were not modified or repeated. No Code Mode optimization is adopted.

The implementation is independent. Pi supplied the idea; the runtime dependency
is MIT-licensed `quickjs-wasi@3.6.2`, the exact version pinned by the inspected
Pi reference. No reference subsystem was copied.

Install the dependency outside the repository; no lifecycle scripts are needed:

```sh
npm install --ignore-scripts --no-audit --no-fund \
  --prefix /tmp/rook-code-mode-runtime quickjs-wasi@3.6.2
ROOK_QUICKJS_DIRECTORY=/tmp/rook-code-mode-runtime/node_modules/quickjs-wasi \
  node --test xtask/probes/code-mode/runtime.test.mjs
```

The host imports only the trusted runtime and its bundled WASM. Each script has
a fresh VM with no Node APIs, filesystem/network extensions or module loader.
Host callbacks are closed over by a VM-created API. Primitive guest strings are
length-checked before copying, then checked for UTF-8 byte size. CPU interruption
uses the actual deadline and a shared cancellation flag, so a guest cannot block
the thread that requests cancellation. A limit violation remains fatal even if
the guest catches its exception. Unsettleable promises fail immediately; pending
host tools receive cancellation when the execution ends. Arbitrary guest return
values are never dumped into the host: `text()`, `console.log()` and returned
values are serialized inside the VM and pass through the same bounded sink.

Default ceilings: 32 KiB source, 16 MiB QuickJS allocation budget, 256 KiB native
stack, 64 calls, 8 pending calls, 16,384 UTF-16 units/64 KiB per argument or output
item, 64 KiB aggregate UTF-8 output, 64 output items and 2 seconds. Host result
strings are limited to 64 KiB before entering the VM. The QuickJS allocation
budget does **not** certify a 16 MiB bound on total Node process RSS or WASM
linear memory. The eventual process bridge must also bound its own queues,
frames, descendant lifetime and host resources. Cooperative host cancellation
does not prove an external effect did not happen; Rook journals must retain that
uncertainty.

Run the fixed eight-pair local-model preflight:

```sh
ROOK_QUICKJS_DIRECTORY=/tmp/rook-code-mode-runtime/node_modules/quickjs-wasi \
  python3 xtask/probes/code-mode/answer-bench.py \
  --endpoint http://192.168.1.46:1234/v1 \
  --model huihui-ornith-1.5-35b-a3b-abliterated-i1 \
  --output /tmp/rook-code-mode-answer-preflight
```

The deterministic fixture contains six JSON partitions and a config-selected
partition. Tasks cover dependent reads, exact identifiers, Russian strings,
aggregates and sorting. The direct arm permits batched native function calls;
it is not forced to make one request per tool. The Code Mode arm uses the same
read-only fixture API through QuickJS and returns selected observations. Arm
order alternates. The final JSON is scored against independently computed
answers, rather than the model's claims. Exact-format compliance (`passed`) and
the unambiguous target JSON value (`answer_correct`) are reported separately;
the latter does not certify any extra prose. Sources are frozen and hashed
before requests; VM invocations use those frozen sources. All physical request/response bodies,
usage, script results, failures and intermediate results are retained. There are
no transport/model retries. Task-level script corrections remain visible within
the fixed eight-step, sixteen-outer-call, 64,000-reported-token and 240-second
budgets; one HTTP request may take at most 90 seconds. Runtime budgets are not
raised to obtain a pass.

The harness is a provider/API and VM preflight, not a native Rook run: there is
no automatic checker, workspace mutation, operator configuration or installed
daemon. Token totals cover these physical requests only; unknown usage remains
unknown and no monetary cost is inferred for the local model. Positive results
justify integration work. They cannot establish the required improvement on
actual Rook tasks or authorize changing a default.
