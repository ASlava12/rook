# Anthropic image-only message compatibility

Anthropic user messages and native tool results can contain images without a
caption. The transport omits an empty or whitespace-only text block alongside
those images. Nonblank captions retain their exact bytes, including surrounding
whitespace. Text-only tool results retain their original empty/whitespace data;
no synthetic output marker is inserted.

A cacheable image-only user message places its ephemeral cache boundary on the
last image. Five-minute caching keeps the default TTL omission; one-hour caching
keeps `ttl: "1h"`. Uncached messages gain no marker. Image order, media types and
base64 payloads remain intact. Tool images stay inside their native `tool_result`
with the original call IDs and consecutive result grouping.

This is an Anthropic wire conversion change. Saved messages and the caller's
input are not rewritten. Normal attachment frontends usually supply nonblank
source framing; the reproduced native case is a valid legacy saved conversation
with a bare-image user turn followed by an assistant turn. Adjacent user messages
can legitimately merge with a new nonblank prompt, which does not exercise this
case. Plain blank user messages without images and other providers are unchanged.

Owned HTTP tests cover empty, ASCII-whitespace and Unicode-whitespace captions,
multiple distinct image payloads, both cache TTLs, uncached images, real captions,
and parallel tool results with plain blank outputs. The
[native replay fixture](../xtask/probes/anthropic-image-only/README.md) additionally
checks ordinary streaming CLI and daemon paths and preserves the original saved
event and body. Neither is evidence of a live Anthropic refusal, cost reduction
or general improvement in image interpretation.

The implementation follows the image and cache representations documented by
[Messages](https://platform.claude.com/docs/en/api/messages/create),
[tool results](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)
and [prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).
Pi's blank-block handling informed the narrow compatibility review; no generic
empty-tool placeholder or upstream code was copied.
