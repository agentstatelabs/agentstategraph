//! Reports and errors as JSON, for the surfaces that hand them on: the MCP
//! server, the HTTP API, the FFI and the bindings. Commit ids are given as
//! `sg_` and their hex, as the rest of the API gives them.

use agentstategraph::sync::{
    FetchReport, PullOutcome, PullReport, PushReport, RefChange, SyncError,
};
use agentstategraph::{RepoError, Repository};
use serde_json::{Value, json};

/// Every named remote `repo` keeps: its name, URL, store and token
/// variable, and where each of its branches was at the last fetch or push.
pub fn remotes(repo: &Repository) -> Result<Value, RepoError> {
    let mut out = Vec::new();
    for (name, config) in repo.remotes()? {
        let refs: serde_json::Map<String, Value> = repo
            .remote_refs(&name)
            .map_err(|e| RepoError::InvalidOperation(e.to_string()))?
            .into_iter()
            .map(|(r, id)| (r, Value::String(id.to_string())))
            .collect();
        out.push(json!({
            "name": name,
            "url": config.url,
            "store": config.store,
            "token_env": config.token_env,
            "refs": refs,
        }));
    }
    Ok(Value::Array(out))
}

fn change(c: &RefChange) -> Value {
    json!({
        "namespace": c.namespace.to_string(),
        "name": c.name,
        "old": c.old.map(|o| o.to_string()),
        "new": c.new.to_string(),
    })
}

pub fn fetch(report: &FetchReport) -> Value {
    json!({
        "updated": report.updated.iter().map(change).collect::<Vec<_>>(),
        "commits": report.commits,
        "objects": report.objects,
        "control_rebuilt": report.control.as_ref().map(|c| c.rebuilt),
        "epoch_conflicts": report.epoch_conflicts.iter().map(|c| json!({
            "epoch": c.epoch,
            "kept_seal": c.kept_seal.map(|s| s.to_string()),
            "dropped_seal": c.dropped_seal.map(|s| s.to_string()),
        })).collect::<Vec<_>>(),
    })
}

pub fn push(report: &PushReport) -> Value {
    json!({
        "updated": report.updated.iter().map(change).collect::<Vec<_>>(),
        "commits": report.commits,
        "objects": report.objects,
        "overridden": report.overridden.iter().map(|(f, o)| json!({
            "finding": f.id,
            "filter": f.filter,
            "rule": f.rule,
            "path": f.path,
            "reason": o.reason,
        })).collect::<Vec<_>>(),
    })
}

pub fn pull_outcome(outcome: &PullOutcome) -> Value {
    let (kind, commit) = match outcome {
        PullOutcome::UpToDate => ("up_to_date", None),
        PullOutcome::Created(c) => ("created", Some(c)),
        PullOutcome::FastForward(c) => ("fast_forward", Some(c)),
        PullOutcome::Merged(c) => ("merged", Some(c)),
    };
    json!({ "outcome": kind, "commit": commit.map(|c| c.to_string()) })
}

pub fn pull(report: &PullReport) -> Value {
    let mut out = pull_outcome(&report.outcome);
    out["fetch"] = fetch(&report.fetch);
    out
}

/// An error's code and message, and a blocked push's findings (never their
/// values), so a caller can override them by id.
pub fn error(e: &SyncError) -> Value {
    let mut out = json!({ "code": e.code(), "message": e.to_string() });
    if let SyncError::Blocked { findings, .. } = e {
        out["findings"] = findings
            .iter()
            .map(|f| {
                json!({
                    "id": f.id, "filter": f.filter, "rule": f.rule, "path": f.path,
                    "overridable": f.overridable,
                })
            })
            .collect();
    }
    out
}
