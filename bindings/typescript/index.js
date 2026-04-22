// napi-rs 2 does not auto-generate index.js for this crate, so we
// hand-export the symbols that the Rust side marks `#[napi]`.
const native = require('./agentstategraph.darwin-arm64.node')

const { AgentStateGraph, TaskStore, PolicyStore, exitCodes } = native

module.exports = { AgentStateGraph, TaskStore, PolicyStore, exitCodes }
