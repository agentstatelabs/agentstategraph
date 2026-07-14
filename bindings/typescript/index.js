// Platform-detecting loader for the prebuilt native addon.
//
// The release CI (.github/workflows/npm.yml) builds one
// `agentstategraph.<platform-tag>.node` per target and ships them all in
// the published package. At runtime we pick the one matching the host.
const { platform, arch } = process

// napi-rs platform tags, keyed by `${process.platform}-${process.arch}`.
const TAGS = {
  'darwin-arm64': 'darwin-arm64',
  'darwin-x64': 'darwin-x64',
  'linux-x64': 'linux-x64-gnu',
  'linux-arm64': 'linux-arm64-gnu',
  'win32-x64': 'win32-x64-msvc',
  'win32-arm64': 'win32-arm64-msvc',
}

const tag = TAGS[`${platform}-${arch}`]
if (!tag) {
  throw new Error(
    `agentstategraph: unsupported platform ${platform}-${arch}. ` +
      `Supported: ${Object.keys(TAGS).join(', ')}. ` +
      `Build from source with \`napi build --release\` in bindings/typescript.`,
  )
}

let native
try {
  native = require(`./agentstategraph.${tag}.node`)
} catch (err) {
  throw new Error(
    `agentstategraph: failed to load native addon for ${platform}-${arch} ` +
      `(agentstategraph.${tag}.node). ${err.message}`,
  )
}

const { AgentStateGraph, TaskStore, PolicyStore, exitCodes } = native

module.exports = { AgentStateGraph, TaskStore, PolicyStore, exitCodes }
