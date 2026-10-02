#!/bin/sh
# Fixture output is deliberately written even at EOF: a cancelled hook must be killed.
IFS= read -r payload
printf '%s\n' '{"ui":[{"kind":"status","id":"stream","text":"FORM_DISPLAY_ONLY"}]}'
printf '%s\n' '{"form":{"id":"setup","title":"Typed extension setup","fields":[{"kind":"text","id":"name","label":"Name"},{"kind":"select","id":"target","label":"Target","choices":["local","remote"]},{"kind":"confirm","id":"confirm","label":"Continue"},{"kind":"integer","id":"count","label":"Count","min":1,"max":10}]}}'
IFS= read -r answer || true
printf '%s' "$answer" > "$1/hook-answer.json"
printf '%s\n' '{"reply":{"context":"EXPLICIT_FINAL_CONTEXT"}}'
