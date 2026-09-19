# Storage

[English](storage.md) · [Русский](ru/storage.md)

## The problem

Agent transcripts are the most redundant data a developer tool produces. The same
system prompt, the same file re-read, the same directory listing, the same failing
test output — thousands of times, across sessions. Handled naively this becomes
gigabytes, and the surveyed projects show what that looks like in practice: a trace
log extrapolated at [~640 TB/year](https://github.com/openai/codex/issues/28224),
a [backup that 504s](https://github.com/agent0ai/agent-zero/issues/1819) because
it zips 1.3 GB inside one HTTP request.

## The three mechanisms

### 1. Content addressing

Objects are keyed by their blake3 hash. A duplicate reuses a readable object;
resubmitting the original bytes repairs a damaged object under the same hash. A session that reads the same 40 KB file twenty times
costs twenty ~50-byte log records, not 800 KB.

This is also why a session log entry holds an `ObjectId` rather than a payload:
the log stays tiny and uniform, and dedup is automatic across sessions.

### 2. Trained zstd dictionaries, per object kind

This is where most of the ratio comes from, and it is the part that is easy to get
wrong. A 400-byte JSON message compressed on its own barely shrinks — zstd never
sees enough context to build a model. A 16 KiB dictionary trained on a few hundred
messages of the same shape turns each one into a few dozen bytes.

Dictionaries are trained per [`Kind`](../crates/rook-store/src/object.rs) —
messages, tool results, file blobs, skills, memories, snapshots, documentation —
because those populations have genuinely different shapes.

Retraining preserves each previous dictionary as `<kind>.<generation>.zdict`
before atomically publishing the new `<kind>.zdict`. Readers retain every
generation across restarts and try them in turn, newest first: an object records
only that it used *a* dictionary, never which one. Trying is safe because a zstd
frame names the dictionary it was written with, so the wrong one is refused
rather than decoded into something else — and it is why a decode costs one
attempt per generation rather than a lookup. Scheduled maintenance trains only
the kinds that have none; `rook store train` retrains them all.

A dictionary that was lost before any of this cannot be recovered from a
backup of the store alone, and the objects it held are ballast: reachable from
live events, so collection by reachability never reaches them, and unreadable,
so nothing else will. `Rook::maintenance` and `rook store gc` remove them and
say how many, before the sweep — a container that will not decode cannot be
asked what it keeps alive. Only for that reason: a dictionary merely missing
from `dicts/` is a file to put back, reports differently, and costs nothing.
The events that named the removed objects keep their records, and their bodies
read back as gone rather than as broken.

Measured, on a synthetic transcript of 3,000 turns plus 320 tool results over 64
distinct source files (`cargo xtask compaction`):

| | size | ratio |
|---|---:|---:|
| logical bytes written by the agent | 23.31 MiB | — |
| after dedup (distinct objects) | 5.29 MiB | 4.4× |
| stored, standalone zstd | 0.63 MiB | 8.4× |
| stored, trained dictionaries | 0.14 MiB | **37.1×** |
| on disk, index + objects | 4.02 MiB | 5.8× end-to-end |

Sixty-four files rather than twenty-five, because a dictionary needs 32 samples
of a kind before it is trained: at twenty-five the file blobs never got one, so
the measurement exercised the message dictionary alone while the claim above is
one dictionary per kind. Both are trained now, which is what the run prints.
The September 17, 2026 rerun measured 4.02 MiB on disk; the earlier 1.07 MiB
figure is superseded. An isolated checkout of the original HEAD measured the
same 4.02 MiB, so this is not growth introduced by the audit fixes. Encoded
payload sizes remain unchanged. The disk total includes redb allocation and is
not the payload compression ratio.

Note the gap between "stored" and "on disk": the redb index has its own overhead,
and at this scale it dominates. `rook store stat` reports both, because reporting
only the flattering number is how a storage claim stops being true.

### 3. Inlining

Objects at or below 1 MiB after encoding live inside the redb index rather than
becoming their own file. An agent produces enormous numbers of tiny objects; one
inode each would waste more space in filesystem slack than the payloads occupy,
and would make the store slow to walk and awkward to back up.

Larger payloads spill to `objects/aa/bb/<hex>`.

## Layout

```
store/
  index.redb            redb: objects, blobs, refs, sessions, events, kv
  objects/aa/bb/<hex>   payloads over the inline threshold
  dicts/<kind>.zdict    trained dictionaries
  tmp/                  staging for atomic writes
```

### Tables

| table | key | value |
|---|---|---|
| `objects` | 32-byte hash | postcard `ObjectMeta` — kind, codec, sizes, created_at, external |
| `blobs` | 32-byte hash | the encoded payload, for inlined objects |
| `refs` | string | 32-byte hash. `skill/<name>/v/<ver>`, `skill/<name>/h/<ms>-<short>`, `checkpoint/<name>/…` |
| `sessions` | 16-byte big-endian ULID | postcard `SessionMeta` |
| `events` | 24-byte (session, seq) big-endian | postcard `EventRecord` |
| `kv` | string | operational values |

Keys are big-endian so redb's lexicographic ordering is also chronological: a
session's events form one contiguous, correctly ordered range.

Metadata is [postcard](https://docs.rs/postcard), not JSON: a `SessionMeta` is
tens of bytes rather than hundreds, and there are a lot of them.

### Format versioning

`format.json` carries a version. Opening a store written by a **newer** format
fails with `StoreError::FormatTooNew` rather than reading it wrong. Format 2
preserves dictionary generations; opening format 1 upgrades its marker under the
store lock. Older binaries then refuse the store instead of overwriting a dictionary
or failing to load retired generations. This upgrade does not recover lost data.

## Bounded growth

Retention is on by default, with real limits, because a default of "unbounded" is
the bug the survey kept finding:

```toml
[storage.retention]
max_session_age_days = 180
max_sessions         = 2000
max_total_bytes      = 4294967296   # 4 GiB
max_history_entries  = 50
protect_tags         = ["keep", "pinned"]
```

Any field may be omitted; an omitted one keeps the default above.

The age and count limits are enforced by `Store::prune`, which deletes the oldest
unprotected sessions first. The byte limit cannot be, because deleting a session
frees nothing until garbage collection runs, so nothing can tell whether the limit
has been met. `Rook::maintenance` enforces it instead: prune, collect, measure,
delete the next batch of oldest sessions, repeat. `rook store maintain` is that
whole cycle, and `rookd` runs it on a configurable interval.

`max_total_bytes` is measured against stored content, not against the file on disk.
redb reuses freed pages rather than returning them, so `index.redb` never shrinks
and a cap on its size could never be met.

Three things here are appended to rather than replaced: a capture per skill
write under `skill/<name>/h/`, an entry per memory change under `memory/h/`, and
a snapshot per `rook checkpoint` under `checkpoint/<name>/`. Each entry is a ref,
and a ref is a root — so until `max_history_entries` existed, every object they
named was immortal and the byte cap could not be met at any price. Retention now
keeps the newest entries of each and drops the rest; `None` keeps them all, which
is what the store did before.

Documentation the agent gathered is one ref per topic and version,
`docs/<topic>/<version>`, and reading a topic again replaces the set rather than
appending one — so it is bounded by how many topics have been asked about, and
`rook docs rm` is how one goes.

What it still does not touch is `skill/<name>/v/<version>`, which is one ref per
distinct version rather than one per write, and `memory/head`, which is the
current state. A store can therefore still sit above its byte budget
legitimately — `store maintain` reports how far over it is, and stops deleting
sessions once a round frees nothing, because at that point the history is what
is holding it and more sessions will not help.

Every step has a dry run, and `--dry-run` is how you find out what a policy would do
before it does it.

Garbage collection is **mark-and-sweep**. Roots are every ref and every event body;
higher layers supply an expander so a snapshot manifest keeps its files alive. A
full sweep is O(objects) and runs in well under a second at realistic sizes.
Refcounting was rejected: refcounts drift after a crash or a manual edit, and a
store that miscounts silently deletes live data.

Anything written in the last ten minutes is left alone whatever the marking says.
An object is unreachable between being written and the event that names it being
appended — and a checkpoint writes every captured file before the manifest that
holds them — so a collection landing in that window would take live data whose
only fault is being new. The daemon runs maintenance on a timer while turns are
running, which is exactly when that window is open. `rook store gc` says how many
it held back, so a store full of garbage that collects none of it explains
itself.

## What ends up in here

Everything, in the clear. A checkpoint captures the workspace as it stands,
`.gitignore` notwithstanding — it has to, or a rewind could not put back a file
the agent changed — so a `.env` is in the store the moment one is taken. Tool
results keep whatever a command printed, secrets included.

Nothing leaves the machine and nothing is encrypted. `rook search` answers the
question that follows from that: it scans captured files as well as the
conversation, and a hit in a file names the path and the capture it came from.
`rook session rm <id>` then `rook store gc` is how it goes away.

## Integrity

- Every read re-hashes the decoded bytes and fails on mismatch.
- `rook store verify` re-reads and re-hashes the entire store.
- Payload files are written to `tmp/` and renamed into place; the index entry is
  committed only afterwards. A crash in between leaves an orphan file, which the
  next `gc` reclaims.

### What a power cut can take

The index is never inconsistent: every commit is atomic, and a machine that
loses power comes back to a store that opens. What it can lose is the tail of
an unfinished turn.

Ordinary event appends are batched, but durability is explicit at the boundaries
where losing intent or an answer would change recovery:

- before an operation can run, and after its result is recorded;
- when a background operation or execution changes state;
- when an active work iteration or evaluation result is saved;
- at checkpoints, compaction, turn completion and store close;
- and every 256 events regardless.

A durable redb commit persists earlier commits as well. Events since the last
barrier can be lost, including unfinished streamed text. An external side effect
and a store commit cannot be made atomic together: if a crash falls between the
effect and its receipt, startup preserves the operation as unknown and pauses
changes pending inspection. `rook session recovery <id>` shows the receipt;
`work --resume` reuses the saved iteration and completed evaluation results.
It does not automatically replay uncertain commands.

The journal is session-scoped JSON in KV; existing postcard records are unchanged.
Recovery state serialization is bounded to 8 MiB, execution previews to 2 KiB,
background operations to 64, and a recovery report to 256 relevant related
executions. Full operation arguments and results stay in the session log. Work
recovery supports at most 256 scorecard checks. Prune and maintenance preserve
sessions carrying the internal `rook:execution` or `rook:work` protection tags.

## Concurrency

redb allows one writer process at a time. `rookd` normally holds it, and the
CLI routes over the daemon's API instead of refusing — every subcommand of
`store`, `session`, `skills`, `memory` and `checkpoint`, reads and writes
alike. `rookd` writes its address to `$ROOK_HOME/rookd.addr` on start and
removes it on shutdown, and a file left behind by a crash is ignored because
nothing answers there.

A turn cannot route the same way, because it writes as it runs. `rook run` and
`rook chat` go to the daemon's chat socket instead — the same engine and the
same conversation from the other side — and say which daemon they are using.
They used to fail here with advice the person had already taken: "start rookd
before them", said to somebody whose `rookd` was running, because it was
running. `rook acp` is the one that still meets the lock. Why one writer rather
than several is [ADR-0006](adr/0006-single-writer-store.md).
