//! C ABI for AgentStateGraph — opaque handle-based API.
//!
//! All functions use opaque pointers and C strings.
//! The caller is responsible for freeing returned strings with agentstategraph_free_string.
//!
//! This crate produces a shared library (.so/.dylib/.dll) and static library (.a)
//! that any language with C FFI can call.
//!
//! Every `extern "C"` function in this crate takes raw pointers by design —
//! they form the C ABI contract. Marking them `unsafe fn` would change that
//! ABI and make every bindgen-generated header noisier. We validate pointer
//! inputs inside each function instead (`is_null` checks, bounded `CStr`
//! reads). The blanket allow below silences clippy on this pattern
//! crate-wide.

#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository, SCHEMA_VERSION};
use agentstategraph_core::IntentCategory;
use agentstategraph_migrate::{CheckResult, Registry, RunMode, StepStatus};
use agentstategraph_storage::{MemoryStorage, SqliteStorage};
use agentstategraph_tasks::{Priority, Proof, ProofKind, TaskId, TaskStore};
use semver::Version;
use serde::Serialize;

/// Opaque handle to a Repository.
pub struct SgRepo {
    inner: Arc<Repository>,
}

/// Opaque handle to a TaskStore.
pub struct SgTaskStore {
    inner: TaskStore,
}

/// Create a new in-memory AgentStateGraph repository.
#[no_mangle]
pub extern "C" fn agentstategraph_new_memory() -> *mut SgRepo {
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    if repo.init().is_err() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(SgRepo {
        inner: Arc::new(repo),
    }))
}

/// Create a new SQLite-backed AgentStateGraph repository.
#[no_mangle]
pub extern "C" fn agentstategraph_new_sqlite(path: *const c_char) -> *mut SgRepo {
    let path = unsafe {
        if path.is_null() {
            return ptr::null_mut();
        }
        match CStr::from_ptr(path).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return ptr::null_mut(),
        }
    };
    let storage = match SqliteStorage::open(&path) {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };
    let repo = Repository::new(Box::new(storage));
    if repo.init().is_err() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(SgRepo {
        inner: Arc::new(repo),
    }))
}

/// Free a repository handle.
#[no_mangle]
pub extern "C" fn agentstategraph_free(repo: *mut SgRepo) {
    if !repo.is_null() {
        unsafe {
            drop(Box::from_raw(repo));
        }
    }
}

/// Free a string returned by AgentStateGraph functions.
#[no_mangle]
pub extern "C" fn agentstategraph_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

/// Get a JSON value at a path. Returns a JSON string (caller must free).
#[no_mangle]
pub extern "C" fn agentstategraph_get(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    let (repo, ref_name, path) = match unsafe { parse_repo_ref_path(repo, ref_name, path) } {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    match repo.inner.get_json(&ref_name, &path) {
        Ok(val) => to_c_string(&serde_json::to_string(&val).unwrap_or_default()),
        Err(_) => ptr::null_mut(),
    }
}

/// Set a JSON value at a path. Returns commit ID string (caller must free).
#[no_mangle]
pub extern "C" fn agentstategraph_set(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
    json_value: *const c_char,
    intent_category: *const c_char,
    intent_description: *const c_char,
) -> *mut c_char {
    let (repo, ref_name, path) = match unsafe { parse_repo_ref_path(repo, ref_name, path) } {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let json_str = unsafe { c_to_str(json_value) };
    let category_str = unsafe { c_to_str(intent_category) };
    let desc_str = unsafe { c_to_str(intent_description) };

    let value: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return ptr::null_mut(),
    };

    let category = parse_category(&category_str);
    let opts = CommitOptions::new("ffi", category, &desc_str);

    match repo.inner.set_json(&ref_name, &path, &value, opts) {
        Ok(id) => to_c_string(&id.to_string()),
        Err(_) => ptr::null_mut(),
    }
}

/// Delete a value at a path. Returns commit ID string.
#[no_mangle]
pub extern "C" fn agentstategraph_delete(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
    intent_category: *const c_char,
    intent_description: *const c_char,
) -> *mut c_char {
    let (repo, ref_name, path) = match unsafe { parse_repo_ref_path(repo, ref_name, path) } {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    let category_str = unsafe { c_to_str(intent_category) };
    let desc_str = unsafe { c_to_str(intent_description) };
    let category = parse_category(&category_str);
    let opts = CommitOptions::new("ffi", category, &desc_str);

    match repo.inner.delete(&ref_name, &path, opts) {
        Ok(id) => to_c_string(&id.to_string()),
        Err(_) => ptr::null_mut(),
    }
}

/// Create a branch. Returns commit ID string.
#[no_mangle]
pub extern "C" fn agentstategraph_branch(
    repo: *const SgRepo,
    name: *const c_char,
    from: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let name = unsafe { c_to_str(name) };
    let from = unsafe { c_to_str(from) };

    match repo.inner.branch(&name, &from) {
        Ok(id) => to_c_string(&id.to_string()),
        Err(_) => ptr::null_mut(),
    }
}

/// Diff two refs. Returns JSON string of DiffOps.
#[no_mangle]
pub extern "C" fn agentstategraph_diff(
    repo: *const SgRepo,
    ref_a: *const c_char,
    ref_b: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let ref_a = unsafe { c_to_str(ref_a) };
    let ref_b = unsafe { c_to_str(ref_b) };

    match repo.inner.diff(&ref_a, &ref_b) {
        Ok(ops) => to_c_string(&serde_json::to_string(&ops).unwrap_or_default()),
        Err(_) => ptr::null_mut(),
    }
}

/// Merge source into target. Returns commit ID or error JSON.
#[no_mangle]
pub extern "C" fn agentstategraph_merge(
    repo: *const SgRepo,
    source: *const c_char,
    target: *const c_char,
    description: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let source = unsafe { c_to_str(source) };
    let target = unsafe { c_to_str(target) };
    let desc = unsafe { c_to_str(description) };

    let opts = CommitOptions::new("ffi", IntentCategory::Merge, &desc);
    match repo.inner.merge(&source, &target, opts) {
        Ok(id) => to_c_string(&id.to_string()),
        Err(e) => to_c_string(&format!("error:{}", e)),
    }
}

/// Get commit log as JSON. Returns JSON array string.
#[no_mangle]
pub extern "C" fn agentstategraph_log(
    repo: *const SgRepo,
    ref_name: *const c_char,
    limit: u32,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let ref_name = unsafe { c_to_str(ref_name) };

    match repo.inner.log(&ref_name, limit as usize) {
        Ok(commits) => {
            let entries: Vec<serde_json::Value> = commits
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id.short(),
                        "agent": c.agent_id,
                        "intent_category": format!("{:?}", c.intent.category),
                        "intent_description": c.intent.description,
                        "reasoning": c.reasoning,
                        "confidence": c.confidence,
                    })
                })
                .collect();
            to_c_string(&serde_json::to_string(&entries).unwrap_or_default())
        }
        Err(_) => ptr::null_mut(),
    }
}

/// Blame — returns JSON string with blame entry.
#[no_mangle]
pub extern "C" fn agentstategraph_blame(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
) -> *mut c_char {
    let (repo, ref_name, path) = match unsafe { parse_repo_ref_path(repo, ref_name, path) } {
        Some(v) => v,
        None => return ptr::null_mut(),
    };
    match repo.inner.blame(&ref_name, &path) {
        Ok(entry) => to_c_string(&serde_json::to_string(&entry).unwrap_or_default()),
        Err(_) => ptr::null_mut(),
    }
}

// ===========================================================================
// TaskStore FFI
// ===========================================================================

/// Create a new TaskStore handle bound to the given repository, path
/// prefix, and agent id. The returned pointer must be freed with
/// `agentstategraph_taskstore_free`. The underlying repository is shared
/// (refcounted); freeing the TaskStore does NOT free the repository.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_new(
    repo: *const SgRepo,
    prefix: *const c_char,
    agent_id: *const c_char,
) -> *mut SgTaskStore {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let prefix = unsafe { c_to_str(prefix) };
    let agent_id = unsafe { c_to_str(agent_id) };
    if prefix.is_empty() || agent_id.is_empty() {
        return ptr::null_mut();
    }
    let store = TaskStore::new(repo.inner.clone(), prefix, agent_id);
    Box::into_raw(Box::new(SgTaskStore { inner: store }))
}

/// Free a TaskStore handle.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_free(store: *mut SgTaskStore) {
    if !store.is_null() {
        unsafe {
            drop(Box::from_raw(store));
        }
    }
}

fn taskstore_ref<'a>(store: *const SgTaskStore) -> Option<&'a SgTaskStore> {
    unsafe { store.as_ref() }
}

fn json_ok<T: Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => to_c_string(&s),
        Err(_) => ptr::null_mut(),
    }
}

fn json_err(msg: &str) -> *mut c_char {
    to_c_string(&format!(
        "{{\"error\":{}}}",
        serde_json::to_string(msg).unwrap_or_else(|_| "\"error\"".into())
    ))
}

// Plan ops ------------------------------------------------------------------

/// Create a plan. Returns the Plan as JSON, or `{"error": ...}` on failure.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_create_plan(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    name: *const c_char,
    description: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let name = unsafe { c_to_str(name) };
    let description = if description.is_null() {
        None
    } else {
        let s = unsafe { c_to_str(description) };
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    match store.inner.create_plan(&ref_name, &name, description) {
        Ok(plan) => json_ok(&plan),
        Err(e) => json_err(&e.to_string()),
    }
}

/// List plans. Returns JSON array.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_list_plans(
    store: *const SgTaskStore,
    ref_name: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    match store.inner.list_plans(&ref_name) {
        Ok(p) => json_ok(&p),
        Err(e) => json_err(&e.to_string()),
    }
}

/// List plans filtered by status. `status` is "active", "completed",
/// "archived", or null/empty for all. Returns JSON array.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_list_plans_by_status(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    status: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let status_str = unsafe { c_to_str(status) };
    let parsed = parse_plan_status(&status_str);
    match store.inner.list_plans_by_status(&ref_name, parsed) {
        Ok(p) => json_ok(&p),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Get a plan by name.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_get_plan(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    name: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let name = unsafe { c_to_str(name) };
    match store.inner.get_plan(&ref_name, &name) {
        Ok(p) => json_ok(&p),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_archive_plan(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    name: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let name = unsafe { c_to_str(name) };
    match store.inner.archive_plan(&ref_name, &name) {
        Ok(p) => json_ok(&p),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_delete_plan(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    name: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let name = unsafe { c_to_str(name) };
    match store.inner.delete_plan(&ref_name, &name) {
        Ok(()) => to_c_string("{\"ok\":true}"),
        Err(e) => json_err(&e.to_string()),
    }
}

// Task ops ------------------------------------------------------------------

/// Add a task. Arguments:
///   - priority: "low" | "medium" | "high" | "critical"
///   - parent_id: nullable, e.g. "t-001"
///   - blockers_json: JSON array of task ids (may be null or "[]")
///   - assigned_to: nullable agent id
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_add_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    title: *const c_char,
    priority: *const c_char,
    parent_id: *const c_char,
    blockers_json: *const c_char,
    assigned_to: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let title = unsafe { c_to_str(title) };
    let priority_str = unsafe { c_to_str(priority) };
    let prio = parse_priority(&priority_str);

    let parent = if parent_id.is_null() {
        None
    } else {
        let s = unsafe { c_to_str(parent_id) };
        if s.is_empty() {
            None
        } else {
            Some(TaskId(s))
        }
    };

    let blockers: Vec<TaskId> = if blockers_json.is_null() {
        Vec::new()
    } else {
        let s = unsafe { c_to_str(blockers_json) };
        if s.is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<String>>(&s) {
                Ok(v) => v.into_iter().map(TaskId).collect(),
                Err(e) => return json_err(&format!("invalid blockers_json: {e}")),
            }
        }
    };

    let assigned = if assigned_to.is_null() {
        None
    } else {
        let s = unsafe { c_to_str(assigned_to) };
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };

    match store
        .inner
        .add_task(&ref_name, &plan, &title, prio, parent, blockers, assigned)
    {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_list_tasks(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    match store.inner.list_tasks(&ref_name, &plan) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_task_ids(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    match store.inner.task_ids(&ref_name, &plan) {
        Ok(ids) => {
            let strs: Vec<String> = ids.into_iter().map(|i| i.0).collect();
            json_ok(&strs)
        }
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_get_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    match store.inner.get_task(&ref_name, &plan, &id) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_start_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    match store.inner.start_task(&ref_name, &plan, &id) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Complete a task. `proof_kind` is "commit" | "file" | "test" | "text".
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_complete_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
    proof_kind: *const c_char,
    proof_value: *const c_char,
    proof_note: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    let kind_str = unsafe { c_to_str(proof_kind) };
    let value = unsafe { c_to_str(proof_value) };
    let kind = match parse_proof_kind(&kind_str) {
        Some(k) => k,
        None => return json_err(&format!("invalid proof kind: {kind_str}")),
    };
    let note = if proof_note.is_null() {
        None
    } else {
        let s = unsafe { c_to_str(proof_note) };
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    let proof = Proof { kind, value, note };
    match store.inner.complete_task(&ref_name, &plan, &id, proof) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_abandon_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
    reason: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    let reason = unsafe { c_to_str(reason) };
    match store.inner.abandon_task(&ref_name, &plan, &id, &reason) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_set_priority(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
    priority: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    let prio = parse_priority(&unsafe { c_to_str(priority) });
    match store.inner.set_priority(&ref_name, &plan, &id, prio) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Set blockers. `blockers_json` is a JSON string array (may be null or "[]").
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_set_blockers(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
    blockers_json: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    let blockers: Vec<TaskId> = if blockers_json.is_null() {
        Vec::new()
    } else {
        let s = unsafe { c_to_str(blockers_json) };
        if s.is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<String>>(&s) {
                Ok(v) => v.into_iter().map(TaskId).collect(),
                Err(e) => return json_err(&format!("invalid blockers_json: {e}")),
            }
        }
    };
    match store.inner.set_blockers(&ref_name, &plan, &id, blockers) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_assign_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
    agent: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    let agent = unsafe { c_to_str(agent) };
    match store.inner.assign_task(&ref_name, &plan, &id, &agent) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_unassign_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    task_id: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(task_id) });
    match store.inner.unassign_task(&ref_name, &plan, &id) {
        Ok(t) => json_ok(&t),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Returns the next unblocked task as JSON, `null` if none, or error JSON.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_next_task(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    match store.inner.next_task(&ref_name, &plan) {
        Ok(opt) => json_ok(&opt),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Next task filtered by assignment. `agent` null for any; `include_unassigned`
/// controls fallback to unassigned when an agent is specified.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_next_task_for(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    agent: *const c_char,
    include_unassigned: u8,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let agent_str = if agent.is_null() {
        None
    } else {
        let s = unsafe { c_to_str(agent) };
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    match store.inner.next_task_for(
        &ref_name,
        &plan,
        agent_str.as_deref(),
        include_unassigned != 0,
    ) {
        Ok(opt) => json_ok(&opt),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Compute the derived (rollup) status of a parent task. Returns
/// `"pending"` | `"in_progress"` | `"done"` | `"abandoned"` JSON-wrapped.
#[no_mangle]
pub extern "C" fn agentstategraph_taskstore_derived_status(
    store: *const SgTaskStore,
    ref_name: *const c_char,
    plan: *const c_char,
    parent_id: *const c_char,
) -> *mut c_char {
    let Some(store) = taskstore_ref(store) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let plan = unsafe { c_to_str(plan) };
    let id = TaskId(unsafe { c_to_str(parent_id) });
    match store.inner.derived_status(&ref_name, &plan, &id) {
        Ok(s) => json_ok(&s),
        Err(e) => json_err(&e.to_string()),
    }
}

// ===========================================================================
// Migration FFI
// ===========================================================================

/// Check the migration status of a repository. Returns a JSON report:
/// `{"status":"up_to_date","version":"0.4.0"}`, `"upgrade_available"`,
/// `"downgrade"`, `"unversioned"`, `"corrupt"`. `target` may be null to
/// use the binary's current `SCHEMA_VERSION`.
#[no_mangle]
pub extern "C" fn agentstategraph_migrate_check(
    repo: *const SgRepo,
    ref_name: *const c_char,
    target: *const c_char,
) -> *mut c_char {
    let Some(repo) = (unsafe { repo.as_ref() }) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let target_str = if target.is_null() {
        SCHEMA_VERSION.to_string()
    } else {
        let s = unsafe { c_to_str(target) };
        if s.is_empty() {
            SCHEMA_VERSION.to_string()
        } else {
            s
        }
    };
    let target_version = match Version::parse(&target_str) {
        Ok(v) => v,
        Err(e) => return json_err(&format!("invalid target: {e}")),
    };
    let registry = Registry::builtin();
    match agentstategraph_migrate::check(&repo.inner, &ref_name, &target_version, &registry) {
        Ok(r) => to_c_string(&check_result_json(&r)),
        Err(e) => json_err(&e.to_string()),
    }
}

/// Run migrations. `mode` is `"apply"` or `"dry-run"`. Returns a JSON
/// report describing each step.
#[no_mangle]
pub extern "C" fn agentstategraph_migrate_run(
    repo: *const SgRepo,
    ref_name: *const c_char,
    target: *const c_char,
    mode: *const c_char,
) -> *mut c_char {
    let Some(repo) = (unsafe { repo.as_ref() }) else {
        return ptr::null_mut();
    };
    let ref_name = unsafe { c_to_str(ref_name) };
    let target_str = if target.is_null() {
        SCHEMA_VERSION.to_string()
    } else {
        let s = unsafe { c_to_str(target) };
        if s.is_empty() {
            SCHEMA_VERSION.to_string()
        } else {
            s
        }
    };
    let target_version = match Version::parse(&target_str) {
        Ok(v) => v,
        Err(e) => return json_err(&format!("invalid target: {e}")),
    };
    let mode_str = unsafe { c_to_str(mode) };
    let run_mode = match mode_str.to_lowercase().as_str() {
        "apply" => RunMode::Apply,
        "dry-run" | "dryrun" | "dry_run" => RunMode::DryRun,
        other => return json_err(&format!("invalid mode: {other}")),
    };
    let registry = Registry::builtin();
    match registry.run(&repo.inner, &ref_name, &target_version, run_mode) {
        Ok(r) => to_c_string(&report_json(&r)),
        Err(e) => json_err(&e.to_string()),
    }
}

fn check_result_json(r: &CheckResult) -> String {
    let v = match r {
        CheckResult::UpToDate { version } => serde_json::json!({
            "status": "up_to_date",
            "version": version.to_string(),
        }),
        CheckResult::UpgradeAvailable {
            from,
            to,
            migrations,
        } => serde_json::json!({
            "status": "upgrade_available",
            "from": from.to_string(),
            "to": to.to_string(),
            "migrations": migrations,
        }),
        CheckResult::Downgrade { db, binary } => serde_json::json!({
            "status": "downgrade",
            "db": db.to_string(),
            "binary": binary.to_string(),
        }),
        CheckResult::Unversioned { implicit } => serde_json::json!({
            "status": "unversioned",
            "implicit": implicit.to_string(),
        }),
        CheckResult::Corrupt(msg) => serde_json::json!({
            "status": "corrupt",
            "message": msg,
        }),
    };
    serde_json::to_string(&v).unwrap_or_default()
}

fn report_json(r: &agentstategraph_migrate::Report) -> String {
    let mode = match r.mode {
        RunMode::DryRun => "dry-run",
        RunMode::Apply => "apply",
    };
    let steps: Vec<serde_json::Value> = r
        .steps
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "describe": s.describe,
                "from": s.from.to_string(),
                "to": s.to.to_string(),
                "status": step_status_str(s.status),
                "commit_id": s.commit_id.as_ref().map(|c| c.to_string()),
                "notes": s.notes,
            })
        })
        .collect();
    let v = serde_json::json!({
        "from": r.from.to_string(),
        "target": r.target.to_string(),
        "final_version": r.final_version.to_string(),
        "mode": mode,
        "steps": steps,
    });
    serde_json::to_string(&v).unwrap_or_default()
}

fn step_status_str(s: StepStatus) -> &'static str {
    match s {
        StepStatus::WouldApply => "would-apply",
        StepStatus::WouldSkip => "would-skip",
        StepStatus::Applied => "applied",
        StepStatus::Skipped => "skipped",
        StepStatus::Failed => "failed",
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

unsafe fn c_to_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
    }
}

unsafe fn parse_repo_ref_path<'a>(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
) -> Option<(&'a SgRepo, String, String)> {
    let repo = unsafe { repo.as_ref()? };
    let ref_name = unsafe { c_to_str(ref_name) };
    let path = unsafe { c_to_str(path) };
    Some((repo, ref_name, path))
}

fn parse_category(s: &str) -> IntentCategory {
    match s.to_lowercase().as_str() {
        "explore" => IntentCategory::Explore,
        "refine" => IntentCategory::Refine,
        "fix" => IntentCategory::Fix,
        "rollback" => IntentCategory::Rollback,
        "checkpoint" => IntentCategory::Checkpoint,
        "merge" => IntentCategory::Merge,
        // SECURITY (threat model v2, finding C3): the FFI boundary has no
        // capability check — a host embedding ASG via C ABI can pass any
        // string. Map "migrate" to a Custom category so `/_meta/*` writes
        // are rejected by the substrate's reserved-path guard. Hosts that
        // genuinely need migrations should construct `IntentCategory::Migrate`
        // directly in Rust (not via this parser).
        "migrate" => IntentCategory::Custom("Migrate-requested".into()),
        "plan" => IntentCategory::Plan,
        other => IntentCategory::Custom(other.to_string()),
    }
}

fn parse_priority(s: &str) -> Priority {
    match s.to_lowercase().as_str() {
        "low" => Priority::Low,
        "high" => Priority::High,
        "critical" => Priority::Critical,
        _ => Priority::Medium,
    }
}

fn parse_plan_status(s: &str) -> Option<agentstategraph_tasks::PlanStatus> {
    match s.to_lowercase().as_str() {
        "" => None,
        "active" => Some(agentstategraph_tasks::PlanStatus::Active),
        "completed" => Some(agentstategraph_tasks::PlanStatus::Completed),
        "archived" => Some(agentstategraph_tasks::PlanStatus::Archived),
        _ => None,
    }
}

fn parse_proof_kind(s: &str) -> Option<ProofKind> {
    match s.to_lowercase().as_str() {
        "commit" => Some(ProofKind::Commit),
        "file" => Some(ProofKind::File),
        "test" => Some(ProofKind::Test),
        "text" => Some(ProofKind::Text),
        _ => None,
    }
}

fn to_c_string(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}
