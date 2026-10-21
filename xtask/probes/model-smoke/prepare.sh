#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  echo 'usage: prepare.sh OLLAMA_URL MODEL' >&2
  exit 2
fi
base_url="${1%/}"
model="$2"
probe_dir="$(dirname -- "${BASH_SOURCE[0]}")"
python_command="${ROOK_SMOKE_PYTHON:-python3}"
request="$("$python_command" -c 'import json,sys; print(json.dumps({"model":sys.argv[1]}))' "$model")"

ready=false
for _ in $(seq 1 30); do
  if curl -fsS --connect-timeout 2 --max-time 2 "$base_url/api/tags" > /dev/null; then
    ready=true
    break
  fi
  sleep 2
done
if [[ $ready != true ]]; then
  echo 'model preparation failed: Ollama did not become ready' >&2
  exit 1
fi

for attempt in 1 2 3; do
  echo "model preparation attempt $attempt of 3"
  if curl -fsS --connect-timeout 5 --max-time 300 "$base_url/api/pull" --data-binary "$request" | \
      "$python_command" "$probe_dir/verify.py" pull && \
    curl -fsS --connect-timeout 5 --max-time 10 "$base_url/api/tags" | \
      "$python_command" "$probe_dir/verify.py" tags --model "$model"; then
    exit 0
  fi
  if [[ $attempt == 3 ]]; then
    echo 'model preparation failed after 3 attempts; no smoke scenarios ran' >&2
    exit 1
  fi
  sleep 5
done
