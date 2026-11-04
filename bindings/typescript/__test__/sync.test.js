// Named sync remotes from TypeScript (DESIGN.md Plan G t-008).
const test = require('node:test')
const assert = require('node:assert/strict')
const { AgentStateGraph } = require('..')

test('remotes are kept, listed and removed', () => {
  const sg = new AgentStateGraph()
  sg.remoteAdd('hub', 'https://hub.example.com', undefined, 'ASG_HUB_TOKEN')
  const remotes = sg.remotes()
  assert.strictEqual(remotes[0].name, 'hub')
  assert.strictEqual(remotes[0].store, 'default')
  assert.strictEqual(remotes[0].token_env, 'ASG_HUB_TOKEN')
  assert.strictEqual(sg.remoteRemove('hub'), true)
  assert.strictEqual(sg.remoteRemove('hub'), false)
})

test('a bad remote or an unknown one throws', () => {
  const sg = new AgentStateGraph()
  assert.throws(() => sg.remoteAdd('hub', 'ftp://hub.example.com'))
  assert.throws(() => sg.fetch('nowhere'), /no remote is named nowhere/)
})
