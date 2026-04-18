//! Round-trip integration test exercising the TaskStore + migrate FFI.
//!
//! Creates a repo, spawns a TaskStore, pushes a plan/task through the
//! pending → in_progress → done lifecycle across the C ABI, then checks
//! migrations against the current schema.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use agentstategraph_ffi::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

unsafe fn read(p: *mut c_char) -> String {
    assert!(!p.is_null(), "expected non-null result");
    let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
    agentstategraph_free_string(p);
    s
}

#[test]
fn full_taskstore_round_trip_via_ffi() {
    unsafe {
        let repo = agentstategraph_new_memory();
        assert!(!repo.is_null());

        let prefix = c("/plans");
        let agent = c("ffi-test-agent");
        let store = agentstategraph_taskstore_new(repo, prefix.as_ptr(), agent.as_ptr());
        assert!(!store.is_null());

        // Create plan.
        let ref_main = c("main");
        let plan_name = c("website-v2");
        let plan_desc = c("Brand pivot");
        let created = read(agentstategraph_taskstore_create_plan(
            store,
            ref_main.as_ptr(),
            plan_name.as_ptr(),
            plan_desc.as_ptr(),
        ));
        let plan_val: serde_json::Value = serde_json::from_str(&created).unwrap();
        assert_eq!(plan_val["name"], "website-v2");
        assert_eq!(plan_val["status"], "active");

        // List plans.
        let listed = read(agentstategraph_taskstore_list_plans(
            store,
            ref_main.as_ptr(),
        ));
        let listed_val: serde_json::Value = serde_json::from_str(&listed).unwrap();
        assert_eq!(listed_val.as_array().unwrap().len(), 1);

        // Add task.
        let title = c("Rewrite hero");
        let priority = c("high");
        let added = read(agentstategraph_taskstore_add_task(
            store,
            ref_main.as_ptr(),
            plan_name.as_ptr(),
            title.as_ptr(),
            priority.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        ));
        let task_val: serde_json::Value = serde_json::from_str(&added).unwrap();
        let task_id = task_val["id"].as_str().unwrap().to_string();
        assert_eq!(task_id, "t-001");
        assert_eq!(task_val["status"], "pending");

        // Start task.
        let id_c = c(&task_id);
        let started = read(agentstategraph_taskstore_start_task(
            store,
            ref_main.as_ptr(),
            plan_name.as_ptr(),
            id_c.as_ptr(),
        ));
        let started_val: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(started_val["status"], "in_progress");

        // Next-task should be none (no remaining pending).
        let next = read(agentstategraph_taskstore_next_task(
            store,
            ref_main.as_ptr(),
            plan_name.as_ptr(),
        ));
        assert_eq!(next, "null");

        // Complete with a commit proof.
        let kind = c("commit");
        let value = c("deadbeef");
        let completed = read(agentstategraph_taskstore_complete_task(
            store,
            ref_main.as_ptr(),
            plan_name.as_ptr(),
            id_c.as_ptr(),
            kind.as_ptr(),
            value.as_ptr(),
            std::ptr::null(),
        ));
        let completed_val: serde_json::Value = serde_json::from_str(&completed).unwrap();
        assert_eq!(completed_val["status"], "done");
        assert_eq!(completed_val["proof"]["kind"], "commit");
        assert_eq!(completed_val["proof"]["value"], "deadbeef");

        // Migrate check on a fresh repo should be up_to_date.
        let check = read(agentstategraph_migrate_check(
            repo,
            ref_main.as_ptr(),
            std::ptr::null(),
        ));
        let check_val: serde_json::Value = serde_json::from_str(&check).unwrap();
        assert_eq!(check_val["status"], "up_to_date");

        // Dry-run migrate.
        let mode = c("dry-run");
        let run = read(agentstategraph_migrate_run(
            repo,
            ref_main.as_ptr(),
            std::ptr::null(),
            mode.as_ptr(),
        ));
        let run_val: serde_json::Value = serde_json::from_str(&run).unwrap();
        assert_eq!(run_val["mode"], "dry-run");

        agentstategraph_taskstore_free(store);
        agentstategraph_free(repo);
    }
}

#[test]
fn null_inputs_return_null() {
    let p = agentstategraph_taskstore_new(std::ptr::null(), c("/plans").as_ptr(), c("a").as_ptr());
    assert!(p.is_null());
}
