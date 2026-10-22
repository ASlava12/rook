# Release asset body recovery

The self-updater and GitHub language-server installer share one verified asset
reader. An interrupted body retains its received prefix in memory and requests
the missing range. There are at most three application attempts, each retaining
the existing four-hop release-host policy, within one 600-second fetch budget.
The connection patience remains fifteen seconds. HTTP refusals, invalid range
metadata, size limits and digest mismatches terminate the fetch.

A resumed `206` must have one unambiguous decimal `Content-Range`, the exact
requested start, the final byte of the representation and a consistent bounded
total. A successful full response that ignores Range clears the old prefix and
hash state before reading again. Known sizes also detect a clean but premature
EOF. Unknown metadata sizes can learn a total from admitted response headers.
If all bytes arrive but HTTP framing fails, another attempt starts from the
beginning rather than requesting an empty remainder.

The release client requests identity encoding and disables automatic body
decoding and internal request retries. Range offsets and SHA256 therefore refer
to the same bytes. Explicit transient attempts return to the original asset URL
and reapply the release-host policy at every hop. Request errors omit embedded
URLs from their displayed reason.

All bytes must match the advertised full SHA256 and admitted size before the
existing installation or self-update staging begins. Cache ownership, placement,
previous-version retention and rollback keep their existing callers. Prefixes
are not persisted between invocations. A very slow transfer can still exhaust
the shared budget; there is no download-speed guarantee.

The asset-content limit remains 256 MiB and API bodies remain limited to 4 MiB.
Checks precede appending the next chunk. These are retained-content limits,
not bounds on all process allocations or decompressed archive contents.

Owned HTTP tests cover interrupted and prematurely terminated bodies, ignored
Range, unknown metadata sizes, conflicting/changed/overflowing ranges, request
limits, encoding/status/digest refusals, redirects and the shared deadline.
The [actual CLI fixture](../xtask/probes/asset-download-recovery/README.md)
additionally exercises `rook update --force` in private installation trees.
An unchanged transport reproduces two failures with `error decoding response
body` while the digest-refusal control passes. This explains lost progress on
that class of error; it does not establish which external component interrupted
the user's original download.
