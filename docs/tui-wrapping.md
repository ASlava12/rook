# Wrapping long TUI text

TUI text with a left gutter and help lines keep the existing word, space,
paragraph and Unicode-scalar wrapping rules. A word wider than the available
width is split on scalar boundaries. Wrapping now counts a word once and visits
its split boundaries without repeatedly copying the remaining suffix. A zero
width uses one column so each iteration consumes input.

The change followed an audit prompted by Pi's nested-blockquote rendering fix
at `8a2ebd38549c3521a02db0e5d758a24320e97f94`. Rook's renderer does not use that
recursive blockquote algorithm; the audit found a separate quadratic loop in
its plain-text wrapper. The implementation was written independently.

Seven focused CLI/TUI tests cover wrapping, including a 65,536-character Unicode
word. Source-extracted execution of the actual changed function matched the
previous implementation on 671,844 cases using spaces, newlines, ASCII, Cyrillic,
CJK and emoji at widths 1 through 12. In one optimized local fixture with 65,536
CJK characters at width eight, the original function took 775,806 microseconds
and the changed function took 408 microseconds. Those are function-level fixture
observations, not a measurement of whole-TUI latency or agent task speed.
