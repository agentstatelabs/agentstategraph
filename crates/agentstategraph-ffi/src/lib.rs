//! C ABI for StateGraph — opaque handle-based API.
//!
//! All functions use opaque pointers and C strings.
//! The caller is responsible for freeing returned strings with stategraph_free_string.
//!
//! This crate produces a shared library (.so/.dylib/.dll) and static library (.a)
//! that any language with C FFI can call.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::{IntentCategory, Object};
use agentstategraph_storage::{MemoryStorage, SqliteStorage};

/// Opaque handle to a Repository.
pub struct SgRepo {
    inner: Repository,
}

/// Create a new in-memory StateGraph repository.
#[no_mangle]
pub extern "C" fn stategraph_new_memory() -> *mut SgRepo {
    let repo = Repository::new(Box::new(MemoryStorage::new()));
    if let Err(_) = repo.init() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(SgRepo { inner: repo }))
}

/// Create a new SQLite-backed StateGraph repository.
#[no_mangle]
pub extern "C" fn stategraph_new_sqlite(path: *const c_char) -> *mut SgRepo {
    let path = unsafe {
        if path.is_null() { return ptr::null_mut(); }
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
    if let Err(_) = repo.init() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(SgRepo { inner: repo }))
}

/// Free a repository handle.
#[no_mangle]
pub extern "C" fn stategraph_free(repo: *mut SgRepo) {
    if !repo.is_null() {
        unsafe { drop(Box::from_raw(repo)); }
    }
}

/// Free a string returned by StateGraph functions.
#[no_mangle]
pub extern "C" fn stategraph_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)); }
    }
}

/// Get a JSON value at a path. Returns a JSON string (caller must free).
#[no_mangle]
pub extern "C" fn stategraph_get(
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
pub extern "C" fn stategraph_set(
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
pub extern "C" fn stategraph_delete(
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
pub extern "C" fn stategraph_branch(
    repo: *const SgRepo,
    name: *const c_char,
    from: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo { Some(r) => r, None => return ptr::null_mut() };
    let name = unsafe { c_to_str(name) };
    let from = unsafe { c_to_str(from) };

    match repo.inner.branch(&name, &from) {
        Ok(id) => to_c_string(&id.to_string()),
        Err(_) => ptr::null_mut(),
    }
}

/// Diff two refs. Returns JSON string of DiffOps.
#[no_mangle]
pub extern "C" fn stategraph_diff(
    repo: *const SgRepo,
    ref_a: *const c_char,
    ref_b: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo { Some(r) => r, None => return ptr::null_mut() };
    let ref_a = unsafe { c_to_str(ref_a) };
    let ref_b = unsafe { c_to_str(ref_b) };

    match repo.inner.diff(&ref_a, &ref_b) {
        Ok(ops) => to_c_string(&serde_json::to_string(&ops).unwrap_or_default()),
        Err(_) => ptr::null_mut(),
    }
}

/// Merge source into target. Returns commit ID or error JSON.
#[no_mangle]
pub extern "C" fn stategraph_merge(
    repo: *const SgRepo,
    source: *const c_char,
    target: *const c_char,
    description: *const c_char,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo { Some(r) => r, None => return ptr::null_mut() };
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
pub extern "C" fn stategraph_log(
    repo: *const SgRepo,
    ref_name: *const c_char,
    limit: u32,
) -> *mut c_char {
    let repo = unsafe { repo.as_ref() };
    let repo = match repo { Some(r) => r, None => return ptr::null_mut() };
    let ref_name = unsafe { c_to_str(ref_name) };

    match repo.inner.log(&ref_name, limit as usize) {
        Ok(commits) => {
            let entries: Vec<serde_json::Value> = commits.iter().map(|c| {
                serde_json::json!({
                    "id": c.id.short(),
                    "agent": c.agent_id,
                    "intent_category": format!("{:?}", c.intent.category),
                    "intent_description": c.intent.description,
                    "reasoning": c.reasoning,
                    "confidence": c.confidence,
                })
            }).collect();
            to_c_string(&serde_json::to_string(&entries).unwrap_or_default())
        }
        Err(_) => ptr::null_mut(),
    }
}

/// Blame — returns JSON string with blame entry.
#[no_mangle]
pub extern "C" fn stategraph_blame(
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

unsafe fn c_to_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

unsafe fn parse_repo_ref_path<'a>(
    repo: *const SgRepo,
    ref_name: *const c_char,
    path: *const c_char,
) -> Option<(&'a SgRepo, String, String)> {
    let repo = repo.as_ref()?;
    let ref_name = c_to_str(ref_name);
    let path = c_to_str(path);
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
        "migrate" => IntentCategory::Migrate,
        other => IntentCategory::Custom(other.to_string()),
    }
}

fn to_c_string(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}
