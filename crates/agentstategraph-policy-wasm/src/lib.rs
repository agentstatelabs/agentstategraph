//! WASM host runner for AgentStateGraph policies (0.7.5 §4b).
//!
//! This crate is a host-side [`ExternalEvaluator`] that loads a WASM
//! policy module via `wasmtime` and invokes it through a small,
//! documented ABI. It is NOT compiled to WASM itself — it runs a
//! WASM module inside the host process.
//!
//! # ABI
//!
//! The module MUST export a linear memory named `memory` plus three
//! functions:
//!
//! ```text
//! (func (export "asg_alloc")
//!   (param $size i32) (result i32))
//!
//! (func (export "asg_free")
//!   (param $ptr i32) (param $size i32))
//!
//! (func (export "asg_evaluate")
//!   (param $input_ptr i32) (param $input_len i32)
//!   (result i64))     ;; high 32 bits = output_ptr, low 32 bits = output_len
//! ```
//!
//! The host:
//!
//! 1. Instantiates the module (no imports required).
//! 2. Calls `asg_alloc(input_len)` and writes the input JSON bytes
//!    into linear memory at the returned pointer.
//! 3. Calls `asg_evaluate(input_ptr, input_len)`. The return value
//!    packs the output pointer in the high 32 bits and the output
//!    length in the low 32 bits of the `i64`.
//! 4. Reads `output_len` bytes from `output_ptr` in linear memory and
//!    parses them as JSON of [`Decision`].
//! 5. Calls `asg_free` on both the input and the output buffers.
//!
//! # Input JSON
//!
//! ```json
//! {
//!   "situation": { "<fact-key>": "<fact-value>", ... },
//!   "action": "<action-name>",
//!   "agent_id": "<agent-id>"
//! }
//! ```
//!
//! # Output JSON
//!
//! Serde tagged union matching [`Decision`] — see the
//! `agentstategraph-policy` crate.
//!
//! # Source variants
//!
//! - [`EvaluatorSource::Inline`] — `body` is interpreted as the raw
//!   bytes of the WASM module (as a string, so binary modules should
//!   be embedded via `FilePath` instead in practice; `Inline` is
//!   useful mainly for WAT fixtures).
//! - [`EvaluatorSource::FilePath`] — read the module bytes from disk.
//! - [`EvaluatorSource::CommitRef`] — not supported by this runner;
//!   returns [`ExternalError::SourceResolution`]. The dispatcher is
//!   responsible for resolving commit refs in a future milestone.

use std::collections::HashMap;

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};

/// Host-side WASM runner.
///
/// Shares a single [`wasmtime::Engine`] across evaluations so module
/// compilation caches are reused. Each call instantiates a fresh
/// [`wasmtime::Store`] — modules are expected to be pure and
/// stateless across invocations.
pub struct WasmEvaluator {
    engine: wasmtime::Engine,
}

impl WasmEvaluator {
    /// Construct a runner with the default wasmtime engine config.
    pub fn new() -> Self {
        Self {
            engine: wasmtime::Engine::default(),
        }
    }
}

impl Default for WasmEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a [`Situation`] (transparent over `HashMap<String,String>`)
/// into a plain `HashMap` for JSON serialization.
fn situation_to_map(s: &Situation) -> HashMap<String, String> {
    s.0.clone()
}

impl ExternalEvaluator for WasmEvaluator {
    fn kind(&self) -> &'static str {
        "wasm"
    }

    fn evaluate(
        &self,
        source: &EvaluatorSource,
        situation: &Situation,
        action: &str,
        agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        // 1. Resolve source -> module bytes.
        let bytes: Vec<u8> = match source {
            EvaluatorSource::Inline { body } => body.as_bytes().to_vec(),
            EvaluatorSource::FilePath { path } => {
                std::fs::read(path).map_err(|e| ExternalError::SourceResolution(e.to_string()))?
            }
            EvaluatorSource::CommitRef { .. } => {
                return Err(ExternalError::SourceResolution(
                    "commit_ref not supported by WasmEvaluator".into(),
                ));
            }
        };

        // 2. Compile + instantiate.
        let mut store = wasmtime::Store::new(&self.engine, ());
        let module = wasmtime::Module::new(&self.engine, &bytes)
            .map_err(|e| ExternalError::Execution(format!("compile: {e}")))?;
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .map_err(|e| ExternalError::Execution(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| ExternalError::Execution("module missing `memory` export".into()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "asg_alloc")
            .map_err(|e| ExternalError::Execution(format!("asg_alloc: {e}")))?;
        let free = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "asg_free")
            .map_err(|e| ExternalError::Execution(format!("asg_free: {e}")))?;
        let evaluate = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "asg_evaluate")
            .map_err(|e| ExternalError::Execution(format!("asg_evaluate: {e}")))?;

        // 3. Serialize the input envelope.
        let input = serde_json::json!({
            "situation": situation_to_map(situation),
            "action": action,
            "agent_id": agent_id,
        });
        let input_bytes = serde_json::to_vec(&input)
            .map_err(|e| ExternalError::Execution(format!("input serialize: {e}")))?;
        let input_len = input_bytes.len() as i32;

        // 4. Alloc + write.
        let input_ptr = alloc
            .call(&mut store, input_len)
            .map_err(|e| ExternalError::Execution(format!("alloc input: {e}")))?;
        memory
            .write(&mut store, input_ptr as usize, &input_bytes)
            .map_err(|e| ExternalError::Execution(format!("write input: {e}")))?;

        // 5. Invoke.
        let packed = evaluate
            .call(&mut store, (input_ptr, input_len))
            .map_err(|e| ExternalError::Execution(format!("evaluate: {e}")))?;
        let out_ptr = (packed >> 32) as i32;
        let out_len = (packed & 0xFFFF_FFFF) as i32;

        if out_len < 0 {
            return Err(ExternalError::Execution(format!(
                "asg_evaluate returned negative output length ({out_len})"
            )));
        }

        // 6. Read output.
        let mut out_bytes = vec![0u8; out_len as usize];
        memory
            .read(&store, out_ptr as usize, &mut out_bytes)
            .map_err(|e| ExternalError::Execution(format!("read output: {e}")))?;

        // 7. Best-effort free; ignore errors (module may not care).
        let _ = free.call(&mut store, (input_ptr, input_len));
        let _ = free.call(&mut store, (out_ptr, out_len));

        // 8. Parse decision.
        let decision: Decision = serde_json::from_slice(&out_bytes)
            .map_err(|e| ExternalError::Execution(format!("parse decision: {e}")))?;
        Ok(decision)
    }
}
