# Skill previews and terminal columns, 2026-10-21

The pinned Goose reference at
`34a222e3f2b95d8b4a43482b53c2cc957ecce26d`,
`crates/goose-cli/src/commands/skills.rs`, uses grapheme-aware display-width
truncation and cell-based padding. Its preliminary scalar-count description
preview can still split a grapheme; Rook does not adopt that preliminary step.
No upstream implementation is copied and the reference pointer is unchanged.

Rook's installed and offered skill lists and the terminal chat `/skills` view
previously took 60 or 72 Unicode scalars. That can leave half an emoji sequence
or omit a combining accent. Shared table padding also counted scalars, causing
later columns to shift after wide glyphs and combining sequences.

The previews now fold whitespace into one line, retain whole extended graphemes
within their existing 60/72-column budgets and mark shortening with an ellipsis.
Copying stops at the column budget or 4096 preview bytes; unusually large
combining clusters are omitted whole, without normalizing the full description.
Shared table padding uses the terminal width model already provided by ratatui.
The existing Unicode segmentation dependency is reused. JSON output, stored
descriptions, model catalog cards and full skill bodies retain their contents.
This change does not promise terminal-width agreement with every font or emulator.

This is a follow-up branch based on the 0.21.0 candidate, outside that release's
frozen source. Formatting, CLI/all-target clippy, the four existing formatter
tests and a locked CLI build passed on Windows.

An isolated native CLI demonstration confirmed the original failures before the
change: an accent was omitted after the 60th scalar, an emoji ended at `👩‍`, and
40 CJK glyphs exceeded the 60-cell description width. After the change, both
fitting accents/emoji and omitted whole clusters render as expected, CJK previews
fit the width, and a cluster with 5000 combining marks becomes `…`. JSON retained
the original accent, emoji and unusually large cluster descriptions exactly.
These observations exercise installed skill listing and its original JSON path;
search and terminal chat reuse the shared helper, but were not separately driven
against a live source/provider. No model-quality improvement is claimed.
