---
name: docx
description: Use when creating or editing Word documents with structure and formatting.
version: 1.0.0
license: MIT
requires:
  language:
    python: ">=3.10"
---

# Word documents

Establish the deliverable, template, language and required preservation of
comments, revisions, fields, headers and layout. Use `python-docx` for ordinary
DOCX construction and edits after checking the import. See
the examples below for tested basic recipes.

Inspect paragraphs, tables, section settings, headers/footers and styles before
editing. Use semantic heading styles and the source template where provided.
Retain text provenance when extracting requirements from an existing document.
Save changes to a new path unless source replacement was requested.

Changing `paragraph.text` destroys its runs and can remove formatting and links.
For an existing document, locate the intended runs or use a transformation that
explicitly preserves the affected OOXML. Tables can contain nested tables;
text boxes and revision markup require separate handling. Do not describe a
body-paragraph-only extraction as the complete document.

`python-docx` does not supply a Word layout engine or a general tracked-changes
editor. A request involving revision acceptance, comments or field evaluation
needs a capable existing tool or a carefully inspected OOXML procedure. Preserve
unsupported parts and report the boundary. Do not substitute a plain-text rewrite
for a requested formatted edit.

Reopen the output and compare the requested content, heading order, tables,
sections and changed formatting. For a layout-sensitive document, render with
an available Word/LibreOffice service and inspect representative pages. Text
read-back proves content, not pagination. Report an unavailable render check
explicitly and deliver the actual DOCX with the verification performed.

## DOCX recipes

Requires `python-docx`. Use a task-local environment if the package is missing
and dependency installation is authorized. Creating a document is distinct from
rendering it: pagination needs a Word-compatible layout engine.

### Create and read back

```python
from pathlib import Path
from docx import Document

output = Path("report.docx")
if output.exists():
    raise FileExistsError(output)
document = Document()
document.add_heading("Inspection report", level=1)
document.add_paragraph("Source: the supplied fixture.")
table = document.add_table(rows=1, cols=2)
table.rows[0].cells[0].text = "Check"
table.rows[0].cells[1].text = "Result"
row = table.add_row().cells
row[0].text = "Content read-back"
row[1].text = "Passed"
document.save(output)
check = Document(output)
assert check.paragraphs[0].text == "Inspection report"
assert check.paragraphs[0].style.name == "Heading 1"
assert check.tables[0].cell(1, 1).text == "Passed"
```

For a template, start with `Document(template_path)` and inspect available styles
and sections before adding content. For existing rich text, change the intended
run instead of assigning a whole paragraph. Replacing text that spans multiple
runs requires preserving hyperlinks, field boundaries and formatting explicitly.

### Optional visual check

When `soffice` is available, convert in an owned output directory with a separate
profile so a running user's LibreOffice process is not commandeered:

```sh
soffice -env:UserInstallation=file:///absolute/task-temp/lo-profile --headless --convert-to pdf --outdir /absolute/task-temp/render /absolute/workspace/report.docx
```

Use an encoded absolute `file:` URI on the host platform, bound the subprocess
duration and retain conversion diagnostics. Check the produced PDF exists before
rendering pages with Poppler. Font substitution and layout differences can occur;
inspect the actual pages rather than inferring quality from the exit code.
