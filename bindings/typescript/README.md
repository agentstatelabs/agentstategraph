# agentstategraph (Node.js / TypeScript)

Node.js bindings for [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph)
via [napi-rs](https://napi.rs/).

## Install

```bash
npm install
npm run build
```

## Versioned state

```js
const { AgentStateGraph } = require('agentstategraph');

const asg = new AgentStateGraph();              // in-memory
// const asg = new AgentStateGraph('./state.db'); // SQLite

asg.set('/name', 'my-cluster', 'init', 'main', 'Checkpoint');
console.log(asg.get('/name'));
```

## TaskStore

```js
const { AgentStateGraph, TaskStore } = require('agentstategraph');

const asg = new AgentStateGraph();
const tasks = new TaskStore(asg, '/plans', 'claude-code');

tasks.createPlan('main', 'website-v2', 'Brand pivot');
const t = tasks.addTask('main', 'website-v2', 'Rewrite hero', 'high');
tasks.startTask('main', 'website-v2', t.id);
tasks.completeTask('main', 'website-v2', t.id, 'commit', 'abc123');

const next = tasks.nextTask('main', 'website-v2');
const mine = tasks.nextTaskFor('main', 'website-v2', 'claude-code', true);

// Verify `done` tasks. verifyByKind maps proof kinds -> bool; true kinds
// are reported as Verified, others as Unverifiable.
const report = tasks.verifyPlanWithKinds('main', 'website-v2', {
  commit: true,
  file: true,
});
console.log(report.summary);
```

Enums (all passed as strings):

- Priority: `low`, `medium`, `high`, `critical`
- Task status: `pending`, `in_progress`, `done`, `abandoned`
- Plan status: `active`, `completed`, `archived`
- Proof kind: `commit`, `file`, `test`, `text`

## Schema migrations

```js
const result = asg.checkSchema();
if (result.status === 'upgrade_available') {
  const report = asg.migrate('main', null, 'apply');
  console.log(`migrated ${result.from} -> ${report.final_version}`);
}
```

## Tests

```bash
npm run build
npm test
```
