---
name: store-triage
description: Work out why a Rook store has grown, and reclaim space safely — read this before running gc or prune on someone's history.
version: 1.0.0
license: MIT
keywords: [storage, disk, gc, maintenance, rook]
requires:
  agent: ">=0.1.0"
---

# Triaging a large store

Answer "what is large" before deleting anything. Deletion is the last step, not the
first.

## 1. Measure

```sh
rook store stat
```

Read four numbers together:

- **logical** — what the agent produced.
- **stored** — what it occupies after compression. The ratio between these two is
  how well compression is working.
- **saved by dedup** — what content addressing avoided.
- **on disk** — index plus object files. If this is far above *stored*, the index
  itself is the bulk.

The per-kind table says which category dominates. A `file` row far above the others
usually means large captures; a `tool-result` row usually means uncapped command
output.

## 2. If the ratio is poor

A ratio near 1× on `message` or `tool-result` means no dictionary has been trained
yet:

```sh
rook store train
```

Dictionaries need at least 32 objects of a kind. Existing objects keep their old
encoding — they record the codec they were written with — so this improves new
writes, not old ones.

## 3. Find the offenders

```sh
rook store ls --kind file --limit 50
rook session ls                  # events and token counts per session
rook store refs checkpoint/      # captures holding file blobs alive
```

A checkpoint of a directory that should have been excluded is the usual cause of a
large `file` total. Check `[storage]` exclusions in `config.toml`.

## 4. Reclaim, dry run first

```sh
rook store prune --dry-run       # what the retention policy would drop
rook store gc --dry-run          # what is unreachable
```

Read both outputs before running either for real. `prune` deletes *sessions*; `gc`
deletes *objects nothing references*. Sessions tagged `keep` or `pinned` are never
pruned automatically.

```sh
rook store prune
rook store gc
```

## 5. Confirm

```sh
rook store verify                # re-reads and re-hashes everything
rook store stat
```

## Do not

- Delete files under `store/objects/` by hand. They are addressed by the index;
  `gc` reclaims orphans safely and reports them.
- Run `gc` while `rookd` holds the store — it will refuse, and that refusal is
  correct.
- Raise `max_total_bytes` to make a symptom go away without finding out what is
  filling the store.
