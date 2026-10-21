# Owned managed-work runtime probe

Requires Python 3.11 or newer, Node.js and a directory containing matching `rook`
and `rookd` binaries. Run without Python optimization, including
`PYTHONOPTIMIZE`; the probe refuses optimized execution because its assertions
are part of the verification.

```sh
python3 xtask/probes/deferred-work/run.py --bin-dir /path/to/extracted/package
```

The probe creates isolated state and workspaces, starts its own daemon and local
scripted HTTP provider, and cleans up its owned processes. It does not use an
installed daemon, user store or live model. Set `ROOK_BUILTIN_SKILLS` to the
tagged source's `skills` directory when verifying an extracted release package.

Its 22 checks cover deferred checkpoints, process restart, same-session
continuation, skipped tool-batch tails, pause/cancel, human blockers and explicit
resume, retry activity, guidance wake-up, execution capacity and attached
WebSocket observers. In the ordinary-retry case, provider concurrency exceeds
the single managed execution slot: an unrelated workspace runs while the first
goal waits, and fresh guidance cannot bypass the occupied slot.

The JSON result records checks and physical request counts. A successful process
exit and all check flags are required. Scripted-provider results establish
runtime behavior, not live-model task quality. Archive verification and platform
CI remain separate requirements in the [release procedure](../../../docs/releasing.md).
