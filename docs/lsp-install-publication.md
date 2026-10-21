# Cache mutation ownership across processes

Cooperating installers targeting the same cache and command now hold an
exclusive file lock while changing its version/current trees and final tag.
Different commands and cache roots continue independently. In-process sharing
still owns the whole overlapping operation; cross-process downloads remain
independent and are not a permanent result cache.

Release downloads acquire mutation ownership only after their bytes match the
publisher's digest. Metadata/digest refusal therefore creates no cache or lock
directory. Command-built packages acquire ownership before creating their
prefix or launching the package manager. The lease remains held through tag
publication, and command cancellation keeps existing descendant cleanup.

The cache's private `.install-locks` directory uses a stable command hash per
lease. Zero-byte coordination files persist after release; removing a live
lock file could let a new inode bypass its still-open owner. They contain no
credentials, routing profiles or PID-based ownership records. OS file-handle
lifetime releases the lease; callers never delete or steal a live owner's lock.

Lock admission uses nonblocking attempts with cancellable sleeps and a ten-minute
wait ceiling. Cancelling a waiter creates no detached work and never ends the
foreign owner. An ownership IO failure or expired wait returns an error before that caller
changes package files. Already installed files remain subject to the existing
cache rules; this is writer coordination, not a transactional multi-file read.

The implementation uses Rust's [file-locking API](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock),
stable since 1.89 and below Rook's 1.90 MSRV. No dependency, agent configuration
key or database layout changes. The kernel lease coordinates cooperating updated
installers, including independent CLI processes. Legacy writers and external
cache editors do not participate.

Five owned native unit regressions cover blocked/released handles, command/cache
independence, cancelled waiters, deadline/IO refusal and actual child-process
loss without deleting its coordination file. The
[CLI probe](../xtask/probes/install-publication/README.md) covers overlapping
processes, same/different release versions, caller results, final checked
payload/tag and digest refusal before all cache mutation. Its payloads never run.

The Windows baseline reproduces sharing error32 in three runs of the actual
published 0.21.2 CLI. A locally built guarded CLI succeeds in the corresponding
three runs. Downloaded-package and Unix evidence remain separate requirements;
scripted fixtures do not prove real package build quality or crash-transaction
recovery. This extends the earlier in-process install ownership audited from
historical OpenCode; no upstream code is copied or reference pointer advanced.
