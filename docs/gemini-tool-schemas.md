# Gemini tool schemas

The native Google provider sends tool declarations through
`FunctionDeclaration.parametersJsonSchema`. Rook tools, including MCP tools,
carry JSON Schema. Google's older `parameters` field is a typed OpenAPI Schema
message and cannot represent all those keywords. The two fields are mutually
exclusive, so a declaration contains only `parametersJsonSchema`.

This applies to both `generateContent` and `streamGenerateContent`. Tool names,
descriptions, required fields, definitions and references remain in the supplied
schema. The Google API still decides which JSON Schema features it supports;
using the JSON Schema field does not promise support for every keyword or for
third-party endpoints that implement only an older API shape.

For Gemini compatibility, string and number `const` constraints become
single-value `enum` constraints when that schema has no explicit `enum`.
An explicit `enum`, and boolean, null, array or object `const` values, remain
unchanged. This conversion does not validate schemas or repair contradictory
constraints.

Conversion follows schema-bearing keywords and named schema maps, including
properties, definitions, array items and conditional/composition schemas. A
property or definition named `const` or `properties` keeps its name. Values of
`default`, `examples`, `enum`, object/array `const` and unknown annotations stay
literal data, even when they contain objects that resemble another schema.
The supplied tool schema is not mutated; conversion belongs to Google's wire
request, after construction of the canonical logical request.

The socket tests exercise real ordinary and streaming HTTP requests against an
owned loopback server, with nested constraints and literal-data controls:

```sh
cargo test -p rook-llm --test google --test google_schema
```

Those tests verify request encoding and response handling, not live Gemini
acceptance or model quality. See the official
[FunctionDeclaration contract](https://ai.google.dev/api/generate-content#FunctionDeclaration).
