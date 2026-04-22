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

// ---------------------------------------------------------------------------
// 0.7.5 §5b: signing + multi-tenant + external-evaluator field round-trips
// Mirrors bindings/python/tests/test_policy.py §5a.
// ---------------------------------------------------------------------------

test('policy signature field round-trips (§2a/b tagged union)', () => {
  const { ps } = fresh();
  const pol = policy('infra/signed', { allow: [{ action: 'touch' }] });
  pol.signature = {
    algorithm: 'ed25519',
    signer_key_id: 'ops-root-2026',
    signature_hex: 'aa'.repeat(64),
  };
  ps.propose('main', pol);
  const fetched = ps.get('main', 'infra/signed', null);
  assert.deepEqual(fetched.signature, {
    algorithm: 'ed25519',
    signer_key_id: 'ops-root-2026',
    signature_hex: 'aa'.repeat(64),
  });
});

test('policy tenant_id field round-trips (§3a Option<String>)', () => {
  const { ps } = fresh();
  const pol = policy('infra/scoped', { allow: [{ action: 'touch' }] });
  pol.tenant_id = 'tenant-acme';
  ps.propose('main', pol);
  const fetched = ps.get('main', 'infra/scoped', null);
  assert.equal(fetched.tenant_id, 'tenant-acme');

  // Global (tenant_id omitted) — serde skip_serializing_if=Option::is_none.
  const pol2 = policy('infra/global', { allow: [{ action: 'touch' }] });
  ps.propose('main', pol2);
  const fetched2 = ps.get('main', 'infra/global', null);
  assert.equal(fetched2.tenant_id ?? null, null);
});

test('policy external_evaluator field round-trips across all 3x3 variants (§4a)', () => {
  const { ps } = fresh();
  const matrix = [
    ['rego', 'a', { kind: 'inline', body: 'package asg\nallow { true }' }],
    ['cedar', 'b', { kind: 'file_path', path: '/etc/asg/policy.cedar' }],
    ['wasm', 'c', { kind: 'commit_ref', path: '/evaluators/x.wasm' }],
    ['rego', 'd', { kind: 'file_path', path: '/etc/asg/policy.rego' }],
    ['cedar', 'e', { kind: 'inline', body: 'permit(principal, action, resource);' }],
    ['wasm', 'f', { kind: 'inline', body: 'AGFzbQEAAAA=' }],
    ['rego', 'g', { kind: 'commit_ref', path: '/evaluators/rbac.rego' }],
    ['cedar', 'h', { kind: 'commit_ref', path: '/evaluators/corp.cedar' }],
    ['wasm', 'i', { kind: 'file_path', path: '/etc/asg/runner.wasm' }],
  ];
  for (const [kind, suffix, source] of matrix) {
    const pol = policy(`infra/ext-${suffix}`);
    pol.external_evaluator = { kind, source };
    ps.propose('main', pol);
    const fetched = ps.get('main', `infra/ext-${suffix}`, null);
    assert.deepEqual(fetched.external_evaluator, { kind, source });
  }
});

test('evaluate with tenantFilter restricts scoped policies (§3b)', () => {
  const { ps } = fresh();
  const acme = policy('infra/acme-only', {
    allow: [{ action: 'deploy' }],
    situation_selector: { kind: 'always' },
  });
  acme.tenant_id = 'tenant-acme';
  ps.propose('main', acme);
  ps.ratify('main', 'infra/acme-only', 'ops', 'ok');

  const other = policy('infra/other-only', {
    allow: [{ action: 'deploy' }],
    situation_selector: { kind: 'always' },
  });
  other.tenant_id = 'tenant-other';
  ps.propose('main', other);
  ps.ratify('main', 'infra/other-only', 'ops', 'ok');

  // acme tenant sees only the acme policy.
  const d = ps.evaluate('main', {}, 'deploy', 'agent-1', 'tenant-acme');
  assert.equal(d.kind, 'allow');
  assert.equal(d.matched_policy, 'infra/acme-only@1');

  // Unknown tenant → both scoped policies filtered out.
  const d2 = ps.evaluate('main', {}, 'deploy', 'agent-1', 'tenant-unknown');
  assert.equal(d2.kind, 'no_policy_match');

  // active() with tenantFilter agrees.
  const acmeActives = ps.active('main', null, 'tenant-acme');
  assert.deepEqual(acmeActives.map((p) => p.path), ['infra/acme-only']);
});

test('evaluate with tenantFilter: global (tenant_id=null) policy applies under every tenant', () => {
  const { ps } = fresh();
  const globally = policy('infra/global-allow', {
    allow: [{ action: 'noop' }],
    situation_selector: { kind: 'always' },
  });
  ps.propose('main', globally);
  ps.ratify('main', 'infra/global-allow', 'ops', 'ok');

  for (const tf of ['tenant-a', 'tenant-b', null]) {
    const d = ps.evaluate('main', {}, 'noop', 'agent-1', tf);
    assert.equal(d.kind, 'allow', `tenantFilter=${JSON.stringify(tf)}`);
    assert.equal(d.matched_policy, 'infra/global-allow@1');
  }
});

test('evaluateChange accepts tenantFilter (§3b)', () => {
  const { ps } = fresh();
  const pol = policy('infra/tenant-change', {
    triggers: ['reindex'],
    require_approval: [
      { action: 'promote', approvers: ['human'], fallback: { kind: 'block' } },
    ],
  });
  pol.tenant_id = 'tenant-a';
  ps.propose('main', pol);
  ps.ratify('main', 'infra/tenant-change', 'ops', 'ok');

  const proposal = {
    action: 'promote',
    agent_id: 'agent-1',
    intent: '',
    preferred_option: 'x',
    tokens: ['reindex'],
    attached_fields: {},
  };
  // Matching tenant → policy consulted.
  const d = ps.evaluateChange('main', proposal, 'tenant-a');
  assert.equal(d.kind, 'require_approval');
  // Different tenant → filtered out, no match.
  const d2 = ps.evaluateChange('main', proposal, 'tenant-b');
  assert.equal(d2.kind, 'no_policy_match');
});

test('Session.scope_tenant field surfaces in JS dict (§3a)', () => {
  const asg = new AgentStateGraph();
  const s = asg.createSession('agent/a', 'main');
  assert.ok('scope_tenant' in s);
  assert.equal(s.scope_tenant ?? null, null);

  const fetched = asg.getSession(s.id);
  assert.ok('scope_tenant' in fetched);
  assert.equal(fetched.scope_tenant ?? null, null);

  const listed = asg.listSessions(null);
  assert.ok(listed.every((x) => 'scope_tenant' in x));
});

// ---------------------------------------------------------------------------
// §5b: real sign/verify wiring via agentstategraph-policy-sign.
// setExternalEvaluator remains a stub per plan §4c (FFI dispatcher is
// post-production per docs/POLICY_GUIDE.md).
// ---------------------------------------------------------------------------

const { createPrivateKey, createPublicKey } = require('node:crypto');

// Fixed 32-byte Ed25519 seed — deterministic for test reproducibility.
// Hex form of [1u8; 32].
const TEST_SEED_HEX = '01'.repeat(32);

// Derive the matching public key from the seed via Node's crypto module.
// ed25519 SubjectPublicKeyInfo DER prefix is 12 bytes; the last 32 bytes
// are the raw public key. Likewise for PKCS#8 the last 32 bytes of the
// DER are the private seed — but we already have the seed, so here we
// only derive the public side.
function publicHexFromSeedHex(seedHex) {
  const seed = Buffer.from(seedHex, 'hex');
  // Build a PKCS#8 DER envelope for an Ed25519 private key:
  //   30 2e 02 01 00 30 05 06 03 2b 65 70 04 22 04 20 <seed...>
  const pkcs8Prefix = Buffer.from(
    '302e020100300506032b657004220420',
    'hex',
  );
  const pkcs8 = Buffer.concat([pkcs8Prefix, seed]);
  const privKey = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
  const pubKey = createPublicKey(privKey);
  const spki = pubKey.export({ format: 'der', type: 'spki' });
  // SPKI envelope is 12 bytes, followed by the 32-byte raw public key.
  return Buffer.from(spki).subarray(-32).toString('hex');
}

test('PolicyStore.sign produces a valid signature on a proposed policy (§5b)', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/to-sign'));

  const result = ps.sign('main', 'infra/to-sign', 'test-key-1', TEST_SEED_HEX);
  assert.equal(result.algorithm, 'ed25519');
  assert.equal(result.signer_key_id, 'test-key-1');
  assert.equal(typeof result.signature_hex, 'string');
  assert.equal(result.signature_hex.length, 128); // 64 bytes hex

  const fetched = ps.get('main', 'infra/to-sign', null);
  assert.ok(fetched.signature);
  assert.equal(fetched.signature.algorithm, 'ed25519');
  assert.equal(fetched.signature.signer_key_id, 'test-key-1');
  assert.equal(fetched.signature.signature_hex, result.signature_hex);
});

test('PolicyStore.sign + verify round-trips (§5b)', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/roundtrip'));

  ps.sign('main', 'infra/roundtrip', 'test-key-1', TEST_SEED_HEX);
  const publicHex = publicHexFromSeedHex(TEST_SEED_HEX);

  const v = ps.verify('main', 'infra/roundtrip', publicHex);
  assert.equal(v.valid, true);
  assert.equal(v.algorithm, 'ed25519');
  assert.equal(v.signer_key_id, 'test-key-1');
});

test('PolicyStore.verify on unsigned policy reports unsigned (§5b)', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/unsigned'));
  const publicHex = publicHexFromSeedHex(TEST_SEED_HEX);
  const v = ps.verify('main', 'infra/unsigned', publicHex);
  assert.equal(v.valid, false);
  assert.equal(v.reason, 'unsigned');
});

test('PolicyStore.setExternalEvaluator still returns stub envelope (plan §4c)', () => {
  const { ps } = fresh();
  ps.propose('main', policy('infra/to-sign'));
  const ext = ps.setExternalEvaluator('main', 'infra/to-sign', {
    kind: 'rego',
    source: { kind: 'inline', body: 'package x' },
  });
  assert.equal(ext.error, 'not yet wired');
});
