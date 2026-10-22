# Transcript metadata admission

Navigable transcript reads admit saved session metadata at 64 KiB and each event
metadata record at 16 KiB before deserializing variable-length fields. The same
event bound applies to page acquisition, search batches, individual entries,
quotes and explicit image reads, previous/next existence probes, and neighboring
timing/change/detail/image records used to decorate a result. `session_recall`
shares the page/search readers.

Forward acquisition applies its captured exclusive head to the database range,
before admitting metadata, as reverse reads already did. Frozen search cursors,
page acquisition and next/previous probes cannot inspect later appended records.
Optional timing measurements on a page use that same captured head. Otherwise an
unrelated oversized future note could fail a read of valid older evidence even
though the renderer would eventually omit it. Individual entry decoration keeps
its existing live timing lookup; fixed event reads still admit only the captured
session head.

A record that exceeds the bound returns an explicit read error naming the session
or event. It is neither silently removed nor evidence that a saved action did not
happen. The original journal is unchanged. Ordinary page and body output budgets,
forward cursors, exact result/companion attribution and fork boundaries retain
their existing behavior. Older non-navigable service readers keep their existing
contract through wrappers around the bounded companion helpers.

This adds admission for metadata decoding. It does not impose a new payload cap
on explicit large-body retrieval, which remains available through existing range
reads. Verified compressed range reads still traverse the complete object;
inline encoded payloads are copied by the store reader. These metadata limits
therefore do not establish a bound on total transcript read latency, I/O or heap.
The checker's separate acquisition/payload limits are described in the 0.21.6
release notes.

The follow-up was prompted by Pi's pinned committed-range context reader
(`482c1381a00d8d00bc81697e4a980c593db6803e`,
`packages/durable/src/harness/context.ts`) and extends Rook's own metadata reader
admission independently. No upstream implementation is copied.

The regression suite drives public transcript and recall APIs with oversized
event labels, oversized session headers and unrelated neighboring records.
The three event/neighbor fixtures failed against the original implementation.
An additional frozen-search regression reproduced a read failure caused by an
oversized event beyond the saved cursor head. Store range tests cover both
metadata boundaries, exclusive/empty/inverted ranges, count limits and session
isolation. A saved-measurement fixture excludes future timing and oversized notes
while retaining an in-range exact measurement and legacy lookup behavior.
Normal long-session paging, escaped serialized page budgets, large-body search
continuation, UTF-8 quotes, saved measurement linkage and inherited history remain
covered by the existing transcript and recall suites. Local Windows validation
does not substitute for the required platform CI jobs or downloaded release checks.
