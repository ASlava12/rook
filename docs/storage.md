# Storage

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

Objects are keyed by their blake3 hash. Storing the same bytes twice is a hash
lookup and nothing else. A session that reads the same 40 KB file twenty times
costs twenty ~50-byte log records, not 800 KB.

This is also why a session log entry holds an `ObjectId` rather than a payload:
the log stays tiny and uniform, and dedup is automatic across sessions.

### 2. Trained zstd dictionaries, per object kind

This is where most of the ratio comes from, and it is the part that is easy to get
wrong. A 400-byte JSON message compressed on its own barely shrinks — zstd never
sees enough context to build a model. A 16 KiB dictionary trained on a few hundred
messages of the same shape turns each one into a few dozen bytes.

Dictionaries are trained per [`Kind`](../crates/rook-store/src/object.rs) —
messages, tool results, file blobs, skills, memories, snapshots — because those
populations have genuinely different shapes.

Retraining never invalidates history: **every object records the codec it was
written with**, so old objects keep decoding against the dictionary they were
written against.

Measured, on a synthetic transcript of 3,000 turns plus 300 tool results over 25
distinct source files (`cargo xtask compaction`):

| | size | ratio |
|---|---:|---:|
| logical bytes written by the agent | 21.88 MiB | — |
| after dedup (distinct objects) | 2.54 MiB | 8.6× |
| stored, standalone zstd | 0.60 MiB | 4.3× |
| stored, trained dictionaries | 0.12 MiB | **20.7×** |
| on disk, index + objects | 1.07 MiB | 20.5× end-to-end |

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
fails with `StoreError::FormatTooNew` rather than reading it wrong.

## Bounded growth

Retention is on by default, with real limits, because a default of "unbounded" is
the bug the survey kept finding:

```toml
[storage.retention]
max_session_age_days = 180
max_sessions         = 2000
max_total_bytes      = 4294967296   # 4 GiB
protect_tags         = ["keep", "pinned"]
```

`Store::prune` deletes the oldest unprotected sessions until the policy is
satisfied; `Store::gc` then reclaims what nothing references. Both have a dry run,
and the CLI's `--dry-run` is how you find out what a policy would do before it does
it. `rookd` runs both on a configurable interval.

Garbage collection is **mark-and-sweep**. Roots are every ref and every event body;
higher layers supply an expander so a snapshot manifest keeps its files alive. A
full sweep is O(objects) and runs in well under a second at realistic sizes.
Refcounting was rejected: refcounts drift after a crash or a manual edit, and a
store that miscounts silently deletes live data.

## Integrity

- Every read re-hashes the decoded bytes and fails on mismatch.
- `rook store verify` re-reads and re-hashes the entire store.
- Payload files are written to `tmp/` and renamed into place; the index entry is
  committed only afterwards. A crash in between leaves an orphan file, which the
  next `gc` reclaims.

## Concurrency

redb allows one writer process at a time. `rookd` normally holds it, and the CLI
says so plainly rather than surfacing a lock error nobody can act on. Routing the
CLI through the daemon when it is running is [ADR-0006](adr/0006-single-writer-store.md).
