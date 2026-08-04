import Foundation
import CAgentStateGraph

extension AgentStateGraph {
    /// Inspect the repository and return its schema status as a JSON string.
    /// Pass `target = nil` to use the binary's own `SCHEMA_VERSION`.
    public func migrateCheck(target: String? = nil, ref: String = "main") throws -> String {
        let r = try rawHandle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cTarget = sgDup(target); defer { free(cTarget) }
        return try consume(agentstategraph_migrate_check(r, cRef, cTarget), "migrate_check")
    }

    /// Execute the migration plan. `mode` is `"apply"` or `"dry-run"`.
    /// Returns the run report as a JSON string.
    @discardableResult
    public func migrateRun(target: String? = nil, mode: String, ref: String = "main") throws -> String {
        let r = try rawHandle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cTarget = sgDup(target); defer { free(cTarget) }
        let cMode = sgDup(mode); defer { free(cMode) }
        return try consume(agentstategraph_migrate_run(r, cRef, cTarget, cMode), "migrate_run")
    }
}
