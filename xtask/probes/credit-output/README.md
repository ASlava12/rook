# Owned credit-limited response probe

Run Python 3.11 or newer without optimization against an actual CLI:

```sh
python3 xtask/probes/credit-output/run.py --bin-dir /path/to/package/bin \
  --output /fresh/path/credit-output.json
```

Require exit zero and all ten flags. Seven isolated CLI runs use a local HTTP
provider. The successful case must send allowances 20000 then 7053 after a
402 naming 7117 affordable tokens. A repeated refusal must stop after that
single retry. Exhausted credit, malformed/tiny budgets, an already sufficient
allowance and non-payment refusals must not trigger credit recovery.

Each capture is bounded to 128 KiB while draining, every request body to 2 MiB,
and each CLI run to 60 seconds. Request counts are bounded. The report records
actual allowances, exits, bounded output tails and the tested CLI SHA256.
Owned workspace sentinels and the CLI must remain unchanged. No real model,
tool command or installed daemon is involved. Only owned temporary state is
cleaned. The scripted completion/checker replies do not establish task quality.

The published Windows 0.21.3 CLI fails the two retry cases and passes the other
eight controls. Library HTTP/stream fixtures separately prove that a later
request retains its original allowance and that partial output is never replayed.
Corrected native source and downloaded-package checks remain separate evidence.
