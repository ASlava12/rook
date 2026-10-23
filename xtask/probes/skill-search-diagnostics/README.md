# Skill search diagnostics

Run `python3 xtask/probes/skill-search-diagnostics/run.py --bin-dir target/debug
--output /owned/path/report.json` after building `rook` and `rookd`.

Private local directories provide a readable skill and an unavailable source.
Mixed, fully unavailable and readable-but-unmatched searches run through both
the CLI's direct store and three owned daemon processes. The probe compares the
legacy JSON array with diagnostic `items`, checks actual source failures and
plain-text semantics, verifies the daemon API's retained errors, requires JSON
for the new flag, and confirms daemon retirement and unchanged binaries. There
are 27 aggregate conditions. Sources are local; no model or public Git host is
used. Fixture files and bounded output captures are retained under the reported
owned root. This does not establish remote Git availability or model quality.
