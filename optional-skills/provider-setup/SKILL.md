---
name: provider-setup
description: Use when connecting a model API, importing its credential or selecting a default model.
version: 1.0.0
license: MIT
---

# Provider setup

Read the active Rook config location and relevant overrides before editing.
Preserve existing models, comments and unrelated settings; keep a private backup
before a substantial change. Determine the protocol, endpoint including its path,
authentication scheme, advertised model IDs and requested context size.

Treat a supplied `.env` as data, not shell code. Parse only the requested
variables privately; do not source or print the file. Inspect endpoint scheme,
host and path without exposing embedded credentials. Send the credential only
to the configured intended service. Keep it out of command arguments, history,
model prompts and config exports.

Use `request_secret` for private input when the value was not supplied. Import
an authorized value into Rook's vault or refer to an existing environment or
keychain source. Put only `secret:NAME` or `env:NAME` in the model config. An
environment reference must be present in the daemon's actual launch environment;
a variable in a project `.env` is not automatically available there. See
the examples below.

Use a named `[endpoints]` entry for shared transport/authentication and separate
`[models]` entries for models. Distinguish the Rook alias from the wire model ID.
Do not infer support for a suffix or a context length from a display name. Check
the provider's metadata/documentation and a small request; retain a user-facing
alias when the API requires a different spelling. Do not silently select another
model after a rejected ID.

Validate offline first, then query only the intended new endpoint. Use
`rook models --source ALIAS --refresh`; unavailable old tunnels need not be
probed. With authorization to use the API, make a small bounded generation
request to verify more than catalog access. Set `agent.model` to the requested
alias and read the effective setting back. A short request does not test the
maximum context window.

Inspect daemon/activity status before applying a required restart. Honor a
request to defer interruption of active turns. If the user declines or ignores
a credential request, keep an actionable draft and name the unavailable check.
Report installed/configured/connected states and actual checks separately.

## Named endpoint example

This is a structural example. Replace protocol, URL, model ID and context with
values verified for the user's service; `example.invalid` is intentionally unusable.

```toml
[agent]
model = "fast-long-context"

[endpoints.team-api]
api = "anthropic"
url = "https://example.invalid/api/anthropic"
key = "secret:team-api"

[models.fast-long-context]
endpoint = "team-api"
model = "provider-model-id"
context_window = 1000000
```

Rook accepts `openai`, `responses`, `anthropic` and `google` protocols. A gateway
brand is not its protocol. `models.<alias>.model` is sent to the endpoint;
`agent.model` selects the alias. Set context explicitly only from a supported
service contract, not by copying the example's number.

```sh
rook --version
rook daemon status
rook secrets add team-api
rook secrets add team-api --source env:TEAM_API_KEY
rook config check --offline --json
rook models --source fast-long-context --refresh --json
rook config set agent.model fast-long-context
```

Choose either a kept secret or a reference, not both commands indiscriminately.
The interactive command uses hidden terminal input. To import an already supplied
value programmatically, use an available private-input interface; do not put the
value after a command-line flag or echo it into a shell pipeline. Inspect help
for the installed version before choosing an automation path.

These configuration commands require operator access to Rook state. A contained
shell cannot read protected `ROOK_HOME`. When the current interface cannot edit
configuration, prepare the exact config changes and verification commands for
the operator rather than disabling isolation or claiming they were applied.

When editing keys containing dots or brackets, preserve TOML quoting:
`[models."model-version[1m]"]` is one model entry. Do not split its name into
nested tables. Keep backups and secrets at private filesystem permissions.
