# Public model and price references

`rook prices` reads the saved public reference **offline**. `rook prices --refresh`
explicitly fetches [models.dev's provider API](https://models.dev/api.json).
There is no fetch during a turn, config load, model selection or ordinary listing.
No endpoint credentials, credential commands or keychain helpers are used.

Use `rook prices --source cloud --json` to inspect a named source. The result
shows the public source, observation time/age, configured rates, reference rates,
missing fields and a review token. Apply the reviewed missing fields with:

```text
rook prices --source cloud --apply <review_token>
```

The terminal form is `rook prices --interactive`, also opened with `p` from
`rook config edit` after saving/discarding its draft. Select a source, use `r`
to refresh, Enter to review and `y` to apply. Escape cancels a running refresh.
The browser's **Prices** tab uses the same inspection/refresh/review/application
logic through `/api/models/prices`, `/refresh` and `/apply` respectively.
An active turn retains its existing provider/rates; subsequent turns read the
updated global config. Project settings retain their existing allowlist.

## Identity and precedence

References are public USD estimates per million tokens, not account billing.
Only exact named sources at recognised direct cloud addresses can match:

| Provider | Configured API | Base URL (optional trailing slash) |
|---|---|---|
| openai | openai / responses | `https://api.openai.com/v1` |
| anthropic | anthropic | `https://api.anthropic.com` |
| google | google | `https://generativelanguage.googleapis.com/v1beta` |
| deepseek | openai | `https://api.deepseek.com` or `/v1` |
| xai | openai | `https://api.x.ai/v1` |
| openrouter | openai | `https://openrouter.ai/api/v1` |

Named endpoints and inline addresses use the same structural resolver. A custom
address, local server, explicit proxy or inherited proxy environment keeps billing
identity unknown. `proxy = 'direct'` bypasses the inherited proxy restriction.
An API dialect, shorthand provider name, canonical-model alias or similar model
spelling cannot establish identity. Both provider and offering IDs must match
exactly; duplicate identities or differing object-key/ID pairs invalidate a fetch.
References for unsupported cloud addresses remain unknown; manual rates work as
before for any source.

Explicit configured rates are never overwritten, including zero. Only missing
input/output/cache-read/cache-write fields are filled. Reference context, tools
and reasoning support are display-only; live scoped observations and configured
context/capabilities keep their current precedence. Tiered/context rates,
different reasoning/audio rates, or offering-specific provider/experimental
overrides are visible but cannot be imported into Rook's flat-rate accounting.
Missing cache rates remain unknown, so nonzero cache usage cannot become free.
Zero public rates require the same explicit review as other rates.

Application recomputes the preview and checks the whole global-config revision,
source/endpoint identity, passive credential definition, passive environment-key
value and cache revision. Changed settings, account definitions, environment
keys or a refreshed reference require a new preview. Commands/keychain sources
cannot report external account rotation offline: their *definition* is the
scope, and they are never executed. The config editor and `config set` share a
write lock; the editor checks external changes under that lock before replacing
the file. Unrelated TOML/comments and literal dotted source names are preserved.
`models.<source>.price_reference` records the last explicit reference application
and applied fields for attribution; it does not select runtime rates. Manual edits
after that application retain precedence. Existing physical-attempt receipts
retain their saved numeric rates/costs, regardless of later refreshes or edits.

## Bounds and offline recovery

`[price_catalog]` configures `max_bytes` (8 MiB default, 1 KiB–16 MiB),
`max_providers` (512, 1–1,024), `max_models` (32,768, 1–65,536),
`timeout_secs` (30, 1–60), and `max_age_secs` (604,800, 0–2,592,000).
Refresh uses one fixed HTTPS URL, refuses redirects, admits Content-Length and
each decoded body chunk before copying, and enforces a deadline for the finite
catalog body. Provider/model counts are admitted while decoding, known identity
strings before retaining (512 bytes, UTF-8), unknown fields as borrowed raw slices.
All rates must be finite and within the existing 0–1,000,000 USD bounds.
An unsupported upstream cost field refuses the refresh rather than silently
discarding a possible billing rule.

`ROOK_HOME/cache/price-reference-v1.json` is a private atomic cache with a version,
observation timestamp and admitted raw data. Its envelope has 256 extra bytes;
file size is checked before reading and growth is bounded while reading. Invalid,
oversized or future-dated caches are unknown; stale data can be viewed but cannot
be applied. Failed refresh preserves the previous complete cache. Refreshing
does not apply prices, and no source/account credentials are copied into the cache.
The daemon admits at most two concurrent catalog operations and a 4 KiB apply
request. Blocking cache/config work runs outside engine locks. Browser responses
and old buttons belong to their originating view/review; they cannot overwrite a
new tab or submit another form's token. Saved store/wire formats are unchanged.
