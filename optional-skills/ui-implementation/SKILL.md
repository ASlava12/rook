---
name: ui-implementation
description: Use when implementing working graphical screens, components or responsive web interfaces.
version: 1.0.0
license: MIT
---

# Implement graphical user interfaces

Identify the user journey, target platform, actual data contract and requested
deliverable. Inspect the existing routes, components, state management, styles,
design tokens and build commands. Continue the project's framework and design
system; adding a screen does not imply replacing its stack. For a new app, choose
the smallest suitable stack and record the choice. For native desktop/mobile,
use that toolkit's layout, accessibility and lifecycle APIs rather than forcing
web DOM assumptions onto it.

Turn the journey into a component and state model before detailed styling.
Separate server data, local editable state and derived values. Represent initial,
loading, empty, populated, validation, failure and success states where needed.
Build a working vertical slice from input through the real handler to observable
result. A clickable control needs an implemented action; label fixture data and
unavailable integrations instead of manufacturing successful requests.

Make the visual hierarchy deliberate: a clear primary action, readable type,
consistent spacing and aligned groups. Reuse existing tokens and components. For
new visual work, choose a coherent type/space/color system suited to the content;
avoid filling the screen with decorative cards or fake product claims. Preserve
provided branding and assets. Add animation only where it explains state or aids
interaction, respecting reduced-motion settings.

Use semantic controls and accessible names. Associate fields with labels and
errors; preserve entered values on recoverable failures. Support keyboard paths,
visible focus and predictable focus after dialogs, navigation and validation.
Prefer native semantics before adding ARIA. A visually hidden or disabled element
must not remain an unintended focus target. Status needs text or shape in addition
to color. Custom composite controls need the matching keyboard interaction model,
not an arbitrary `tabindex` on every child.

Use content-driven layout with intrinsic sizing, sensible wrapping and explicit
overflow behavior. Check narrow/wide windows, zoom, long/localized text and large
data. Avoid global fixed heights and absolute positions that only fit a screenshot.
Keep dialog, popover and scroll containers usable with an on-screen keyboard where
relevant. Preserve reading order when the visual layout changes.

Connect asynchronous actions with explicit pending/error handling, cancellation
or stale-result protection when needed. Prevent duplicate submission while its
outcome is unknown; reconcile with actual state before retrying a write. Keep
credentials and privileged calls in the appropriate backend. Treat supplied text
as data rather than HTML. Keep rendering work and retained lists bounded; measure
a real interaction before adding memoization or a new state library.

Run the repository's relevant build, type and component checks. Exercise the main
journey and recovery in a real renderer with controlled inputs. For web work use
Rook's `browser` tool when offered, or the project's existing browser tests; source
inspection cannot establish that a click works. Native screenshots use a fixed
viewport, so use other available equipment for responsive claims. Compare the
rendered output to the requested design and inspect clipping, focus and overlays.
Do not claim console, mobile or assistive-technology checks that were unavailable.

Deliver the runnable interface, changed behavior and actual validation. Separate
fixture-backed behavior, verified integrations and remaining environmental limits.
Use `ui-ux-design` for a design-only task and `browser-qa` for a dedicated web audit
when those skills are available; neither is required to load this workflow.

Keyboard reference: [WAI-ARIA Authoring Practices](https://www.w3.org/WAI/ARIA/apg/practices/keyboard-interface/).
