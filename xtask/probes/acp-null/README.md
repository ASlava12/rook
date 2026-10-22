# Owned ACP null/default payload probe

Requires Python 3.11 or newer and an actual CLI. Run without Python optimization
and choose a fresh output path in an existing directory:

```sh
python3 xtask/probes/acp-null/run.py --bin-dir /path/to/package/bin \
  --output /path/to/acp-null-report.json
```

Eighteen isolated CLI/editor exchanges use local scripted model replies. Each
performs exactly two main requests, one verifier reporting `unproven`, and one
completion classifier; these replies do not certify task quality. The editor
uses its own buffers and fake terminal responses. No terminal command or real
model runs, disk fixture contents remain unchanged, and the installed daemon is
not involved. Only owned child processes/services and temporary state are cleaned.

Require exit zero and all eighteen flags. Controls distinguish object/null write
acknowledgements from missing results, explicit errors and invalid scalar
acknowledgements. Null read results still fail without required text. Terminal
wait/release null acknowledgements must produce one create/wait/output/release
sequence with no duplicate cleanup. A valid flattened wait exit must be used
when output has no nested exit status. Signal-only and unsigned 32-bit failure
statuses remain known failures. Missing creation identity, output, truncation
flag or valid exit evidence remain uncertain; cleanup is attempted where the
terminal identity is known. All connections exercise object, null,
omitted and malformed (boolean/array/string) session-list parameters. Malformed
release acknowledgements must retain the existing cleanup guard without losing
an already confirmed command result. After connection close, missing,
error and malformed write acknowledgements and uncertain terminal responses must
retain their potentially effective operation in CLI recovery. An actual fresh CLI process then tries
to resume each affected session. Its model requests a write, which must be
refused before changing the file; the same unknown operation must remain for
explicit review. Model-assisted inspection remains allowed. Ordinary successful
operations and refused reads leave no unknown write.
The report binds the checks,
method/status observations, 72 initial provider requests and at most four
additional requests for each of the ten recovery attempts to the CLI SHA256.

The published Windows 0.21.3 baseline passes the object, missing/error and read
controls but fails null write, scalar acknowledgement refusal, null/omitted
list requests, single terminal cleanup and durable uncertain-effect recovery.
It also loses terminal uncertainty and ignores a valid flattened wait exit.
The baseline's acknowledged terminal
release is followed by another kill/release. Corrected-source native CI and
downloaded-package evidence are separate requirements.

The protocol source is the pinned ACP schema's explicit default-on-null
[payload inventory](https://github.com/agentclientprotocol/agent-client-protocol/blob/2c8270462725d07d9d7cd845beb21dc99c9eae68/agent-client-protocol-schema/src/serde_util.rs).
The same snapshot's [client response types](https://github.com/agentclientprotocol/agent-client-protocol/blob/2c8270462725d07d9d7cd845beb21dc99c9eae68/agent-client-protocol-schema/src/v1/client.rs)
define required output/truncation fields, flattened wait status and optional
unsigned exit code/signal. No default exit code is assumed.
Rook preserves raw JSON-RPC result presence and validates at the consuming
method; it does not recursively replace nulls or relax required payload fields.
No upstream implementation is copied and reference pointers remain unchanged.
