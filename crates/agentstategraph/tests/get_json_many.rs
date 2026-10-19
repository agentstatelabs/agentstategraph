//! `Repository::get_json_many` answers like one `get_json` per path.

use agentstategraph::{CommitOptions, RepoError, Repository};
use agentstategraph_core::IntentCategory;
use agentstategraph_storage::SqliteStorage;
use serde_json::json;

fn repo() -> Repository {
    let repo = Repository::new(Box::new(SqliteStorage::in_memory().unwrap()));
    repo.init().unwrap();
    let state = json!({
        "sessions": {
            "s1": {"title": "first", "meta": {"source": "claude", "models_used": ["opus"]}},
            "s2": {"meta": {"source": "codex", "models_used": []}},
        },
        "memory": {"pinned": {"rules": {"title": "Be brief", "body": "…"}}},
    });
    repo.set_json(
        "main",
        "/",
        &state,
        CommitOptions::new("test", IntentCategory::Custom("Seed".into()), "seed"),
    )
    .unwrap();
    repo
}

#[test]
fn each_path_reads_as_get_json_would() {
    let repo = repo();
    let paths = [
        "/sessions/s1/title",
        "/sessions/s1/meta",
        "/sessions/s2/title",
        "/sessions/s2/meta",
        "/memory/pinned",
        "/nope/at/all",
    ];
    let many = repo.get_json_many("main", &paths).unwrap();
    assert_eq!(many.len(), paths.len());
    for (path, got) in paths.iter().zip(&many) {
        assert_eq!(got, &repo.get_json("main", path).ok(), "{path}");
    }
    assert_eq!(many[0], Some(json!("first")));
    assert_eq!(many[2], None);
    assert!(repo.get_json_many("main", &[]).unwrap().is_empty());
}

#[test]
fn secret_paths_are_refused() {
    let repo = repo();
    let err = repo
        .get_json_many("main", &["/memory/pinned", "/_meta/_secret/key"])
        .unwrap_err();
    assert!(matches!(err, RepoError::ReservedPath(_)), "{err:?}");
}

#[test]
fn an_unknown_ref_is_an_error() {
    let repo = repo();
    assert!(repo.get_json_many("no-such-branch", &["/memory"]).is_err());
}
