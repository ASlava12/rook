# Owned daemon background-install probe

Requires Python 3.11 or newer, Node.js with built-in `WebSocket`, and an extracted
daemon. Run without Python optimization. Choose a fresh report path with an
existing parent directory.

```sh
python3 xtask/probes/lsp-install/run.py --bin-dir /path/to/package/bin \
  --output /path/to/lsp-install-report.json
```

The probe starts one owned daemon, local model/release HTTP services and two
ordinary chat sockets in different workspaces with one temporary cache. The
first physical asset download is held while both model turns and their
completion checks finish. Each session must retain its own install intent.
After release, both chats and background receipts must finish against a single
metadata lookup and download, with checked payload/tag and no unknown effects.
The report binds ten flags and physical request counts to the daemon's SHA256.
Require exit zero and every flag true.

The daemon uses autonomous cache fetch, a local release API and an empty PATH.
Any fixture approval is restricted to the known LSP network request. A disabled
placeholder server prevents starting the downloaded fixture. No system package
manager or real language server runs, and installed daemon/state are untouched.
Only owned processes and temporary files are cleaned up.

The published 0.21.1 baseline issues two metadata requests and downloads. In the
controlled Windows case, one caller fails publishing the shared destination
with `os error 32`; the other succeeds. A locally built corrected daemon passes
all ten flags and retires both receipts. These are process-local protocol and
ownership checks; separate CLI/daemon processes and real indexing are outside
this probe. Archive provenance and versions are verified separately.
