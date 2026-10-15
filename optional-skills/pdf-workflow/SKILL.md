---
name: pdf-workflow
description: Use when splitting, merging, creating or filling PDFs and checking their output.
version: 1.0.0
license: MIT
requires:
  agent: ">=0.13.0"
  language:
    python: ">=3.10"
---

# PDF workflow

Identify the source files, page ranges, encryption, forms, scan quality and
requested output. Use Rook's `read_pdf` for bounded reading and its documented
continuation; it already handles extraction, rendering and optional OCR. Preserve
file/page attribution and do not equate an empty text layer with an empty page.

For manipulation use `pypdf`; for new typeset documents use `reportlab` when
available. Check imports before choosing a branch. Read
the examples below for creation, page manipulation and form
recipes. Use explicit output paths and keep the original by default.

Confirm whether a requested page range is one-based inclusive before converting
to library indices. Verify page counts and order after extraction or merging.
Preserve bookmarks and metadata when requested. Modification generally
invalidates digital signatures; surface this when the input is signed rather
than calling the changed file a signed original.

For forms, list exact field names/types and export values first. Fill a copy,
then read values back. A stored non-Latin value can still render blank if the
field font lacks glyphs. Filling is not flattening: flatten only with a capable
tool and check the appearance afterward. Do not invent signatures or derive
missing personal values.

OCR is evidence with possible recognition errors, not an authoritative rewrite.
Compare important numbers and names against page images. Keep source language,
page boundaries and uncertain spans visible in downstream analysis.

Reopen every produced PDF to verify structure and the requested content. Render
representative changed pages using available Poppler helpers; check text clipping,
fonts and form appearances. A parsing check and a visual check establish different
things. Deliver the output and identify skipped OCR/render/signature checks.

## PDF recipes

Read with Rook's `read_pdf` first when available. Use `pypdf` for manipulation
and `reportlab` for creation; they are optional task dependencies. Check the
installed version's help/API before applying an advanced transformation.

### Create and inspect a small PDF

```python
from pathlib import Path
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas
from pypdf import PdfReader

output = Path("report.pdf")
if output.exists():
    raise FileExistsError(output)
page = canvas.Canvas(str(output), pagesize=A4)
page.drawString(72, A4[1] - 72, "Inspection report")
page.showPage()
page.drawString(72, A4[1] - 72, "Appendix")
page.save()
check = PdfReader(output)
assert len(check.pages) == 2
assert "Inspection report" in check.pages[0].extract_text()
```

The example uses ASCII. For Cyrillic or another script, register and embed an
available font covering the actual characters; the default Helvetica is not a
general Unicode font. Check the rendered text, not only extraction.

### Extract and merge pages

```python
from pathlib import Path
from pypdf import PdfReader, PdfWriter

source = PdfReader("report.pdf")
output = Path("selected.pdf")
if output.exists():
    raise FileExistsError(output)
writer = PdfWriter()
writer.add_page(source.pages[1])  # source page 2, zero-based library index
writer.add_page(source.pages[0])
with output.open("xb") as stream:
    writer.write(stream)
check = PdfReader(output)
assert len(check.pages) == 2
assert "Appendix" in check.pages[0].extract_text()
```

For a whole-file merge, `writer.append(input_path)` preserves more document
structure than manually adding pages. Confirm bookmark and form behavior for
the installed library and requested inputs.

### Forms and visual checks

List `PdfReader(form_path).get_fields()` first. Clone the source into a
`PdfWriter`, then use its `update_page_form_field_values` for the specific pages
and exact field names. Set only supplied values and reopen the output to compare
field values. Do not label this as flattening or signing. Inspect appearance
streams/font coverage through a rendered page, especially for non-Latin values.

With Poppler, render an explicit page range into an owned directory, for example
`pdftoppm -f 1 -l 2 -scale-to 1600 -png report.pdf /absolute/task-temp/page`.
Bound runtime and output size. A successful parse does not prove that text and
form appearances are visible.
