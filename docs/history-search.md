# Unicode history search

Global history search (`rook search` and its shared API/UI paths) uses the
existing ranked term search over conversation/file objects. Query and text terms
now use Unicode default case folding after Rust lowercase mapping. For example,
`STRASSE` matches `Straße`, and Greek final/nonfinal sigma forms match. Equivalent
folded query terms are deduplicated before scoring. Memory retrieval and skill
ranking retain their own existing semantics.

```sh
rook search STRASSE
rook session find last STRASSE
```

Queries are at most 1,024 UTF-8 bytes. Empty/noise-only global term queries still
return no hits. A preview selects source text around the earliest matching term
on the best-scoring line, then normalizes whitespace for display. It keeps at
most 160 Unicode scalar values before normalization, plus omission ellipses.
The original journal/file bytes and casing are retained.

Paged literal search (`Rook::transcript_search` and its CLI/API/UI views)
uses the same folding semantics without changing literal whitespace: tabs,
newlines and repeated spaces remain meaningful. Reported snippet offsets are
original UTF-8 byte offsets, suitable for source quotes, rather than coordinates
in an expanded/lowercased copy. Fold expansions map to whole original code
points; `ss` can match one `ß`, and a query `ß` can match two source characters.
Case folding does not remove accents or promise canonical/compatibility Unicode
normalization or locale-specific collation.

The matcher uses a prefix/fallback table and a source-position ring bounded by
the folded query length. It does not retain a character-position vector for a
whole transcript body. Scan input and page counts retain the existing
`transcript.search_bytes`, `search_events`, and `page_entries` bounds. Page
overlap admits four source bytes per folded query character so an expansion can
straddle a byte-page boundary. Invalid/incomplete UTF-8 bytes break a match,
without shifting later source coordinates or synthesizing replacement-text hits.
Frozen `through` bounds still exclude later appends; searching changes no history.

The engine tests cover Unicode expansions, Greek sigma, an emoji at a scan
boundary, literal mixed whitespace, original-source slicing, long-line previews
and oversized query refusal. These are deterministic source-coordinate and
retrieval checks, not a benchmark of semantic search quality.
