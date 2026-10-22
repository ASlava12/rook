# Bounded environment probing

The first skill catalog asks for installed language/tool versions. Sixteen
independent commands previously ran sequentially, making their waits accumulate
before an empty chat could consume input. Environment detection now uses at most
four active probes per invocation, including the caller and up to three temporary
worker threads. It still runs once per existing lazy environment value.

Each command keeps the existing five-second patience, hidden-process setup,
version parser and direct-child cleanup. Failed or missing tools remain absent
from the same language/tool maps. Output slots follow the static input inventory,
and the resulting maps retain their deterministic ordering regardless of which
command finishes first. Failed worker creation leaves the caller and remaining
workers to drain unclaimed probes; it does not drop queued inventory entries.

The bound applies to each invocation, not globally across independent Rook
instances. This does not establish a deadline for an entire application startup,
change the existing subprocess output/descendant contract or make environment
detection transactional against changes in installed tools. No persistent cache,
configuration key, dependency or model request is added.

The implementation independently adapts the pinned Hermes connectivity doctor's
bounded parallel-probe idea to local version commands. Hermes uses eight workers
for HTTP checks; Rook uses four slots for processes and retains its own command
deadlines and parsing. It does not copy the upstream implementation.

Deterministic tests saturate the four-probe bound, reject oversized requested
parallelism, force a later probe to finish first, exercise serial/empty input and
preserve missing results and exact invocation counts. Owned CLI measurements use
four isolated two-second programs and compare actual process intervals and
version maps, without turning a wall-time speed estimate into a statistical gate.
