# ADR-0013: A secret is named, never valued, in everything the model sees

## Status

Accepted.

## Context

The agent is asked to do work that needs a password: connect to a host, publish
a package, reach an API. Every ordinary way of doing that puts the value where
the whole design says it must not be. Written into a command, it is in the tool
call, in the session log, in the store, in what compaction summarises, in what a
sub-agent inherits, and in the next request to the provider — six places, one of
which is somebody else's machine. Pasted into the chat, it is in all of them
before the model has read it.

The store is the wrong place to fix that. It is content-addressed, searchable,
printable with `store cat`, walked by garbage collection and copied by a fork:
every property that makes it good makes it wrong for this.

## Decision

**A secret is named, never valued, in everything the model sees.** The model
asks for `ssh_prod`; the value is put in inside the tool, on the way out, into
the environment of the process that needs it, and taken back out of whatever
comes back.

**Values live in `~/.rook/secrets.toml`, 0600, in the clear** — the same
property as `~/.ssh/id_rsa` without a passphrase, `~/.aws/credentials`,
`.netrc`, and as `config.toml` already has, where an MCP server's API key goes.
A value that already lives somewhere else stays there: `env:NAME`,
`cmd:<command>` and `keychain:service/account` name where to get it, and then
this file holds nothing at all.

**The keychain is read, not written.** Writing one means passing the value as an
argument to `security` or `secret-tool`, where every process on the machine can
read it out of the process list for as long as the call takes. It is put there
by the person, with their own tool.

**No call returns a value.** Not `secrets ls`, not `GET /api/secrets`, not the
browser, not the TUI. There is no `show`. This is the property everything else
rests on, and it is worth more than the convenience of a `show`.

**Missing secrets may be requested privately.** `request_secret` accepts only
name/reason. A distinct metadata-only chat event opens a dedicated input field;
values use an owner/nonce-bound write-only HTTP route or a direct private sink
in standalone terminals. They never use ordinary question answers, composer
drafts, prompt history, or replay. Browsers discard private edits on disconnect.
The bounded receipt contains only request identity and status. Decline, skip,
expiry, cancellation and unavailable input suppress another request for that
name in the same turn or managed generation. Late/duplicate responses cannot
admit a save, and a newer credential is never silently overwritten. An admitted
save may finish after its observer disconnects; an uncertain outcome is not
replayed. The turn imports a saved credential and registers redaction before
the next command. ACP has no private-input mapping and does not advertise the
tool. No input from an absent person is interpreted as consent.

**Redaction happens at the one place a tool's answer becomes context.** Every
value the vault has handed out this turn is taken back out of what a command
printed, a page said, a file held or an MCP server answered — one rule, one
place, rather than one per tool.

Known values of at least six bytes are also masked in the exact single-level
JSON and Rust debug string representations used by tool arguments and quoted
diagnostics. Longer matching values are processed before their registered
prefixes. This prevents ordinary serialization from bypassing known-value
masking or leaving a longer credential's suffix visible. The bounded recording
path applies the same patterns and refuses an output whose replacements exceed
its limit. Escaped pattern bodies are built within the input text's byte length;
a longer pattern cannot match it. Review-source masking keeps its existing raw
value and line-preservation rules.

The actual AgentLoop regression uses an artificial quoted/backslash credential
and inspects the saved tool arguments, result and next provider request. Unit
controls cover overlapping registrations, control characters, Unicode and byte
limits. This extends exact known-value matching; it does not recognize unknown
credentials, arbitrary encodings or repeatedly nested string serialization.

The live continuation uses a masked message copy too. The main request boundary
also scrubs known values from message text such as the workspace sketch, including
values resolved during the current turn. Tool execution keeps the original
arguments so a masked path does not change which file is read. Signed reasoning
is withheld when its visible message is changed, using the same rule as saved
provider state. A separate execution control verifies the successful file read
and masked context in both the first and subsequent main requests.

The transport visits credentials it already holds when a new agent vault is
created. Retry, concurrency, capability, source-selection and fallback wrappers
forward that registration, including every fallback leaf. Credential helpers
are not resolved a second time. This closes a gap where provider construction
registered a key in a temporary vault that the actual turn did not retain.
The default visitor keeps custom provider implementations source-compatible;
custom hosts can also use explicit connection-secret registration.

The owned native fixture runs direct CLI and private daemon turns in both the
OpenAI-compatible and Anthropic transports. It checks 43 conditions, including
unchanged real authorization headers and masked saved/live context. All four
transport families and their wrappers also have a credential-forwarding control.

**A value is resolved when it is used and held for the turn.** The vault is
built per turn and dropped with it, so a value fetched for one turn is not in
memory for the next. A `cmd:` secret nobody asked for is never resolved at all.

## What this does not do

It does not stop a model from printing a secret on purpose. A command that can
use one can echo one, in an encoding the redaction does not catch — base64, a
character at a time, split across two lines. Nothing here claims otherwise. What
stops that is the approval policy and the sandbox: an approval names the secret
a call would spend, so a person granting it sees what is being spent.

What this stops is the way secrets actually end up in transcripts: written into
a command, printed by a script that dumps its environment, `cat`ed out of a
config file, echoed by a debug flag. The accident, not the attack.

## Consequences

`ssh` reads a password from a terminal and from nowhere else — not from an
argument, not from the environment — so the environment alone would have been a
mechanism that did not work for the case that motivated it. A command that names
exactly one secret is given `SSH_ASKPASS`, `GIT_ASKPASS` and `SUDO_ASKPASS`
pointing at a helper script that prints the variable it inherits: the value is
in no argument, the script holds nothing, and it is removed with the command.

A value typed into the browser crosses a loopback HTTP connection to the daemon.
It never leaves the machine, and it is not the same as typing it into a
terminal; saying so is part of offering the tab.

An editor that runs commands in its own terminal cannot be given a secret: its
environment is not this process's. Such a call is refused with the reason rather
than run without the value.
