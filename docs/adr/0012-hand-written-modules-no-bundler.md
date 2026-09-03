# ADR-0012: The web UI is hand-written ES modules, still with no bundler

## Status

Accepted. Supersedes the "one file" part of
[ADR-0007](0007-no-js-build-step.md); keeps the rest.

## Context

ADR-0007 made the web UI one hand-written HTML file with no build step, for
two reasons: a JavaScript toolchain would become a build prerequisite on four
platforms, which is exactly where cross-platform support decays in this
ecosystem; and the UI was a read-only viewer, a few hundred lines of DOM
construction. It named its own trigger for revisiting: the UI becoming
interactive enough to want a component model, type checking or hot reload.

The UI is interactive now. It chats over a socket, answers the agent's
approvals and questions in place, resumes a session, stops a turn, follows a
transcript being written by a terminal, and asks for a notification when a
turn is waiting on a person. One file held six hundred lines of that and was
heading for twelve hundred, with the tab list and the renderers a thousand
lines apart — the shape the embedding test had already been written against.

## Decision

The UI is a handful of hand-written ES modules — a shell, a helper module,
one module per interactive tab and one for the read-mostly tabs — which the
browser loads as they are through `<script type="module">` and `import`.
`rookd` embeds the directory at compile time and serves each file with its
type. There is still no bundler, no `npm`, no transpiler and no dependency:
`cargo build` is the whole story on every platform, as before.

What the trigger asked for is answered without a toolchain: the component
model is a module per concern; hot reload is a page reload of files the
daemon serves from memory; type checking is not done, and the daemon's tests
do the part of it that matters — every module the page or another module
loads is embedded and served as script, and every tab has a renderer.

## Consequences

- Splitting is free and the cost of a split is nothing: a module is a file.
  The invariant is the daemon's, not the developer's — a module that is
  referenced and not embedded is caught by a test, because the SPA fallback
  would otherwise hand the page back in place of the script and the failure
  would be a blank screen with a console error nobody sees.
- Markdown in the model's answers is rendered by a small function that builds
  nodes, never by assigning HTML: the text is the model's, and so is anything
  in it. What it renders is the little of Markdown an answer uses — fences,
  paragraphs, bullets, inline code, bold — and nothing more will be added
  without a reason.
- `node --check` on the modules is a syntax check available where `node`
  happens to be installed, and nothing depends on it: CI does not have it and
  does not need it.
- A framework remains the wrong size. The UI is under a thousand lines across
  its modules and readable as such; React would be more code, not less, and
  a build step besides.
