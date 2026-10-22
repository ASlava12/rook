# Language-server discovery owns its short probes

Finding a language-server binary on PATH does not prove that it runs: rustup can
provide a rust-analyzer shim for an absent component. Rook asks such programs
for `--version`. A zero exit is usable; a nonzero exit is refused. A program
still running after ten seconds may have started serving instead, and remains
eligible. Installed payloads continue to bypass this PATH-only check.

Previously the check killed only a timed-out direct child and then waited
without a bound. A normal exit never cleaned up descendants. A controlled
published 0.21.9 Windows CLI finished `lsp servers` in 0.70 seconds while its
version helper's descendant remained connected for its own thirty-second
lifetime. The timeout verdict also depended on the platform's kill status:
Windows' numeric termination code refused a running probe that Unix accepted
as signal termination.

`rook-contain::probed_within` retains the observed status separately from a
still-running deadline outcome. It checks status before the clock, so a delayed
observer does not replace an already-finished shim's refusal with a timeout.
Standard input is null and output is discarded. Every outcome requests owned
group and direct-child cleanup, including a normal exit. Direct-child reaping
polls for at most one additional second. Discovery maps the original outcome
to admission before cleanup status can change its meaning; spawn/status errors
remain refusals.

The existing capture reader shares this owned-child guard. Its combined output
ceiling, pipe-EOF deadline, partial-output refusal and successful-output behavior
are unchanged. Independent discovery checks now use a
[four-worker pool](lsp-discovery-pool.md) with stable language result order. The
pool is per discovery call; an overall startup deadline is not introduced.

Process creation/signalling remains synchronous. Darwin group cleanup retains
its additional five-second grace. Windows job attachment follows spawn, and
detached descendants retain the existing group-ownership limits. Best-effort
cleanup does not establish a universal containment or security guarantee.

The short discovery deadline idea comes from pinned Hermes
`22063fc099ec9ad8357d7f8f6e6248a0400680d3`,
`hermes_cli/doctor_tools.py::_run_ok`. Rook's owned status probe, its admission
semantics and cleanup are independently implemented; the reference is not
claimed to provide an equivalent process-group API. No upstream code, new
dependency, configuration or persisted layout is introduced.

Containment tests cover retained success/refusal, a distinct timeout result,
spawn failure and descendant-connection retirement after a normal parent exit.
The [actual CLI fixture](../xtask/probes/lsp-detection/README.md) also checks
discovery verdicts and cleanup on an extracted or explicitly identified binary.
These are subprocess-contract checks, not real-server indexing or model-quality
evidence.
