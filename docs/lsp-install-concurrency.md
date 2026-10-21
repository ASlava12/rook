# Overlapping language-server installations

Independent installer values in one process share an overlapping operation for
the same cache, command, source, platform and proxy profile. The shared operation
owns metadata lookup, verified download or build, extraction and publication.
Every participating caller receives its own result and path spelling.

Source identity includes the recipe, release API, explicit proxy policy, a hash
of proxy environment settings captured when the HTTP client was built, target
OS/architecture/userland and the required toolchain version. Different identities
targeting one cache command wait for the current owner, then perform their own
operation. Other cache directories and commands continue independently.

Cancelling one caller leaves the work available to remaining callers. Cancelling
all callers drops the shared future and uses the installer's existing download
and command cleanup. Completed successes and failures retire from admission;
a later explicit install performs a fresh operation. There is no permanent
positive or negative result cache.

Admission permits at most 64 active operations. Matching callers can still join
at capacity; a new destination is refused before HTTP or cache mutation. Weak
registry entries do not retain abandoned work. The short registry mutex protects
lookup/publication only; awaiting work and dropping last-owner resources happen
after unlocking. Cache identity resolves existing path components before applying
parent components, preserving symlink semantics without creating directories.

Ownership is process-local. Separate CLI/daemon processes retain independent
downloads and filesystem publication. Cross-process coordination is a separate
concern, as are real package build quality and language-server startup/indexing.
There are no new dependencies, configuration keys or stored formats.

The idea was reviewed in historical OpenCode
`55ecb0674846ad9f914fd852c53ad8e8deab7da4`,
`packages/opencode/src/lsp/server.ts`: its Roslyn setup shares one promise across
callers and clears it after completion. Rook independently implements bounded
ownership across installer values with cancellation and destination/profile
handling. No upstream code is copied or reference pointer advanced.

Owned HTTP tests check one physical download, independent caches, caller
cancellation, retry after failure/success, separate source/proxy operations and
64 admitted operations plus a refused new destination. Test payloads are never
executed. These API-level checks do not establish daemon background-install
receipt behavior; that native entry-point check remains required before release.
