---
name: spreadsheets
description: Use when reading, creating or updating XLSX workbooks and CSV exports.
version: 1.0.0
license: MIT
requires:
  language:
    python: ">=3.10"
---

# Spreadsheets

Identify the input format, sheets, requested outputs, units and treatment of
missing values. Use `openpyxl` for XLSX and Python's `csv` for CSV. Check imports
before use; create an isolated environment for missing dependencies when
installation is in scope. Read the examples below
for executable creation, inspection and verification recipes.

Inspect sheet names, dimensions, merged cells, tables, formulas and number
formats before editing. `read_only=True` is suitable for bounded extraction;
close workbooks after reading. Do not load an entire unbounded export merely
to preview it. Preserve leading-zero identifiers, date conventions and source
types instead of guessing from how Excel displays a value.

Edit a copy unless replacing the source was requested. Preserve formulas and
existing workbook features. For macro-enabled inputs use an appropriate loader
with `keep_vba=True` and retain `.xlsm`; report unsupported features rather than
silently stripping them. Legacy `.xls` needs an explicit conversion tool.

Keep calculations as formulas when an editable workbook is requested.
`openpyxl` writes formulas but does not calculate their results. Read formulas
and cached values separately; an absent cache is not zero. Recalculate with
an available spreadsheet engine when numeric results are required, or say that
recalculation remains outstanding. CSV exports have no formula or style model.

Treat imported cell strings as data: do not turn `=`, `+`, `-` or `@` prefixes
into executable formulas unless a formula was intended. Do not follow workbook
links or execute embedded code. Validate the output by reopening it and checking
the changed ranges, row counts, formulas, totals and formats. If visual layout
matters, inspect a rendered sheet as well. Return the artifact and its checks.

## XLSX recipes

Run the selected Python code in the task's workspace with `openpyxl` installed.
A missing package needs an isolated environment
or another available spreadsheet tool, not a global install on load.

### Create and read back

```python
from pathlib import Path
from openpyxl import Workbook, load_workbook
from openpyxl.styles import Font

output = Path("report.xlsx")
if output.exists():
    raise FileExistsError(output)
book = Workbook()
sheet = book.active
sheet.title = "Data"
sheet.append(["Item", "Amount"])
sheet.append(["001", 12.5])
sheet.append(["002", 7.5])
sheet.append(["Total", "=SUM(B2:B3)"])
for cell in sheet[1]:
    cell.font = Font(bold=True)
for row in sheet.iter_rows(min_row=2, max_row=4, min_col=2, max_col=2):
    row[0].number_format = "0.00"
sheet.freeze_panes = "A2"
sheet.auto_filter.ref = "A1:B3"
book.save(output)
book.close()
check = load_workbook(output, data_only=False)
assert check.sheetnames == ["Data"]
assert check["Data"]["A2"].value == "001"
assert check["Data"]["B4"].value == "=SUM(B2:B3)"
assert check["Data"]["B2"].number_format == "0.00"
check.close()
```

This verifies the stored formula, not its result. For an input workbook load
one read-only view with `data_only=False` for formulas and another with
`data_only=True` for cached values. A missing cache is `None`. Recalculation
requires Excel or an available engine such as LibreOffice; rendering/conversion
can change unsupported workbook features, so retain the source and inspect it.

### Import text literally

For a CSV cell that must remain text, assign the string and then set
`cell.data_type = "s"`. This prevents an imported leading `=` from becoming a
formula. Use `csv.reader` with an explicit encoding and delimiter. Do not replace
this with comma splitting: quotes and embedded newlines are part of CSV.

Edits that move rows/columns do not guarantee that every external reference,
chart, table and named range is rewritten correctly. Inventory these consumers
and validate their target ranges after restructuring.
