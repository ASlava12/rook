# Native Google tool schemas

Run against one built or downloaded CLI/daemon pair:

```sh
python3 xtask/probes/google-tool-schemas/run.py --bin-dir target/debug --output /tmp/google-schemas.json
```

The fixture creates private homes and workspaces, a loopback Google/MCP server
and its own daemon. It records binary and script hashes and retains commands'
stdout/stderr and received declarations. It never uses a real provider key.
The report includes the actual commands, exact UTF-8 output and output hashes,
plus the owned daemon's exit status, for independent receipt validation.

Local CLI and daemon turns each discover one MCP tool, send its schema to the
Google endpoint, execute it once with the original arguments and send its result
back before accepting the final reply. Checks include the JSON Schema field,
scalar constants, unchanged literal annotations and definitions, absence of the
legacy field, authentication, workspace contents and cleanup of the owned daemon.

These are protocol and integration checks, not live Gemini acceptance, model
quality measurements or a replacement for downloaded-release verification.
