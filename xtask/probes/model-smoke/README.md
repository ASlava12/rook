# Model preparation checks

The optional Ollama smoke must finish preparation before judging any scenario.
`verify.py pull` reads bounded NDJSON from stdin, rejects error records even
after a success record, and requires a final `success` at EOF. `verify.py tags
--model NAME` requires that exact model in the bounded inventory response.
Validation uses explicit checks and also works with optimized Python.

`prepare.sh OLLAMA_URL MODEL` bounds curl connection/transfer time, checks
pipeline failures, and permits three preparation attempts. The workflow also
bounds the total step duration at 20 minutes. A preparation
failure stops before `cargo xtask smoke`; it is not a model-quality result.
Progress messages are emitted on status changes and every 30 seconds of
continuing progress. No model is installed by the offline regression suite:

```sh
python3 xtask/probes/model-smoke/check.py
```

The suite requires Python 3, curl and Bash (Git Bash on Windows). It checks real
local HTTP 200 error/success responses through curl and the verifier, terminal
completeness, malformed/oversized progress, model availability and optimized
Python. It also runs the actual preparation script against owned HTTP fixtures:
transient-error recovery, three failed pulls, absent model after pull success,
incomplete transfer after a success record, and exhausted readiness. Only sleep
is shortened in the child test shell. It does not claim remote download
reliability or improved model task quality. Ollama documents streamed errors
retaining the original HTTP status in its
[API errors contract](https://docs.ollama.com/api/errors), and final pull status
in its [API reference](https://github.com/ollama/ollama/blob/main/docs/api.md#pull-a-model).
