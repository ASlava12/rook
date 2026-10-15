---
name: presentations
description: Use when creating or editing a PowerPoint deck from content or a template.
version: 1.0.0
license: MIT
requires:
  language:
    python: ">=3.10"
---

# Presentations

Establish audience, purpose, source material, template, slide size and the final
file format. Build a short narrative before arranging slides. Use `python-pptx`
for PPTX after checking the import; adapt the concrete recipes in
the examples below.

For an existing deck, inventory layouts, masters, slides, text, charts, tables,
notes and embedded media. Preserve the template and edit only the requested
parts. Prefer editable text, shapes and charts when an editable deck is wanted.
Do not flatten a deck to images just to preserve its appearance.

Design one main point per slide with readable text, consistent alignment and
adequate contrast. Keep chart values tied to the supplied data and label units.
Use concise source attribution and speaker notes where needed. Missing data
stays missing; illustrative placeholders must be identified and removed before
a finished deliverable is claimed.

Save a separate output by default and reopen it to check slide count, order,
text, chart values and notes. When visual fidelity matters, render slides with
an available PowerPoint/LibreOffice engine and inspect for clipping, overlap,
font substitution and unreadable labels. `python-pptx` itself does not render.
See the example below for an optional conversion command and its limitations.

Legacy `.ppt`, complex animation and unsupported embedded objects may need a
different tool. Name such limitations instead of silently discarding features.
Deliver the PPTX and any requested PDF/preview, with content and render checks
reported separately.

## PPTX recipes

Requires `python-pptx`. The basic example creates editable content and notes.
For a supplied template, inspect its named layouts and placeholders instead of
assuming that its layout indices match the default PowerPoint template.

### Create and read back

```python
from pathlib import Path
from pptx import Presentation
from pptx.util import Inches

output = Path("report.pptx")
if output.exists():
    raise FileExistsError(output)
deck = Presentation()
deck.slide_width = Inches(13.333)
deck.slide_height = Inches(7.5)
slide = deck.slides.add_slide(deck.slide_layouts[1])
slide.shapes.title.text = "Inspection results"
slide.placeholders[1].text = "Source-backed content\nVerified output"
slide.notes_slide.notes_text_frame.text = "Source: supplied fixture; illustrative deck."
deck.save(output)
check = Presentation(output)
assert len(check.slides) == 1
assert check.slides[0].shapes.title.text == "Inspection results"
assert "Verified output" in check.slides[0].placeholders[1].text
assert "supplied fixture" in check.slides[0].notes_slide.notes_text_frame.text
```

### Edit and verify

Walk text frames, table cells and chart data separately. Changing
`shape.text` replaces paragraph/run formatting. Preserve the requested style
by editing individual runs where possible. Confirm chart category/series order,
units and source values rather than checking only its image.

Optional rendering uses a separately owned LibreOffice profile:

```sh
soffice -env:UserInstallation=file:///absolute/task-temp/lo-profile --headless --convert-to pdf --outdir /absolute/task-temp/render /absolute/workspace/report.pptx
```

Use an encoded absolute profile URI, a subprocess deadline and retained logs.
Verify the PDF page count against the expected slides, then render pages with
Poppler. Inspect clipping, overlap, charts and fonts. If the renderer is missing,
deliver the checked PPTX and explicitly report that the visual pass was not run.
