//! §7 FFI smoke tests for the 8 new taint externs.

use std::ffi::{CStr, CString};

fn call_string(raw: *mut std::os::raw::c_char) -> String {
    assert!(!raw.is_null(), "extern returned null");
    let s = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    agentstategraph_ffi::agentstategraph_free_string(raw);
    s
}

#[test]
fn taint_apply_then_list_through_ffi() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    assert!(!repo.is_null());
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/cluster").unwrap();
    let params =
        CString::new(r#"{"name":"t1","effect":"warn","reason":"x","agent_id":"ops"}"#).unwrap();
    let raw = agentstategraph_ffi::agentstategraph_taint_apply(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    );
    let out = call_string(raw);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["ok"], true, "got {out}");
    let id = parsed["id"].as_str().unwrap().to_string();

    let list_raw = agentstategraph_ffi::agentstategraph_list_taints(
        repo,
        std::ptr::null(),
        std::ptr::null(),
        false,
    );
    let list = call_string(list_raw);
    assert!(list.contains(&id));

    agentstategraph_ffi::agentstategraph_free(repo);
}

#[test]
fn check_taint_through_ffi() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/cluster").unwrap();
    let params =
        CString::new(r#"{"name":"t1","effect":"block","reason":"x","agent_id":"ops"}"#).unwrap();
    let _ = call_string(agentstategraph_ffi::agentstategraph_taint_apply(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    ));
    let check_path = CString::new("/cluster/inner").unwrap();
    let agent = CString::new("agent-1").unwrap();
    let raw = agentstategraph_ffi::agentstategraph_check_taint(
        repo,
        check_path.as_ptr(),
        agent.as_ptr(),
        1.0,
    );
    let out = call_string(raw);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["check"]["can_write"], false);
    assert_eq!(parsed["check"]["tainted"], true);
    agentstategraph_ffi::agentstategraph_free(repo);
}

#[test]
fn quarantine_and_release_round_trip() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/secret").unwrap();
    let params = CString::new(
        r#"{"name":"sec","reason":"audit","authorized_agents":["sec"],"agent_id":"sec"}"#,
    )
    .unwrap();
    let apply = call_string(agentstategraph_ffi::agentstategraph_quarantine_apply(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    ));
    assert!(apply.contains("\"ok\":true"));

    let release_params =
        CString::new(r#"{"name":"sec","reason":"clear","proof":"c1","agent_id":"sec"}"#).unwrap();
    let release = call_string(agentstategraph_ffi::agentstategraph_quarantine_release(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        release_params.as_ptr(),
    ));
    assert!(release.contains("\"ok\":true"));
    agentstategraph_ffi::agentstategraph_free(repo);
}

#[test]
fn watch_apply_and_remove() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/metric").unwrap();
    let params = CString::new(
        r#"{"name":"w1","reason":"perf","metric":"pct","threshold":80.0,"agent_id":"ops"}"#,
    )
    .unwrap();
    let apply = call_string(agentstategraph_ffi::agentstategraph_watch_apply(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    ));
    assert!(apply.contains("\"ok\":true"));

    let remove_params = CString::new(r#"{"name":"w1","agent_id":"ops"}"#).unwrap();
    let remove = call_string(agentstategraph_ffi::agentstategraph_watch_remove(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        remove_params.as_ptr(),
    ));
    assert!(remove.contains("\"ok\":true"));
    agentstategraph_ffi::agentstategraph_free(repo);
}

#[test]
fn taint_apply_invalid_effect_returns_error() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/x").unwrap();
    let params =
        CString::new(r#"{"name":"bad","effect":"demolish","reason":"x","agent_id":"ops"}"#)
            .unwrap();
    let out = call_string(agentstategraph_ffi::agentstategraph_taint_apply(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    ));
    assert!(out.contains("error"));
    agentstategraph_ffi::agentstategraph_free(repo);
}

#[test]
fn taint_remove_on_unknown_name_errors() {
    let repo = agentstategraph_ffi::agentstategraph_new_memory();
    let ref_name = CString::new("main").unwrap();
    let path = CString::new("/ghost").unwrap();
    let params = CString::new(r#"{"name":"missing","reason":"x","agent_id":"ops"}"#).unwrap();
    let out = call_string(agentstategraph_ffi::agentstategraph_taint_remove(
        repo,
        ref_name.as_ptr(),
        path.as_ptr(),
        params.as_ptr(),
    ));
    assert!(out.contains("error"));
    agentstategraph_ffi::agentstategraph_free(repo);
}
