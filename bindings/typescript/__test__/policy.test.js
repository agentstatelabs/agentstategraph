// PolicyStore napi-rs binding tests. Mirrors bindings/python/tests/test_policy.py.
// Covers POLICY_V1.md §§5, 22.2, 22.4 end-to-end through the JS wrapper:
// propose / ratify / supersede / evaluate / evaluate_change / check_tokens,
// plus active_from scheduled activation (landed in §1 of the 0.7.0 plan),
// plus the Session round-trip audit and the Task extension-field audit.
// Run with: npm test
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { AgentStateGraph, PolicyStore, TaskStore } = require('..');

function utc(date) {
  return date.toISOString().replace(/\.\d{3}Z$/, 'Z');
}

function policy(path, opts = {}) {
  const now = new Date();
  return {
    path,
    version: 1,
    situation: `situation for ${path}`,
    situation_selector: opts.situation_selector ?? { kind: 'always' },
    allow: opts.allow ?? [],
    deny: opts.deny ?? [],
    require_approval: opts.require_approval ?? [],
    triggers: opts.triggers ?? [],
    required_fields: opts.required_fields ?? [],
    severity: opts.severity ?? 'low',
    proposed_by: 'nodetest',
    proposed_at: utc(now),
    active_from: utc(opts.active_from ?? now),
  };
}

function fresh() {
  const asg = new AgentStateGraph();
  const ps = new PolicyStore(asg, '/policies', 'nodetest');
  return { asg, ps };
}

test('propose creates unratified policy', () => {
  const { ps } = fresh();
  const handle = ps.propose('main', policy('infra/k8s/pod-failing'));
  assert.equal(handle, 'infra/k8s/pod-failing@1');
  const got = ps.get('main', 'infra/k8s/pod-failing', null);
  assert.equal(got.version, 1);
  assert.equal(got.ratified_by ?? null, null);
  assert.equal(got.proposed_by, 'nodetest');
});

test('ratify promotes policy', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/restart', { allow: [{ action: 'restart_pod' }] }));
  ps.ratify('main', 'infra/restart', 'ops-lead', 'approved after review');
  const p = ps.get('main', 'infra/restart', null);
  assert.equal(p.ratified_by, 'ops-lead');
  assert.equal(p.ratification_reasoning, 'approved after review');
  assert.ok(p.ratified_at);
});

test('supersede chain + history', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/scale', { allow: [{ action: 'scale_up' }] }));
  ps.ratify('main', 'infra/scale', 'ops', 'v1');
  const v2 = policy('infra/scale', {
    allow: [{ action: 'scale_up' }, { action: 'scale_down' }],
  });
  v2.ratified_by = 'ops';
  v2.ratified_at = utc(new Date());
  const handle = ps.supersede('main', 'infra/scale', v2);
  assert.equal(handle, 'infra/scale@2');
  const history = ps.history('main', 'infra/scale');
  assert.deepEqual(history.map((p) => p.version), [1, 2]);
  assert.equal(history[history.length - 1].supersedes, 'infra/scale@1');
});

test('evaluate allow / deny / require_approval / no_match', () => {
  const { ps } = fresh();
  ps.propose(
    'main',
    policy('infra/restart', {
      allow: [{ action: 'restart_pod' }],
      situation_selector: { kind: 'eq', key: 'namespace', value: 'prod' },
    }),
  );
  ps.ratify('main', 'infra/restart', 'ops', 'ok');
  const allow = ps.evaluate('main', { namespace: 'prod' }, 'restart_pod', 'agent-1');
  assert.equal(allow.kind, 'allow');
  assert.equal(allow.matched_policy, 'infra/restart@1');

  ps.propose(
    'main',
    policy('infra/no-delete', {
      deny: [{ action: 'delete_node', condition: 'always' }],
    }),
  );
  ps.ratify('main', 'infra/no-delete', 'ops', 'ok');
  const deny = ps.evaluate('main', {}, 'delete_node', 'agent-1');
  assert.equal(deny.kind, 'deny');

  ps.propose(
    'main',
    policy('infra/risky', {
      require_approval: [
        { action: 'truncate_index', approvers: ['human'], fallback: { kind: 'block' } },
      ],
    }),
  );
  ps.ratify('main', 'infra/risky', 'ops', 'ok');
  const ra = ps.evaluate('main', {}, 'truncate_index', 'agent-1');
  assert.equal(ra.kind, 'require_approval');
  assert.deepEqual(ra.approvers, ['human']);
  assert.equal(ra.fallback.kind, 'block');

  // Fresh store → no policies ever consulted.
  const fresh2 = fresh();
  const none = fresh2.ps.evaluate('main', {}, 'whatever', 'agent-1');
  assert.equal(none.kind, 'no_policy_match');
});

test('evaluate_change with triggers + required_fields + fallback', () => {
  const { ps } = fresh();
  ps.propose(
    'main',
    policy('infra/high-cost', {
      triggers: ['reindex', 'downtime'],
      required_fields: ['estimated_downtime'],
      require_approval: [
        {
          action: 'promote',
          approvers: ['human'],
          fallback: { kind: 'lowest_risk_alternative' },
        },
      ],
      severity: 'high',
    }),
  );
  ps.ratify('main', 'infra/high-cost', 'ops', 'big changes need approval');
  const proposal = {
    action: 'promote',
    agent_id: 'agent-1',
    intent: 'merge option C',
    preferred_option: 'spec-7',
    alternatives: ['spec-1', 'spec-3'],
    tokens: ['reindex'],
    attached_fields: { estimated_downtime: '5m' },
  };
  const d = ps.evaluateChange('main', proposal);
  assert.equal(d.kind, 'require_approval');
  assert.equal(d.fallback.kind, 'lowest_risk_alternative');
});

test('evaluate_change short-circuits on missing required_fields', () => {
  const { ps } = fresh();
  ps.propose(
    'main',
    policy('infra/needs-downtime', {
      triggers: ['reindex'],
      required_fields: ['estimated_downtime'],
      require_approval: [
        { action: 'promote', approvers: ['human'], fallback: { kind: 'block' } },
      ],
    }),
  );
  ps.ratify('main', 'infra/needs-downtime', 'ops', 'ok');
  const proposal = {
    action: 'promote',
    agent_id: 'agent-1',
    intent: '',
    preferred_option: 'x',
    tokens: ['reindex'],
    attached_fields: {},
  };
  const d = ps.evaluateChange('main', proposal);
  assert.equal(d.kind, 'require_approval');
});

test('evaluate ignores not-yet-active policy (§1)', () => {
  const { ps } = fresh();
  const future = new Date(Date.now() + 60 * 60 * 1000);
  const pol = policy('infra/future', {
    allow: [{ action: 'do_it' }],
    active_from: future,
  });
  ps.propose('main', pol);
  ps.ratify('main', 'infra/future', 'ops', 'scheduled');
  const d = ps.evaluate('main', {}, 'do_it', 'agent-1');
  assert.equal(d.kind, 'no_policy_match');
  const actives = ps.active('main', null);
  assert.ok(actives.every((p) => p.path !== 'infra/future'));
});

test('check_tokens filters by trigger intersection', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/with-reindex', { triggers: ['reindex'] }));
  ps.ratify('main', 'infra/with-reindex', 'ops', 'ok');
  ps.propose('main', policy('infra/with-network', { triggers: ['network'] }));
  ps.ratify('main', 'infra/with-network', 'ops', 'ok');
  const matched = ps.checkTokens('main', ['reindex']);
  assert.deepEqual(
    matched.map((p) => p.path).sort(),
    ['infra/with-reindex'],
  );
  const both = ps.checkTokens('main', ['reindex', 'network']);
  assert.deepEqual(
    both.map((p) => p.path).sort(),
    ['infra/with-network', 'infra/with-reindex'],
  );
});

test('list and active filters', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/a'));
  ps.propose('main', policy('infra/b'));
  ps.ratify('main', 'infra/b', 'ops', 'ok');
  const listed = ps.list('main', null);
  assert.deepEqual(listed.map((p) => p.path).sort(), ['infra/a', 'infra/b']);
  const actives = ps.active('main', null);
  assert.deepEqual(actives.map((p) => p.path), ['infra/b']);
  const onlyA = ps.list('main', 'infra/a');
  assert.deepEqual(onlyA.map((p) => p.path), ['infra/a']);
});

// ---------------------------------------------------------------------------
// Session / SessionStatus round-trip audit
// ---------------------------------------------------------------------------

test('session round-trip via AgentStateGraph', () => {
  const asg = new AgentStateGraph();
  const s = asg.createSession('agent/planner', 'main', null, null, null, '/plans/');
  assert.equal(s.agent_id, 'agent/planner');
  assert.equal(s.working_branch, 'main');
  assert.equal(s.status, 'active');
  assert.equal(s.path_scope, '/plans/');
  assert.ok(s.head && typeof s.head === 'string');
  assert.equal(s.ended_at, null);

  const fetched = asg.getSession(s.id);
  assert.equal(fetched.id, s.id);
  const listed = asg.listSessions(null);
  assert.ok(listed.some((x) => x.id === s.id));

  asg.endSession(s.id, 'completed');
  const ended = asg.getSession(s.id);
  assert.equal(ended.status, 'completed');
  assert.ok(ended.ended_at);
});

// ---------------------------------------------------------------------------
// Task extension fields (payload / parent_change / on_complete) round-trip
// ---------------------------------------------------------------------------

test('task extension fields round-trip (promote_change / named / none)', () => {
  const asg = new AgentStateGraph();
  const ts = new TaskStore(asg, '/plans', 'nodetest');
  ts.createPlan('main', 'p', null);

  // PromoteChange variant.
  const t = ts.addTask(
    'main',
    'p',
    'approve high-cost change',
    'high',
    null,
    null,
    null,
    { proposal: { preferred_option: 'spec-7' } },
    'spec-7@42',
    { kind: 'promote_change' },
  );
  assert.equal(t.parent_change, 'spec-7@42');
  assert.deepEqual(t.payload, { proposal: { preferred_option: 'spec-7' } });
  assert.deepEqual(t.on_complete, { kind: 'promote_change' });

  const fetched = ts.getTask('main', 'p', t.id);
  assert.deepEqual(fetched.payload, { proposal: { preferred_option: 'spec-7' } });
  assert.equal(fetched.parent_change, 'spec-7@42');
  assert.deepEqual(fetched.on_complete, { kind: 'promote_change' });

  // Named variant.
  const t2 = ts.addTask(
    'main',
    'p',
    'custom hook',
    'low',
    null,
    null,
    null,
    null,
    null,
    { kind: 'named', name: 'notify-slack' },
  );
  assert.deepEqual(t2.on_complete, { kind: 'named', name: 'notify-slack' });

  // None variant — plain addTask with no extensions.
  const t3 = ts.addTask('main', 'p', 'plain', 'low');
  assert.equal(t3.payload ?? null, null);
  assert.equal(t3.parent_change ?? null, null);
  assert.equal(t3.on_complete ?? null, null);
});
