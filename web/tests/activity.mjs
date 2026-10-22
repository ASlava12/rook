import test from 'node:test';
import assert from 'node:assert/strict';
import { activityPhase } from '../dist/activity.js';

test('provider waits remain distinct from scheduler waits, overdue requests and historical observations', () => {
  const state = { stage: 'model', owner_live: true,
    provider_wait: { until_ms: 45000, next_attempt: 2, status: 429 } };
  assert.equal(activityPhase(state, 0), 'provider retry in 45s after HTTP 429 (attempt 2)');
  assert.equal(activityPhase(state, 50000), 'provider retry overdue by 5s after HTTP 429 (attempt 2)');
  assert.match(activityPhase({...state, owner_live: null}, 0), /^saved provider retry/);
  assert.match(activityPhase({...state, owner_live: false}, 0), /^saved provider retry/);
  assert.match(activityPhase({...state, provider_wait: {...state.provider_wait, status: null}}, 0), /temporary provider failure/);
  assert.equal(activityPhase({stage: 'model'}), 'model');
  assert.match(activityPhase({stage: 'queued', retry_at: 45}), /^waiting to retry at/);
});
