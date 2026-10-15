---
name: diagram-maker
description: Use when a process or architecture needs a standalone editable diagram.
version: 1.0.0
license: MIT
---

# Diagrams

Extract the entities, directed relationships, boundaries and labels from the
user's material. Mark uncertain relationships instead of inventing dependencies.
Choose the output the user can use: Mermaid for a repository document, SVG/HTML
for a standalone artifact, or Excalidraw JSON for an editable whiteboard.

Lay out groups and connections before styling. Use a consistent direction and
short labels; split a crowded graph into meaningful views. Show trust or system
boundaries where they affect the explanation. Distinguish ordinary flow, error
flow and dependencies through labels or line styles as well as color.

For SVG, give the root a `viewBox`, an accessible title and description. Draw
connectors behind nodes and leave room for arrowheads and text. Escape labels
as XML data. Prefer inline assets and fonts already available to the reader;
a standalone file should not silently depend on external network resources.
See the examples below for a minimal example.

For Excalidraw, use its documented element schema with bound text, identifiers
and links. Parse the JSON and open it in a compatible renderer. A file named
`.excalidraw` is not proof of schema compatibility.

Check that every requested relationship appears with the correct direction.
Parse the source and inspect the rendered artifact for clipping, overlapping
labels and obscured edges. Source validation alone cannot establish readability.
Deliver the editable source and any requested export. State when rendering was
unavailable instead of claiming a visual review.

## Standalone SVG example

Adapt node names, coordinates and connections to the actual system. The labels
below are synthetic. The code verifies XML structure; opening the result in a
renderer is a separate visual check.

```python
from pathlib import Path
from xml.etree import ElementTree as ET

output = Path("diagram.svg")
if output.exists():
    raise FileExistsError(output)
svg = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 180" role="img" aria-labelledby="title desc">
<title id="title">Example request flow</title>
<desc id="desc">A client sends a request to a service.</desc>
<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#334155"/></marker></defs>
<rect width="520" height="180" fill="white"/>
<path d="M 190 90 H 310" fill="none" stroke="#334155" stroke-width="2" marker-end="url(#arrow)"/>
<rect x="30" y="55" width="160" height="70" rx="8" fill="#eff6ff" stroke="#334155"/>
<rect x="315" y="55" width="175" height="70" rx="8" fill="#eff6ff" stroke="#334155"/>
<g font-family="sans-serif" font-size="18" text-anchor="middle" fill="#0f172a"><text x="110" y="96">Client</text><text x="402" y="96">Service</text></g>
</svg>'''
root = ET.fromstring(svg)
assert root.attrib["viewBox"] == "0 0 520 180"
output.write_text(svg, encoding="utf-8")
```

For user-supplied labels, construct XML through an escaping XML writer rather
than substituting raw strings into markup. For a diagram with many nodes,
prefer an available layout engine or a few focused views over tiny unreadable
labels. SVG/HTML with scripts or external URLs needs the same trust boundary as
other executable/source content.
