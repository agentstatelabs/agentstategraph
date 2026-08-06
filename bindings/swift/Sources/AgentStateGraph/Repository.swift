import Foundation
import CAgentStateGraph

/// Intent categories understood by the AgentStateGraph commit layer.
///
/// Every mutation records *why* it happened. Pass one of these to `set`,
/// `delete`, etc. Unknown strings fall back to the native default, so you
/// may also use `.other("myCategory")` for project-specific intents.
public enum IntentCategory: RawRepresentable, Sendable, Equatable {
    case checkpoint
    case correction
    case refinement
    case exploration
    case merge
    case rollback
    case other(String)

    public init(rawValue: String) {
        switch rawValue.lowercased() {
        case "checkpoint": self = .checkpoint
        case "correction": self = .correction
        case "refinement": self = .refinement
        case "exploration": self = .exploration
        case "merge": self = .merge
        case "rollback": self = .rollback
        default: self = .other(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .checkpoint: return "Checkpoint"
        case .correction: return "Correction"
        case .refinement: return "Refinement"
        case .exploration: return "Exploration"
        case .merge: return "Merge"
        case .rollback: return "Rollback"
        case .other(let s): return s
        }
    }
}

/// One row returned by `listBranches`.
public struct BranchEntry: Codable, Sendable, Equatable {
    public let name: String
    public let target: String
}

/// One row returned by `log`.
public struct LogEntry: Codable, Sendable, Equatable {
    public let id: String
    public let agent: String
    public let intentCategory: String
    public let intentDescription: String
    public let reasoning: String?
    public let confidence: Double?

    enum CodingKeys: String, CodingKey {
        case id, agent, reasoning, confidence
        case intentCategory = "intent_category"
        case intentDescription = "intent_description"
    }
}

/// A handle to an AgentStateGraph repository — an AI-native, versioned,
/// intent-carrying state store.
///
/// ```swift
/// let asg = try AgentStateGraph()          // in-memory
/// try asg.set("/name", json: "\"my-cluster\"", category: .checkpoint, description: "init")
/// let name = try asg.get("/name")          // "\"my-cluster\""
/// asg.close()
/// ```
///
/// The handle owns native memory. Call `close()` when done, or rely on
/// `deinit`. Not safe to use after `close()`.
public final class AgentStateGraph {
    private var repo: UnsafeMutableRawPointer?

    init(repo: UnsafeMutableRawPointer?) throws {
        guard let repo = repo else {
            throw AgentStateGraphError.operationFailed("open repository")
        }
        self.repo = repo
    }

    /// Create a new in-memory (ephemeral) repository.
    public convenience init() throws {
        try self.init(repo: agentstategraph_new_memory())
    }

    /// Open (or create) a durable SQLite-backed repository at `path`.
    public convenience init(path: String) throws {
        let c = sgDup(path); defer { free(c) }
        try self.init(repo: agentstategraph_new_sqlite(c))
    }

    deinit { close() }

    /// Free the underlying repository. Idempotent.
    public func close() {
        if let r = repo {
            agentstategraph_free(r)
            repo = nil
        }
    }

    private func handle() throws -> UnsafeMutableRawPointer {
        guard let r = repo else { throw AgentStateGraphError.closed("repository") }
        return r
    }

    /// Raw pointer for sibling stores (TaskStore/PolicyStore). Internal.
    func rawHandle() throws -> UnsafeMutableRawPointer { try handle() }

    // MARK: - Values

    /// Return the JSON value at `path` (as a JSON string) on `ref`.
    public func get(_ path: String, ref: String = "main") throws -> String {
        let r = try handle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        return try consume(agentstategraph_get(r, cRef, cPath), "get")
    }

    /// Decode the JSON value at `path` into a `Decodable` type.
    public func get<T: Decodable>(_ path: String, as type: T.Type, ref: String = "main") throws -> T {
        try decodeJSON(try get(path, ref: ref))
    }

    /// Write a raw JSON string at `path`, recording intent. Returns the commit id.
    @discardableResult
    public func set(
        _ path: String,
        json: String,
        category: IntentCategory,
        description: String,
        ref: String = "main"
    ) throws -> String {
        let r = try handle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cVal = sgDup(json); defer { free(cVal) }
        let cCat = sgDup(category.rawValue); defer { free(cCat) }
        let cDesc = sgDup(description); defer { free(cDesc) }
        return try consume(agentstategraph_set(r, cRef, cPath, cVal, cCat, cDesc), "set")
    }

    /// Encode `value` to JSON and write it at `path`. Returns the commit id.
    @discardableResult
    public func set<T: Encodable>(
        _ path: String,
        value: T,
        category: IntentCategory,
        description: String,
        ref: String = "main"
    ) throws -> String {
        try set(path, json: try encodeJSON(value), category: category, description: description, ref: ref)
    }

    /// Delete the value at `path`, recording intent. Returns the commit id.
    @discardableResult
    public func delete(
        _ path: String,
        category: IntentCategory,
        description: String,
        ref: String = "main"
    ) throws -> String {
        let r = try handle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cCat = sgDup(category.rawValue); defer { free(cCat) }
        let cDesc = sgDup(description); defer { free(cDesc) }
        return try consume(agentstategraph_delete(r, cRef, cPath, cCat, cDesc), "delete")
    }

    // MARK: - Branches

    /// Create branch `name` starting from ref `from`. Returns the commit id.
    @discardableResult
    public func branch(_ name: String, from: String) throws -> String {
        let r = try handle()
        let cName = sgDup(name); defer { free(cName) }
        let cFrom = sgDup(from); defer { free(cFrom) }
        return try consume(agentstategraph_branch(r, cName, cFrom), "branch")
    }

    /// List branches whose name starts with `prefix` (pass `nil` for all).
    public func listBranches(prefix: String? = nil) throws -> [BranchEntry] {
        let r = try handle()
        let cPrefix = sgDup(prefix); defer { free(cPrefix) }
        let raw = try consume(agentstategraph_list_branches(r, cPrefix), "list_branches")
        return try decodeJSON(raw)
    }

    /// Delete branch `name`. Returns `true` if it existed, `false` if not.
    @discardableResult
    public func deleteBranch(_ name: String) throws -> Bool {
        let r = try handle()
        let cName = sgDup(name); defer { free(cName) }
        let raw = try consume(agentstategraph_delete_branch(r, cName), "delete_branch")
        struct Resp: Decodable { let deleted: Bool }
        return try decodeJSON(raw, as: Resp.self).deleted
    }

    // MARK: - History / inspection

    /// Structured diff between two refs, as a JSON string.
    public func diff(_ refA: String, _ refB: String) throws -> String {
        let r = try handle()
        let cA = sgDup(refA); defer { free(cA) }
        let cB = sgDup(refB); defer { free(cB) }
        return try consume(agentstategraph_diff(r, cA, cB), "diff")
    }

    /// Merge `source` into `target`. Returns the merge commit id.
    @discardableResult
    public func merge(source: String, target: String, description: String) throws -> String {
        let r = try handle()
        let cSrc = sgDup(source); defer { free(cSrc) }
        let cTgt = sgDup(target); defer { free(cTgt) }
        let cDesc = sgDup(description); defer { free(cDesc) }
        return try consume(agentstategraph_merge(r, cSrc, cTgt, cDesc), "merge")
    }

    /// Commit log for `ref`, most recent first, up to `limit` entries.
    public func log(limit: UInt32 = 50, ref: String = "main") throws -> [LogEntry] {
        let r = try handle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let raw = try consume(agentstategraph_log(r, cRef, limit), "log")
        return try decodeJSON(raw)
    }

    /// Who last modified `path`, and why — as a JSON string.
    public func blame(_ path: String, ref: String = "main") throws -> String {
        let r = try handle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        return try consume(agentstategraph_blame(r, cRef, cPath), "blame")
    }
}
