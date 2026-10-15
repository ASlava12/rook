---
name: document-translation
description: Use when translating a long document while preserving terminology, structure and code.
version: 1.0.0
license: MIT
---

# Document translation

Establish source and target languages, authoritative input version, audience,
output format and whether a glossary or style guide already exists. Use this
workflow for the requested document translation, not as a hidden translation
layer around every user request.

Inventory sections, pages, tables, figures, footnotes, links and code blocks.
Extract bounded chunks with stable source coordinates. Use `read_pdf` for PDF
sources when offered; preserve the difference between extracted and OCR text.
Record unreadable source spans instead of translating guesses.

Create a small glossary of recurring terms, names, identifiers and units.
Preserve commands, flags, filenames, paths, API identifiers, code and literal
output unless their translation was requested. Translate surrounding explanation
without changing executable examples. Keep numbers, warnings, negation and
normative terms faithful to the source.

Translate by logical section with neighboring context, carrying the glossary
and decisions between chunks. Track each source chunk and output location in
an explicit ledger; see the examples below. Resume
from verified completed chunks rather than repeating the whole file after a
failure. If an API or optional service is unavailable or declined, retain work
and report the exact remaining scope.

Check coverage against the source outline and chunk ledger. Compare code blocks,
identifiers, numbers, links, table shape and terminology. Review difficult spans
against the original; reverse translation alone is not proof of accuracy. Keep
ambiguous or corrupt spans visible for review.

Reassemble the requested output and verify ordering, headings, cross-references
and page/layout constraints where applicable. Deliver the translated artifact,
glossary and coverage/uncertainty notes. Report partial translation as partial;
do not fill missing sections from model memory.


## Example record

```csv
source_file,source_revision,source_location,chunk_id,status,output_location,glossary_revision,notes
example.md,EXAMPLE_HASH,section-1,EXAMPLE-001,pending,,EXAMPLE_GLOSSARY,Illustrative ledger row; replace with actual source inventory
```
