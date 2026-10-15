# Local PDF documents

`read_pdf` reads a workspace PDF through Poppler. It extracts ordinary UTF-8
text, so an ordinary text-only model can read a document without a PDF API,
cloud OCR or credentials. The CLI, daemon/browser and TUI use the same tool.
This tool reads workspace files; the chat attachment picker still accepts its
existing text/image formats.

Install Poppler's `pdfinfo`, `pdftotext` and, for optional rendering, `pdftoppm`
on the machine running Rook. Local OCR additionally needs `tesseract` and its
English language data. Missing programs produce an actionable tool error; Rook
does not install them or ask for a secret. Password-protected PDFs need an
explicitly unlocked copy; there is no password argument or automatic prompt.

```json
{"path":"manual.pdf","pages":"2-4"}
```

Pages are one-based, with the default selecting only page one. The reply carries
the exact snapshot's SHA256, total/selected page numbers, per-page extraction
method, text hash, and explicit continuation. To continue a long page, copy that page’s `next_cursor` into `cursor`, without `pages`. The cursor
binds the page, byte offset, source/text hashes and extraction mode. A changed source or changed extracted page refuses continuation.
`next_page` names the first page outside the selected range, when any remain.
Source text is untrusted document data, not executable instructions.

An empty text layer is not evidence of an empty page. The result says that
scans and vector-only pages need visual inspection or OCR. `mode="ocr"` tries local
English OCR on pages with no embedded text and labels the result approximate
and partial. It does not silently claim to transcribe figures, tables, formulas
or other languages. `mode="render"` selects one page and attaches its PNG through
the ordinary image limits; use this only with a model that can see images.
Embedded-text results also explain that figures, handwriting and reading order
may be missing.

```toml
[documents]
max_bytes = 33554432
max_pages = 8
max_text_bytes = 262144
timeout_secs = 30
```

The input limit is checked before and during a capability-relative read. Every
parser reads a private snapshot with a fixed filename; a user pathname never
enters a shell command. Selection is capped before starting helpers. Text,
metadata, diagnostics and image reads are bounded while bytes arrive; tool text
also fits `sandbox.max_output_bytes`. The whole operation is bounded by the
smaller of `documents.timeout_secs` and the command deadline. Cancellation kills
the owned helper process group and removes its private snapshot. Helpers use
the configured OS containment and request network access disabled; explicitly
disabling containment retains the existing sandbox semantics. The byte limits
bound Rook's reads and output, rather than claiming a hard parser RAM quota.

Editor-owned files and configured remote execution are refused rather than
reading an unrelated local path. Copy the selected source into the local
workspace explicitly when using this tool with such an environment.
