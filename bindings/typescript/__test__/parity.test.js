// Cross-binding policy parity runner — TypeScript (node:test) side.
//
// §7 of the 0.7.0-beta.1 plan. Loads the shared fixture at
// spec/policy_parity_fixture.json, seeds the scenario via the napi-rs
// PolicyStore binding, and asserts the same decision.kind +
// matched_policy prefix as every other binding's runner.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const { AgentStateGraph, PolicyStore } = require('..');

function loadFixture() {
  // __dirname = bindings/typescript/__test__
  const fixturePath = path.resolve(__dirname, '..', '..', '..', 'spec', 'policy_parity_fixture.json');
  return JSON.parse(fs.readFileSync(fixturePath, 'utf8'));
}

test('cross-binding policy parity — TypeScript runner', () => {
  const fixture = loadFixture();
  const prefix = fixture.prefix ?? '/policies';
  const agentId = fixture.agent_id ?? 'parity-runner';
  const ref = fixture.ref ?? 'main';

  const asg = new AgentStateGraph();
  const ps = new PolicyStore(asg, prefix, agentId);

  for (const pol of fixture.policies) {
    ps.propose(ref, pol);
  }
  for (const r of fixture.ratify) {
    ps.ratify(ref, r.path, r.ratifier, r.reasoning);
  }

  for (const entry of fixture.change_proposals) {
    const label = entry.label ?? '<unlabelled>';
    const d = ps.evaluateChange(ref, entry.proposal);
    assert.equal(
      d.kind,
      entry.expected_decision_kind,
      `${label}: got ${JSON.stringify(d)}`,
    );
    if (entry.expected_matched_policy_prefix) {
      const matched = d.matched_policy ?? '';
      assert.ok(
        matched.startsWith(entry.expected_matched_policy_prefix),
        `${label}: matched_policy ${JSON.stringify(matched)} should start with ${JSON.stringify(entry.expected_matched_policy_prefix)}`,
      );
    }
  }

  for (const entry of fixture.evaluate) {
    const label = entry.label ?? '<unlabelled>';
    const d = ps.evaluate(ref, entry.situation, entry.action, entry.agent_id);
    assert.equal(
      d.kind,
      entry.expected_decision_kind,
      `${label}: got ${JSON.stringify(d)}`,
    );
  }
});
