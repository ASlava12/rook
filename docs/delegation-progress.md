# Delegated progress and durable results

Parent views receive a short display hint when a delegated child announces a
tool call. Text and tool-argument fragments do not enter that hint queue. Hints
describe activity; they do not certify a finished operation or current files.

The shared engine keeps the latest pending hint for each child. A slow reader
can replace intermediate hints; at the configured child limit, the oldest
pending child hint is evicted to admit a newly active child. Updating an existing
child retains its delivery position. Producers never wait for a display reader.
Blocking delegation, the background nursery and independent checkers use the same
delivery implementation in CLI, TUI, daemon/browser and ACP-driven turns.

```toml
[agent]
delegation_progress_entries = 32
```

The limit accepts 1–128 pending child hints per queue. Directly constructed invalid
configurations are also clamped to this range. Each hint admits at most 2,048
UTF-8 bytes before copying. Tool display fields already admit 1 KiB in
`calls::doing`. Thus retained hint text is at most 64 KiB per default queue,
256 KiB at the maximum, plus entry and channel overhead. One bounded wakeup slot
contains no producer text. Receiver cancellation preserves the next pending hint;
closing the view releases retained hints. Sender closure drains admitted hints.

This coalescing is limited to intermediate parent display hints. Actual child
tool calls, tool-result records, execution receipts, child outcomes and scoped
observer lifecycle/result events retain their existing delivery and persistence.
ACP transport and daemon chat delivery have their own bounds. No saved postcard
struct, stable progress/wire enum or execution journal format changes.

## Reproducible audit

The core fixture executes 256 actual file reads through `Crew::run_subtask`, with
its parent display reader deliberately delayed until the child finishes. It
compares the same 1 KiB answer delivered in one versus 1,024 text fragments, with
28 KiB of older parent conversation already saved. The final read names another
file, so the fixture also checks that the surviving hint is the latest call.

| Observation | Previous queue | Coalesced queue |
|---|---|---|
| One child's pending display high-water | 256 | 1 |
| Saved child conversation records, 1 / 1,024 fragments | 787 / 787 | 787 / 787 |
| Actual saved tool calls / results | 256 / 256 | 256 / 256 |

The current engine fixture measures 515 successful `save_with_claim` execution
companion writes in each fragmentation case, totalling about 322 KiB across all
updates. The largest individual receipt is 737 bytes. These counters measure
that execution companion save path, rather than every database transaction.
The fixture separately measures the actual child journal: about 42 KiB logical
body bytes, with no copied parent conversation. Variable timing fields can
change its exact byte count. No full-history snapshot is saved for a hint or
text fragment. Critical observers still receive all 256 tool announcements,
256 saved-result notifications and the child's start/end events.

Reached delivery fixtures cover more children than available slots, last-hint
replacement, Unicode byte admission, receiver cancellation/closure and invalid
configuration ceilings. The native HTTP-provider scenario drives blocking and
background delegation through real local CLI and an owned scratch daemon. It
reopens both stores and checks all 256 call/result pairs, final result sequences,
clean execution receipts and exact child outcomes. It uses an intentionally
ready provider and disables the unrelated tool-cycle guard for the workload;
it makes no real-model quality or throughput claim.

```sh
cargo test -p rook-core --lib delegation -- --nocapture
cargo test -p rook-cli --test cli coalesced_delegation_keeps -- --nocapture
cargo xtask ci
```
