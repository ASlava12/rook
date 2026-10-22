# Bounded language-server discovery

Independent PATH language-server version checks use at most four workers per
discovery call. Each candidate receives one result slot; accepted servers retain
known language order even when starts and completions occur in another order.
The caller also consumes queued work. Failure to create an extra worker leaves
the caller and remaining workers able to finish unclaimed candidates.

Automatic workspace discovery still determines applicability from bounded
metadata before process creation. Notes-only folders run no PATH probes, and
single-language folders omit unrelated commands. Full inventory callers retain
all five known candidates. Explicit configuration still replaces automatic
discovery and does not execute a version check. Installed payloads retain their
probe bypass. Missing commands and refused nonzero exits stay unavailable; a
command still serving at its ten-second patience remains eligible after owned
cleanup, as before.

Each invocation retains the existing status/ownership contract in
[the detection reader](lsp-detection-lifetime.md). There is no daemon-wide process
cap or total startup deadline: simultaneous discovery calls own separate pools,
and process creation/signalling remains synchronous. Bounded reap polling,
Darwin cleanup grace, Windows post-spawn job attachment and detached-process
limits remain. No dependency, configuration, durable format or MSRV change.

Hermes `22063fc099ec9ad8357d7f8f6e6248a0400680d3`,
`hermes_cli/doctor_connectivity.py::run_probes`, supplies the architectural idea
of a fixed pool with submission-order results. Rook independently adapts its
existing environment-version pool pattern to local LSP process discovery. No
upstream code is copied; the remote HTTP pool is not claimed as an LSP port.

The [actual CLI fixture](../xtask/probes/lsp-discovery-parallel/README.md) uses
five owned pending version commands in one mixed workspace. Published 0.21.11
shows one acknowledged live probe at a time and fails only the overlap condition,
with seven other controls passing. The local development candidate passed all
eight conditions with four overlapping probes and all connections retired before
fixture cleanup. Controlled timings were about 50.9 and 20.7 seconds; they are
not production latency measurements or a universal speed guarantee.

Three deterministic unit controls cover the four-worker bound and exact-once
admission, empty/partial discovery, and reversed completion with stable results.
The existing eleven LSP regressions cover installed payload bypass, shared real
starts, failures and shutdown. Workspace fixtures retain exact call multiplicity
with physical start order free to vary; language output order remains mandatory.
Actual source fixtures run on native Linux, Windows and macOS in CI. A completed
source run and later downloaded-package verification remain separate release
requirements; local development observations do not establish either result.
