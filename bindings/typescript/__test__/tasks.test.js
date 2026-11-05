// TaskStore and migrate smoke tests for the napi-rs binding.
// Run with: npm test
const test = require('node:test');
const assert = require('node:assert/strict');
const { AgentStateGraph, TaskStore, exitCodes } = require('..');

function freshStore() {
  const asg = new AgentStateGraph();
  const ts = new TaskStore(asg, '/plans', 'node-test');
  return { asg, ts };
}

test('create plan, add tasks, start, complete', () => {
  const { ts } = freshStore();
  const plan = ts.createPlan('main', 'website-v2', 'Brand pivot');
  assert.equal(plan.name, 'website-v2');
  assert.equal(plan.status, 'active');

  const t1 = ts.addTask('main', 'website-v2', 'Rewrite hero', 'high');
  assert.equal(t1.id, 't-001');
  assert.equal(t1.priority, 'high');
  assert.equal(t1.status, 'pending');

  const started = ts.startTask('main', 'website-v2', t1.id);
  assert.equal(started.status, 'in_progress');

  const done = ts.completeTask('main', 'website-v2', t1.id, 'commit', 'abc123');
  assert.equal(done.status, 'done');
  assert.equal(done.proof.kind, 'commit');
  assert.equal(done.proof.value, 'abc123');

  // Finishing the last task leaves the plan open: closing it is a
  // separate step, with a summary.
  assert.equal(ts.getPlan('main', 'website-v2').status, 'active');
  assert.throws(() => ts.closePlan('main', 'website-v2', '  '), /summary/);
  const closed = ts.closePlan('main', 'website-v2', 'hero rewritten');
  assert.equal(closed.status, 'completed');
  assert.equal(closed.summary, 'hero rewritten');
  assert.equal(closed.closed_by, 'node-test');
  assert.ok(closed.closed_at);
  assert.equal(ts.getPlan('main', 'website-v2').status, 'completed');
});

test('closePlan refuses a plan with open work', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  assert.throws(() => ts.closePlan('main', 'p', 'nothing to do'), /no tasks/);
  const t = ts.addTask('main', 'p', 'x', 'medium');
  assert.throws(() => ts.closePlan('main', 'p', 'too early'), new RegExp(t.id));
  ts.abandonTask('main', 'p', t.id, 'not needed');
  assert.equal(ts.closePlan('main', 'p', 'dropped').status, 'completed');
});

test('notes: add, set and clear', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  const brief = 'Repo: infra/gitops.\n\n1. Write the manifests.\n2. Ask the user. \u2713\n';
  const a = ts.addTask('main', 'p', 'a', 'high', null, null, null, null, null, null, brief);
  assert.equal(a.notes, brief);
  assert.equal(ts.addTask('main', 'p', 'b', 'high').notes, null);
  assert.equal(ts.getTask('main', 'p', a.id).notes, brief);

  ts.startTask('main', 'p', a.id);
  const edited = ts.setNotes('main', 'p', a.id, 'line one\nline two');
  assert.equal(edited.notes, 'line one\nline two');
  assert.equal(edited.status, 'in_progress');
  assert.throws(() => ts.setNotes('main', 'p', a.id, 'x'.repeat(16 * 1024 + 1)), /16384/);
  assert.equal(ts.setNotes('main', 'p', a.id, null).notes, null);
  ts.setNotes('main', 'p', a.id, 'again');
  assert.equal(ts.setNotes('main', 'p', a.id, '').notes, null);
});

test('blocker to nonexistent task rejected', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  assert.throws(() => ts.addTask('main', 'p', 'x', 'medium', null, ['t-999']));
});

test('next_task picks highest priority unblocked', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  ts.addTask('main', 'p', 'low', 'low');
  const high = ts.addTask('main', 'p', 'high', 'high');
  ts.addTask('main', 'p', 'crit-blocked', 'critical', null, [high.id]);

  const nxt = ts.nextTask('main', 'p');
  assert.equal(nxt.id, high.id);
});

test('assign / unassign roundtrip', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  const t = ts.addTask('main', 'p', 'x', 'medium');
  const a = ts.assignTask('main', 'p', t.id, 'codex');
  assert.equal(a.assignedTo ?? a.assigned_to, 'codex');
  const u = ts.unassignTask('main', 'p', t.id);
  assert.equal(u.assignedTo ?? u.assigned_to, null);
});

test('list_plans_by_status filters correctly', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'a', null);
  ts.createPlan('main', 'b', null);
  ts.archivePlan('main', 'b');

  const active = ts.listPlansByStatus('main', 'active');
  const archived = ts.listPlansByStatus('main', 'archived');
  assert.deepEqual(active.map((p) => p.name), ['a']);
  assert.deepEqual(archived.map((p) => p.name), ['b']);
});

test('verify_plan_with_kinds marks commit proofs verified', () => {
  const { ts } = freshStore();
  ts.createPlan('main', 'p', null);
  const t = ts.addTask('main', 'p', 'x', 'medium');
  ts.startTask('main', 'p', t.id);
  ts.completeTask('main', 'p', t.id, 'commit', 'deadbeef');

  const report = ts.verifyPlanWithKinds('main', 'p', { commit: true });
  assert.equal(report.verified_count ?? report.verifiedCount, 1);
  assert.equal(report.all_strongly_verified ?? report.allStronglyVerified, true);
});

test('migrate check + dry-run on fresh graph', () => {
  const asg = new AgentStateGraph();
  const r = asg.checkSchema();
  assert.ok(['up_to_date', 'unversioned'].includes(r.status));
  const report = asg.migrate('main', null, 'dry-run');
  assert.equal(report.mode, 'dry-run');
  assert.ok(Array.isArray(report.steps));
});

test('exit_codes exposes sysexits constants', () => {
  const codes = exitCodes();
  for (const k of ['OK', 'DOWNGRADE_REFUSED', 'CORRUPT_META', 'MIGRATION_FAILED', 'UPGRADE_REQUIRED']) {
    assert.equal(typeof codes[k], 'number');
  }
});
