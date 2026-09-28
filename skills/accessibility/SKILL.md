---
name: accessibility
description: Use when auditing or fixing keyboard, screen-reader, visual or cognitive access to an interface.
---

# Accessibility

Identify the user flow, supported input modes and the project's accessibility
target. Consult the applicable current standard when making a WCAG conformance
claim; do not infer conformance from one automated score.

Inspect the rendered accessibility tree and interaction, not just markup:

- Use semantic controls with accessible names, roles, states and relationships.
  Prefer native elements over recreating behavior with ARIA.
- Complete the main flow by keyboard with visible focus, logical order and no
  trap. Manage focus after navigation, dialogs, errors and dynamic removal.
- Label fields and associate errors/instructions. Announce relevant dynamic
  changes without making every update interrupt the user.
- Check contrast, zoom, reflow and information conveyed by color, motion or sound.
  Respect reduced-motion preferences where animation is present.
- Provide useful text alternatives and media alternatives when content requires
  them. Do not repeat decorative imagery as distracting screen-reader output.

Use automated checks for detectable violations and manual keyboard/assistive
technology checks for behavior. Include loading, validation and failure states,
not just a static happy-path screenshot.

Prioritize findings by the task a user cannot complete and the evidence for the
barrier. Verify fixes in the rendered flow. Report tested pages, tools/input modes,
remaining barriers and unperformed checks. A partial audit should remain explicitly
bounded and must not be described as certification.
