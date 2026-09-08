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

**Redaction happens at the one place a tool's answer becomes context.** Every
value the vault has handed out this turn is taken back out of what a command
printed, a page said, a file held or an MCP server answered — one rule, one
place, rather than one per tool.

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
