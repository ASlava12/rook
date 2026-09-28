---
name: ui-ux-design
description: Use when designing or changing a user flow, screen or responsive interface.
---

# UI/UX design

Identify the user's task, primary action, information needed to decide and the
device/input contexts that matter. Inspect existing screens and design tokens
before creating a new component language.

Sketch the interaction and state transitions before polishing visuals. Include
loading, empty, populated, validation, failure, success and disabled states where
they occur. Preserve user input on recoverable failures and make recovery apparent.

Use the existing design system's spacing, typography, color and components.
Create hierarchy through grouping and clear labels. Avoid exposing implementation
details unless they help a user make a decision. Do not invent product claims,
customer quotes or functional controls that do nothing.

Design for narrow and wide layouts, long content, localization and zoom. Prefer
content-driven breakpoints over assumptions about a few named devices. Keep
keyboard navigation, semantic controls, visible focus, accessible names and
non-color status cues part of the implementation.

Verify the main task in the rendered interface, not just the source: exercise
actions and failure recovery, inspect representative viewport sizes and check
focus movement. Use available screenshots or browser tools when helpful; state
which visual or assistive-technology checks could not be performed.

Deliver the working flow, its important states and verification evidence. For a
design-only request, provide the interaction decisions and handoff details without
claiming implementation or usability testing that did not happen.
