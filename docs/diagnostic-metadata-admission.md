# Support report metadata admission

Support reports keep a one-MiB serialized output ceiling. The reader also admits
variable-length saved metadata before decoding it:

- The primary session and each of at most 64 ancestry records have a 64-KiB raw
  metadata ceiling. Oversized primary metadata is refused, including HTTP
  workspace routing before engine selection. An unavailable or oversized ancestor
  omits correlation with a notice while retaining the rest of the report.
- The captured recent event range retains the configured 1..2048 count ceiling,
  with at most 4 KiB of raw metadata per event. The normal read uses one bounded
  range transaction. If a record is oversized or unreadable, exact captured slots
  are inspected separately; readable neighbors survive and an omission notice is
  included. Newly appended events cannot enter the captured range. At most 2048
  fallback slots are read, without copying a refused label.
- Existing activity receipts admit 2048 bytes, execution JSON admits eight MiB,
  timing bodies admit one KiB and model-attempt bodies admit four KiB before
  parsing. Opt-in log tails keep their existing 64-KiB per-file limit. These
  separate input bounds are not a claim that the entire process uses one MiB.

Saved records and payloads are retained. Ordinary full inspection can still read
a record omitted from support output. Report fields, normal trace identities,
explicit log selection, the existing output ceiling and persisted formats stay
unchanged. A support notice never includes a refused private label or title.

Explicit session identifiers are parsed before the CLI or core requests a session
listing. Invalid identifiers likewise fail without unrelated metadata or HTTP
reads. The `last` alias retains existing enumeration semantics; that selection
step is outside these per-report reader bounds. This does not introduce an
end-to-end latency deadline or establish a cause for an earlier CI observer
request timeout.

Regression controls first failed all three cases against unchanged reader code:
oversized event labels, primary session metadata and ancestry metadata. The
candidate preserves neighboring events and current activity, refuses oversized
primary metadata, and omits only the unavailable trace. Existing diagnostic
execution/timing/privacy checks and actual source-client HTTP controls are also
required. Packaged CLI/daemon runtime verification remains a release gate; unit
success alone is not that evidence.

This follows the existing Pi-inspired committed-range and transcript-admission
audit, with Pi `482c1381a00d8d00bc81697e4a980c593db6803e`,
`packages/durable/src/harness/context.ts::captureContextBounds`, reviewed locally.
The support-reader limits and recovery are independent Rook implementations;
no upstream diagnostic API or code is copied. No new dependency, configuration
key, storage migration or MSRV change.
