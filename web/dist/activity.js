// Shared by live chat, diagnostics and family inspection; no owner inference.
export function activityPhase(activity, now = Date.now()) {
  if (!activity) return undefined;
  if (activity.provider_wait) {
    const wait = activity.provider_wait;
    const remaining = Number(wait.until_ms) - now;
    const timing = remaining > 0 ? `in ${Math.ceil(remaining / 1000)}s` : `overdue by ${Math.floor(-remaining / 1000)}s`;
    const refusal = wait.status == null ? 'temporary provider failure' : `HTTP ${wait.status}`;
    return `${activity.owner_live === true ? '' : 'saved '}provider retry ${timing} after ${refusal} (attempt ${wait.next_attempt})`;
  }
  if (activity.retry_at != null && activity.stage === 'queued') {
    return `waiting to retry at ${new Date(Number(activity.retry_at) * 1000).toLocaleString()}`;
  }
  return activity.stage;
}
