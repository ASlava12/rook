# Workspace admission before language-server probes

Opening an automatic workspace first scans bounded server metadata for matching
files, then probes only the applicable PATH commands. An empty/notes-only folder
executes no language-server version commands. A Python-only folder does not ask
Rust, TypeScript, Go or C servers to identify themselves.

The scan retains the existing 4,000-entry/six-level budget, ignore rules and
no-follow-link behavior. It is an applicability hint, not a complete filesystem
index. Known language order, command arguments and extension sets remain fixed.
Explicit nonempty `[[lsp]]` configuration still replaces automatic discovery;
disabled entries remain disabled and explicit commands are not version-probed.
A configured language with custom extensions is not offered for automatic
installation merely because other extensions are present.
An applicable explicitly installed payload retains its PATH-probe bypass.

`detected()` and `configured()` retain full inventory behavior for callers that
ask for it. `for_workspace()` admits metadata before process creation;
`missing_here()` uses that same scoped automatic discovery when checking what
installation would be for, while retaining explicit configured languages. Applicable PATH probes retain their ten-second status/cleanup
contract and use a [four-worker discovery pool](lsp-discovery-pool.md). Known
result order remains stable while physical starts may overlap. No timeout,
configuration, storage schema or dependency changes; the pool is per discovery
call and does not introduce a total startup deadline.

The [owned actual CLI fixture](../xtask/probes/lsp-workspace-detection/README.md)
checks six workspaces with all five known server commands and all sixteen
inventory names shadowed. The published 0.21.10 binary physically executes all
five server version commands even for a notes-only workspace; its offered
language result remains correct. The candidate must skip inapplicable calls
while preserving mixed-language ordering, explicit overrides and refusal.
Elapsed times are observational, without a performance threshold or model
quality claim. No user daemon, installation or cache is changed.

Reading Hermes doctor discovery and its independent probe pool at
`22063fc099ec9ad8357d7f8f6e6248a0400680d3` prompted this independent audit.
Hermes does not supply this workspace-admission implementation; no upstream code
was copied.
