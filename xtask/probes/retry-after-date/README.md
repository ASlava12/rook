# Owned Retry-After date probe

Run Python 3.11 or newer without optimization against an actual CLI:

```sh
python3 xtask/probes/retry-after-date/run.py --bin-dir /path/to/package/bin \
  --output /fresh/path/retry-after-date.json
```

Require exit zero and all eight flags. Five isolated CLI runs use a local model
API. A future HTTP-date must delay the second physical request until that date;
a numeric header must retain its requested delay. Past dates, dates beyond the
existing 120-second ceiling and invalid dates must leave ordinary backoff usable.
Each case must finish with exactly two main physical requests and the original
output allowance. The report retains request timestamps, header values, exits
and the CLI SHA256.

Captures are bounded to 128 KiB while draining, generation request bodies to
2 MiB and generation requests to six per case. Every CLI run has a 60-second
ceiling. Sentinels and executable hashes must stay unchanged. No real model,
tool command or installed daemon is used; only owned temporary state is cleaned.
The scripted checker/classifier replies do not establish task quality.

The actual published Windows 0.21.4 CLI fails the future-date condition by
requesting again before the date, while all seven controls pass. Corrected
source-native and downloaded-package checks are separate requirements.

The reference is Hermes `agent/retry_utils.py` at
`22063fc099ec9ad8357d7f8f6e6248a0400680d3` and
[RFC 9110 section 10.2.3](https://www.rfc-editor.org/rfc/rfc9110.html#field.retry-after).
Rook uses the already locked `httpdate` parser and keeps its existing wait ceiling.
