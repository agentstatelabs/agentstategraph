//! WASM bindings for StateGraph — runs in browsers, Deno, and Node.
//!
//! Usage (browser/Deno):
//!   import init, { WasmStateGraph } from './stategraph_wasm.js'
//!   await init()
//!   const sg = new WasmStateGraph()
//!   sg.set("/name", "my-cluster", "Checkpoint", "init")
//!   sg.get("/name")  // → '"my-cluster"'

use wasm_bindgen::prelude::*;

use stategraph::speculation::SpecHandle;
use stategraph::{CommitOptions, Repository};
use stategraph_core::{IntentCategory, Object};
use stategraph_storage::IndexedDbStorage;

fn parse_category(s: &str) -> IntentCategory {
    match s.to_lowercase().as_str() {
        "explore" => IntentCategory::Explore,
        "refine" => IntentCategory::Refine,
        "fix" => IntentCategory::Fix,
        "rollback" => IntentCategory::Rollback,
        "checkpoint" => IntentCategory::Checkpoint,
        "merge" => IntentCategory::Merge,
        "migrate" => IntentCategory::Migrate,
        other => IntentCategory::Custom(other.to_string()),
    }
}

fn make_opts(description: &str, category: &str, reasoning: Option<String>, confidence: Option<f64>) -> CommitOptions {
    let cat = parse_category(category);
    let mut opts = CommitOptions::new("wasm", cat, description);
    if let Some(r) = reasoning {
        opts = opts.with_reasoning(r);
    }
    if let Some(c) = confidence {
        opts = opts.with_confidence(c);
    }
    opts
}

/// StateGraph for WASM — uses IndexedDB for persistent browser storage.
///
/// Architecture: in-memory cache with write-through to IndexedDB.
/// - All reads are instant (from memory)
/// - All writes queue changes for IndexedDB flush
/// - Call `drain_pending()` from JS to get queued writes, then persist to IndexedDB
/// - Call `load_data()` on startup to hydrate from IndexedDB
#[wasm_bindgen]
pub struct WasmStateGraph {
    repo: Repository,
    storage: std::sync::Arc<IndexedDbStorage>,
}

#[wasm_bindgen]
impl WasmStateGraph {
    /// Create a new StateGraph with IndexedDB-backed storage.
    /// After construction, call `load_data()` with data from IndexedDB to restore state.
    #[wasm_bindgen(constructor)]
    pub fn new(db_name: Option<String>) -> Result<WasmStateGraph, JsValue> {
        let name = db_name.unwrap_or_else(|| "stategraph".to_string());
        let storage = std::sync::Arc::new(IndexedDbStorage::new(&name));
        let repo = Repository::new(Box::new(IndexedDbStorage::new(&name)));
        repo.init().map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        // Re-create with shared storage so we can access pending writes
        let storage2 = std::sync::Arc::new(IndexedDbStorage::new(&name));
        let repo2 = Repository::new(Box::new(IndexedDbStorage::new(&name)));
        repo2.init().map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        Ok(Self { repo: repo2, storage: storage2 })
    }

    /// Load objects from IndexedDB dump. Call on startup.
    /// Pass a JSON string: [["hex_id", "json"], ...]
    pub fn load_objects(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage.load_objects(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Load commits from IndexedDB dump.
    pub fn load_commits(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage.load_commits(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Load refs from IndexedDB dump.
    pub fn load_refs(&self, json_pairs: &str) -> Result<(), JsValue> {
        let pairs: Vec<(String, String)> = serde_json::from_str(json_pairs)
            .map_err(|e| JsValue::from_str(&format!("parse error: {}", e)))?;
        self.storage.load_refs(&pairs)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Get pending object writes for flushing to IndexedDB. Returns JSON.
    pub fn drain_pending_objects(&self) -> String {
        let pending = self.storage.drain_pending_objects();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get pending commit writes.
    pub fn drain_pending_commits(&self) -> String {
        let pending = self.storage.drain_pending_commits();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get pending ref writes.
    pub fn drain_pending_refs(&self) -> String {
        let pending = self.storage.drain_pending_refs();
        serde_json::to_string(&pending).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get the IndexedDB database name.
    pub fn db_name(&self) -> String {
        self.storage.db_name().to_string()
    }

    /// Get a JSON value at a path.
    pub fn get(&self, path: &str, reference: Option<String>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let val = self.repo.get_json(&ref_name, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&val).unwrap_or_default())
    }

    /// Set a JSON value at a path.
    pub fn set(
        &self,
        path: &str,
        json_value: &str,
        category: &str,
        description: &str,
        reference: Option<String>,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let value: serde_json::Value = serde_json::from_str(json_value)
            .map_err(|e| JsValue::from_str(&format!("JSON error: {}", e)))?;
        let opts = make_opts(description, category, reasoning, confidence);
        let id = self.repo.set_json(&ref_name, path, &value, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Delete a value at a path.
    pub fn delete(
        &self,
        path: &str,
        category: &str,
        description: &str,
        reference: Option<String>,
    ) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let opts = make_opts(description, category, None, None);
        let id = self.repo.delete(&ref_name, path, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Create a branch.
    pub fn branch(&self, name: &str, from: Option<String>) -> Result<String, JsValue> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let id = self.repo.branch(name, &from_ref)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Merge source into target.
    pub fn merge(
        &self,
        source: &str,
        target: Option<String>,
        description: Option<String>,
    ) -> Result<String, JsValue> {
        let target_ref = target.unwrap_or_else(|| "main".to_string());
        let desc = description.unwrap_or_else(|| "merge".to_string());
        let opts = make_opts(&desc, "Merge", None, None);
        let id = self.repo.merge(source, &target_ref, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Structured diff between two refs. Returns JSON.
    pub fn diff(&self, ref_a: &str, ref_b: &str) -> Result<String, JsValue> {
        let ops = self.repo.diff(ref_a, ref_b)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&ops).unwrap_or_default())
    }

    /// Commit log. Returns JSON.
    pub fn log(&self, reference: Option<String>, limit: Option<u32>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let max = limit.unwrap_or(10) as usize;
        let commits = self.repo.log(&ref_name, max)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let entries: Vec<serde_json::Value> = commits.iter().map(|c| {
            serde_json::json!({
                "id": c.id.short(),
                "agent": c.agent_id,
                "intent": {
                    "category": format!("{:?}", c.intent.category),
                    "description": c.intent.description,
                    "tags": c.intent.tags,
                },
                "reasoning": c.reasoning,
                "confidence": c.confidence,
                "timestamp": c.timestamp.to_rfc3339(),
            })
        }).collect();
        Ok(serde_json::to_string(&entries).unwrap_or_default())
    }

    /// Blame — who modified a path and why.
    pub fn blame(&self, path: &str, reference: Option<String>) -> Result<String, JsValue> {
        let ref_name = reference.unwrap_or_else(|| "main".to_string());
        let entry = self.repo.blame(&ref_name, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(serde_json::to_string(&entry).unwrap_or_default())
    }

    /// Create a speculation. Returns handle ID.
    pub fn speculate(&self, from: Option<String>, label: Option<String>) -> Result<u32, JsValue> {
        let from_ref = from.unwrap_or_else(|| "main".to_string());
        let handle = self.repo.speculate(&from_ref, label)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(handle.id() as u32)
    }

    /// Get from a speculation.
    pub fn spec_get(&self, handle_id: u32, path: &str) -> Result<String, JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let obj = self.repo.spec_get(handle, path)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let val = match &obj {
            Object::Atom(a) => match a {
                stategraph_core::Atom::Null => serde_json::Value::Null,
                stategraph_core::Atom::Bool(b) => serde_json::json!(b),
                stategraph_core::Atom::Int(i) => serde_json::json!(i),
                stategraph_core::Atom::Float(f) => serde_json::json!(f),
                stategraph_core::Atom::String(s) => serde_json::json!(s),
                _ => serde_json::json!(format!("{:?}", obj)),
            },
            _ => serde_json::json!(format!("{:?}", obj)),
        };
        Ok(serde_json::to_string(&val).unwrap_or_default())
    }

    /// Set in a speculation.
    pub fn spec_set(&self, handle_id: u32, path: &str, json_value: &str) -> Result<(), JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let value: serde_json::Value = serde_json::from_str(json_value)
            .map_err(|e| JsValue::from_str(&format!("JSON: {}", e)))?;
        let obj = match &value {
            serde_json::Value::Null => Object::null(),
            serde_json::Value::Bool(b) => Object::bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() { Object::int(i) }
                else { Object::float(n.as_f64().unwrap_or(0.0)) }
            }
            serde_json::Value::String(s) => Object::string(s.clone()),
            _ => Object::string(value.to_string()),
        };
        self.repo.spec_set(handle, path, &obj)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Commit a speculation.
    pub fn commit_speculation(
        &self,
        handle_id: u32,
        category: &str,
        description: &str,
        reasoning: Option<String>,
        confidence: Option<f64>,
    ) -> Result<String, JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        let opts = make_opts(description, category, reasoning, confidence);
        let id = self.repo.commit_speculation(handle, opts)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(id.to_string())
    }

    /// Discard a speculation.
    pub fn discard_speculation(&self, handle_id: u32) -> Result<(), JsValue> {
        let handle = SpecHandle::from_id(handle_id as u64);
        self.repo.discard_speculation(handle)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Create an epoch.
    pub fn create_epoch(&self, id: &str, description: &str) -> Result<String, JsValue> {
        self.repo.create_epoch(id, description, vec![])
            .map(|e| format!("Epoch '{}' created", e.id))
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// Seal an epoch.
    pub fn seal_epoch(&self, id: &str, summary: &str) -> Result<(), JsValue> {
        self.repo.seal_epoch(id, summary)
            .map_err(|e| JsValue::from_str(&format!("{}", e)))
    }

    /// List epochs. Returns JSON.
    pub fn list_epochs(&self) -> Result<String, JsValue> {
        let entries = self.repo.list_epochs()
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        let json: Vec<serde_json::Value> = entries.iter().map(|e| {
            serde_json::json!({
                "id": e.id,
                "description": e.description,
                "status": format!("{:?}", e.status),
                "commits": e.commit_count,
            })
        }).collect();
        Ok(serde_json::to_string(&json).unwrap_or_default())
    }
}
