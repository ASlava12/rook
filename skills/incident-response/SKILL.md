---
name: incident-response
description: Use when an active outage or degradation needs triage, mitigation and a recovery timeline.
---

# Incident response

Establish current user impact, affected environment, start time and incident owner.
Record timestamps with timezone, observations and actions. Follow the project's
incident process without inventing communication authority or severity policy.

Prioritize stabilization. Check recent changes, dependency health, saturation and
failure scope using bounded read-only diagnostics. Preserve volatile evidence
when doing so will not materially delay recovery. Keep facts, hypotheses and
decisions separate.

Choose the smallest reversible mitigation within existing authorization: rollback,
feature disablement, traffic reduction or another established runbook action.
Evaluate data compatibility and failure impact first. Define the expected signal
and stop condition before executing. Avoid simultaneous speculative changes that
make their effects indistinguishable.

Verify mitigation against user-facing signals and sustained recovery, not just
a green process status. If the intervention worsens impact, stop and use the
defined recovery path. An urgent mitigation need not prove the root cause first.

Prepare concise updates with impact, actions, observed results and the next check.
Send them to other people only when that communication is authorized; otherwise
return a draft to the user.

After stability, produce a timeline and evidence-backed causal analysis. Separate
trigger, contributing conditions and detection/recovery gaps. Record follow-up
actions with verifiable completion criteria and proposed owners, without blame
or invented commitments. Do not call an incident resolved while recovery evidence
is still missing.
