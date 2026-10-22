# Version probe output and lifetime

A version command's process can exit while a descendant retains its stdout or
stderr handle. Waiting for output after observing that exit previously removed
the five-second patience: an owned twelve-second pipe holder delayed the
published 0.21.8 doctor for 12.13 seconds. A noisy command could also fill a pipe
before exiting because the caller did not drain it until after exit.

`rook-contain::captured_within` polls ready pipe data and direct-child status
under the same post-spawn patience. Unix uses zero-timeout readiness polling;
Windows peeks at available bytes before reading. Neither path creates detached
reader threads. EOF remains part of the deadline even after the parent exits.
The limit is checked before bytes enter retained capture, across both stdout
and stderr together. Version discovery supplies 64 KiB per command.

A timeout, I/O failure or exceeded ceiling rejects the result, including partial
version text. Other successful inventory entries retain their versions. Failed
captures request owned-group/direct-child termination and poll direct-child
reaping for at most one additional second. Process creation and platform signal
calls remain synchronous. Darwin's existing group cleanup may use an additional
five-second grace; this is not a deadline for the whole application startup.
Windows job attachment occurs after spawn, and detached descendants retain the
existing group-ownership limits. This does not establish an OS security boundary
or certify that every possible escaping descendant was terminated.

The change extends the existing Hermes-inspired short-command deadline idea in
response to the observed Rook pipe failure. Pinned Hermes doctor subprocess
helpers supplied the deadline reference; this ready-pipe reader is an independent
Rook implementation, not a port of an upstream capture API. No upstream code is
copied, no new crate dependency or MSRV is required, and no configuration or
persistent cache is added.

Native subprocess tests exercise both streams, closed stdin, output larger than
pipe capacity, a combined ceiling exceeded by individually smaller streams and
a parent exiting before its pipe-holding descendant. The orphan is deliberate
in that last fixture and belongs to the outer capture's process group. Owned CLI
diagnostics use the [tracked CLI fixture](../xtask/probes/version-capture/README.md)
and retain binary/workspace hashes. These
are process-contract checks, not model-quality or production-throughput results.
