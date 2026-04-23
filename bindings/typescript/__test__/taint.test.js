// 0.7.75 §9b — TypeScript/napi taint / quarantine / watch binding tests.
// Mirrors the Python taint tests: round-trip, block-effect rejection,
// review-effect confidence gating, quarantine gating, watch
// auto-escalation via set_json, and check_taint aggregation.
//
// Run with: npm test
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');

const { AgentStateGraph } = require('..');

function freshRepo() {
  const asg = new AgentStateGraph();
  // Seed main so every ref-scoped write has a parent to commit onto.
  asg.set('/seed', 'ok', 'seed main');
  return asg;
}

test('taint round-trip: apply, list, untaint', () => {
  const asg = freshRepo();
  const id = asg.taint('main', '/cluster', {
    name: 't1',
    effect: 'warn',
    reason: 'degraded',
    agent_id: 'ops',
  });
  assert.equal(typeof id, 'string');
  assert.ok(id.length > 0);

  const listed = asg.listTaints(null, 'taint', false);
  assert.equal(listed.length, 1);
  assert.equal(listed[0].id, id);
  assert.equal(listed[0].name, 't1');
  assert.equal(listed[0].effect, 'warn');
  assert.ok(listed[0].commit_id.length > 0, 'commit_id should be patched post-commit');

  asg.untaint('main', '/cluster', 't1', {
    reason: 'resolved',
    proof: 'commit-xyz',
    agent_id: 'ops',
  });
  const active = asg.listTaints(null, 'taint', false);
  assert.equal(active.length, 0);

  // With include_resolved=true the resolved row resurfaces.
  const all = asg.listTaints(null, 'taint', true);
  assert.equal(all.length, 1);
  assert.ok(all[0].resolved_at);
});

test('block-effect rejects set on tainted path', () => {
  const asg = freshRepo();
  asg.taint('main', '/cluster', {
    name: 'down',
    effect: 'block',
    reason: 'offline',
    agent_id: 'ops',
  });
  // Descendant write should be blocked by the propagating taint.
  assert.throws(
    () => asg.set('/cluster/nodes/a', 'hi', 'try write', 'main', 'Refine', 'agent-1'),
    /Blocked|blocked|taint/i,
  );
});

test('review-effect rejects low confidence, accepts high', () => {
  const asg = freshRepo();
  asg.taint('main', '/cluster', {
    name: 'rev',
    effect: 'review',
    reason: 'needs approval',
    agent_id: 'ops',
  });

  // low confidence (0.5) → rejected
  assert.throws(
    () =>
      asg.set(
        '/cluster/x',
        'v',
        'low-confidence write',
        'main',
        'Refine',
        'agent-1',
        null,
        0.5,
      ),
    /confidence|Insufficient/i,
  );

  // high confidence (0.95) → accepted
  const commitId = asg.set(
    '/cluster/y',
    'v',
    'high-confidence write',
    'main',
    'Refine',
    'agent-1',
    null,
    0.95,
  );
  assert.equal(typeof commitId, 'string');
});

test('quarantine gates writes to authorized agents only', () => {
  const asg = freshRepo();
  asg.quarantine('main', '/secrets', {
    name: 'sec',
    reason: 'audit',
    severity: 'high',
    authorized_agents: ['agent/security'],
    agent_id: 'agent/security',
  });

  // Unauthorized agent is rejected.
  assert.throws(
    () =>
      asg.set('/secrets/x', 'v', 'unauthorized', 'main', 'Refine', 'agent-1'),
    /NotAuthorized|not authorized|quarantine/i,
  );

  // Authorized agent passes.
  const id = asg.set(
    '/secrets/y',
    'v',
    'authorized',
    'main',
    'Refine',
    'agent/security',
  );
  assert.equal(typeof id, 'string');

  // Release the quarantine — unauthorized agent can now write.
  asg.unquarantine('main', '/secrets', 'sec', {
    reason: 'cleared',
    agent_id: 'agent/security',
  });
  asg.set('/secrets/z', 'v', 'post-release', 'main', 'Refine', 'agent-1');
});

test('watch auto-escalation fires when set_json crosses threshold', () => {
  const asg = freshRepo();
  asg.watch('main', '/metrics/cpu', {
    name: 'hot',
    reason: 'saturation watch',
    metric: 'pct',
    threshold: 80.0,
    direction: 'above',
    agent_id: 'ops',
  });

  // Below threshold → no auto-taint.
  asg.setJson('/metrics/cpu', { pct: 50 }, 'cpu normal');
  const before = asg.listTaints('/metrics/cpu', 'taint', false);
  assert.equal(before.length, 0, 'no auto-taint expected below threshold');

  // Above threshold → auto-taint created with canonical name.
  asg.setJson('/metrics/cpu', { pct: 95 }, 'cpu hot');
  const after = asg.listTaints('/metrics/cpu', 'taint', false);
  assert.equal(after.length, 1, 'exactly one auto-taint expected');
  assert.equal(after[0].name, 'watch-threshold-exceeded-hot');
  assert.equal(after[0].effect, 'warn');
  // Metadata carries the auto_escalated flag.
  assert.equal(after[0].metadata.auto_escalated, true);

  // Idempotent: second crossing write does not duplicate.
  asg.setJson('/metrics/cpu', { pct: 97 }, 'cpu still hot');
  const again = asg.listTaints('/metrics/cpu', 'taint', false);
  assert.equal(again.length, 1, 'auto-taint should be idempotent per watch');
});

test('check_taint aggregates taints, quarantines, watches', () => {
  const asg = freshRepo();

  // Warn-effect taint on /svc.
  asg.taint('main', '/svc', {
    name: 'warn-only',
    effect: 'warn',
    reason: 'noisy',
    agent_id: 'ops',
  });
  // Quarantine on /svc restricts to agent/security.
  asg.quarantine('main', '/svc', {
    name: 'q1',
    reason: 'pen-test',
    authorized_agents: ['agent/security'],
    agent_id: 'agent/security',
  });
  // Advisory watch on /svc.
  asg.watch('main', '/svc', {
    name: 'w1',
    reason: 'observe',
    metric: 'rps',
    threshold: 1000,
    agent_id: 'ops',
  });

  // Unauthorized agent cannot write — quarantine blocks.
  const unauth = asg.checkTaint('/svc/child', 'agent-1', 1.0);
  assert.equal(unauth.tainted, true);
  assert.equal(unauth.quarantined, true);
  assert.equal(unauth.watched, true);
  assert.equal(unauth.can_write, false);
  assert.equal(unauth.taints.length, 1);
  assert.equal(unauth.quarantines.length, 1);
  assert.equal(unauth.watches.length, 1);
  assert.deepEqual(unauth.authorized_agents, ['agent/security']);

  // Authorized agent can write (warn-effect taint does not block).
  const auth = asg.checkTaint('/svc/child', 'agent/security', 1.0);
  assert.equal(auth.can_write, true);
  assert.equal(auth.tainted, true);
});

// Accept JSON-string params as well (pattern parity with the FFI layer).
test('taint accepts JSON string params', () => {
  const asg = freshRepo();
  const id = asg.taint(
    'main',
    '/x',
    JSON.stringify({
      name: 'from-string',
      effect: 'warn',
      reason: 'y',
      agent_id: 'ops',
    }),
  );
  assert.ok(id.length > 0);
  const listed = asg.listTaints('/x', 'taint', false);
  assert.equal(listed.length, 1);
  assert.equal(listed[0].name, 'from-string');
});
