import Foundation
import CAgentStateGraph

public struct RepositoryCapabilities: Codable, Sendable, Equatable {
    public let contractVersion: Int
    public let operations: [String]
    enum CodingKeys: String, CodingKey {
        case contractVersion = "contract_version"
        case operations
    }
}

public struct MergePreview: Codable, Sendable, Equatable {
    public let fastForward: Bool
    public let added: [String]
    public let changed: [String]
    public let removed: [String]
    public let conflicts: [JSONValue]
    enum CodingKeys: String, CodingKey {
        case fastForward = "fast_forward"
        case added, changed, removed, conflicts
    }
}

public struct SearchResult: Codable, Sendable, Equatable {
    public let path: String
    public let value: String
}

public struct CommitQuery: Encodable, Sendable, Equatable {
    public var ref: String
    public var agentId: String?
    public var intentCategory: String?
    public var tags: [String]?
    public var reasoningContains: String?
    public var confidenceMin: Double?
    public var confidenceMax: Double?
    public var hasDeviations: Bool?
    public var limit: Int

    public init(ref: String = "main", agentId: String? = nil,
                intentCategory: String? = nil, tags: [String]? = nil,
                reasoningContains: String? = nil, confidenceMin: Double? = nil,
                confidenceMax: Double? = nil, hasDeviations: Bool? = nil,
                limit: Int = 50) {
        self.ref = ref; self.agentId = agentId; self.intentCategory = intentCategory
        self.tags = tags; self.reasoningContains = reasoningContains
        self.confidenceMin = confidenceMin; self.confidenceMax = confidenceMax
        self.hasDeviations = hasDeviations; self.limit = limit
    }

    enum CodingKeys: String, CodingKey {
        case ref, tags, limit
        case agentId = "agent_id"; case intentCategory = "intent_category"
        case reasoningContains = "reasoning_contains"
        case confidenceMin = "confidence_min"; case confidenceMax = "confidence_max"
        case hasDeviations = "has_deviations"
    }
}

public struct CommitQueryResult: Codable, Sendable, Equatable {
    public let id: String
    public let agentId: String
    public let intentCategory: String
    public let intentDescription: String
    public let tags: [String]
    public let reasoning: String?
    public let confidence: Double?
    public let parents: [String]
    public let timestamp: String
    enum CodingKeys: String, CodingKey {
        case id, tags, reasoning, confidence, parents, timestamp
        case agentId = "agent_id"; case intentCategory = "intent_category"
        case intentDescription = "intent_description"
    }
}

public struct SpeculationEntry: Codable, Sendable, Equatable {
    public let handle: UInt64
    public let label: String?
}

public struct SpeculationComparison: Codable, Sendable, Equatable {
    public let baseRef: String
    public let entries: [Entry]
    enum CodingKeys: String, CodingKey { case baseRef = "base_ref"; case entries }

    public struct Entry: Codable, Sendable, Equatable {
        public let handle: UInt64
        public let label: String?
        public let diff: [JSONValue]
    }
}

public enum SessionStatus: String, Codable, Sendable {
    case active = "Active"
    case completed = "Completed"
    case abandoned = "Abandoned"
}

public struct Session: Codable, Sendable, Equatable {
    public let id: String
    public let agentId: String
    public let workingBranch: String
    public let head: String
    public let parentSession: String?
    public let delegatedIntent: String?
    public let reportTo: String?
    public let pathScope: String?
    public let scopeTenant: String?
    public let scopeNamespace: String?
    public let status: SessionStatus
    public let createdAt: String
    public let endedAt: String?
    enum CodingKeys: String, CodingKey {
        case id, head, status
        case agentId = "agent_id"
        case workingBranch = "working_branch"
        case parentSession = "parent_session"
        case delegatedIntent = "delegated_intent"
        case reportTo = "report_to"
        case pathScope = "path_scope"
        case scopeTenant = "scope_tenant"
        case scopeNamespace = "scope_namespace"
        case createdAt = "created_at"
        case endedAt = "ended_at"
    }
}

public enum EpochStatus: String, Codable, Sendable {
    case active = "Active"
    case sealed = "Sealed"
    case archived = "Archived"
}

public struct EpochEntry: Codable, Sendable, Equatable {
    public let id: String
    public let description: String
    public let status: EpochStatus
    public let createdAt: String
    public let sealedAt: String?
    public let rootIntents: [String]
    public let agents: [String]
    public let commitCount: Int
    public let sealHash: String?
    public let tags: [String]
    enum CodingKeys: String, CodingKey {
        case id, description, status, agents, tags
        case createdAt = "created_at"
        case sealedAt = "sealed_at"
        case rootIntents = "root_intents"
        case commitCount = "commit_count"
        case sealHash = "seal_hash"
    }
}

public struct Epoch: Codable, Sendable, Equatable {
    public let id: String
    public let description: String
    public let rootIntents: [String]
    public let status: EpochStatus
    public let createdAt: String
    public let sealedAt: String?
    public let sealSummary: String?
    public let sealHash: String?
    public let commits: [String]
    public let agents: [String]
    public let branches: [String]
    public let tags: [String]
    public let sealedCommits: [String]
    enum CodingKeys: String, CodingKey {
        case id, description, status, commits, agents, branches, tags
        case rootIntents = "root_intents"
        case createdAt = "created_at"
        case sealedAt = "sealed_at"
        case sealSummary = "seal_summary"
        case sealHash = "seal_hash"
        case sealedCommits = "sealed_commits"
    }
}

private struct EmptyRequest: Encodable {}
private struct StringResponse: Decodable { let commit: String }
private struct BoolResponse: Decodable { let deleted: Bool }
private struct HeadResponse: Decodable { let head: String }
private struct NamespaceResponse: Decodable { let namespace: String }
private struct HandleResponse: Decodable { let handle: UInt64 }
private struct OptionalSessionResponse: Decodable { let session: String? }
private struct OptionalEpochResponse: Decodable { let epoch: String? }
private struct CASRequest<Value: Encodable>: Encodable {
    let path: String
    let value: Value
    let expectedHead: String
    let category: String
    let description: String
    let ref: String
    let agentId: String
    let reasoning: String?
    let confidence: Double?
    let tags: [String]?
    enum CodingKeys: String, CodingKey {
        case path, value, category, description, ref, reasoning, confidence, tags
        case expectedHead = "expected_head"
        case agentId = "agent_id"
    }
}
private struct SpecSetRequest<Value: Encodable>: Encodable {
    let handle: UInt64
    let path: String
    let value: Value
}

extension AgentStateGraph {
    public static func capabilities() throws -> RepositoryCapabilities {
        try decodeJSON(try consume(agentstategraph_repository_capabilities(), "repository_capabilities"))
    }

    private func advanced<Response: Decodable, Request: Encodable>(
        _ operation: String, _ request: Request, as type: Response.Type = Response.self
    ) throws -> Response {
        let cOperation = sgDup(operation); defer { free(cOperation) }
        let cRequest = sgDup(try encodeJSON(request)); defer { free(cRequest) }
        let raw = try consume(
            agentstategraph_repository_call(try rawHandle(), cOperation, cRequest), operation)
        return try decodeJSON(raw, as: type)
    }

    public func scoped(to namespace: String) throws -> AgentStateGraph {
        let cNamespace = sgDup(namespace); defer { free(cNamespace) }
        return try AgentStateGraph(repo: agentstategraph_fork_namespace(try rawHandle(), cNamespace))
    }

    public func currentNamespace() throws -> String {
        try advanced("namespace.current", EmptyRequest(), as: NamespaceResponse.self).namespace
    }

    public func createNamespace(_ name: String) throws {
        struct Request: Encodable { let name: String }
        let _: JSONValue = try advanced("namespace.create", Request(name: name))
    }

    public func listNamespaces() throws -> [String] {
        try advanced("namespace.list", EmptyRequest())
    }

    @discardableResult public func deleteNamespace(_ name: String) throws -> Bool {
        struct Request: Encodable { let name: String }
        return try advanced("namespace.delete", Request(name: name), as: BoolResponse.self).deleted
    }

    public func head(ref: String = "main") throws -> String {
        struct Request: Encodable { let ref: String }
        return try advanced("head", Request(ref: ref), as: HeadResponse.self).head
    }

    public func queryCommits(_ query: CommitQuery = CommitQuery()) throws -> [CommitQueryResult] {
        try advanced("query.commits", query)
    }

    @discardableResult public func setCAS<T: Encodable>(
        _ path: String, value: T, expectedHead: String,
        category: IntentCategory, description: String, ref: String = "main",
        agentId: String = "swift", reasoning: String? = nil,
        confidence: Double? = nil, tags: [String]? = nil
    ) throws -> String {
        let request = CASRequest(path: path, value: value, expectedHead: expectedHead,
                              category: category.rawValue, description: description,
                              ref: ref, agentId: agentId, reasoning: reasoning,
                              confidence: confidence, tags: tags)
        return try advanced("set_cas", request, as: StringResponse.self).commit
    }

    public func mergeBase(source: String, target: String) throws -> String {
        struct Request: Encodable { let source: String; let target: String }
        return try advanced("merge.base", Request(source: source, target: target),
                            as: StringResponse.self).commit
    }

    public func previewMerge(source: String, target: String) throws -> MergePreview {
        struct Request: Encodable { let source: String; let target: String }
        return try advanced("merge.preview", Request(source: source, target: target))
    }

    @discardableResult public func mergeChecked(
        source: String, target: String, description: String,
        allowDeletions: Bool = false, agentId: String = "swift"
    ) throws -> String {
        struct Request: Encodable {
            let source: String; let target: String; let description: String
            let allowDeletions: Bool; let agentId: String
            let category = "Merge"
            enum CodingKeys: String, CodingKey {
                case source, target, description, category
                case allowDeletions = "allow_deletions"; case agentId = "agent_id"
            }
        }
        return try advanced("merge.checked", Request(source: source, target: target,
            description: description, allowDeletions: allowDeletions, agentId: agentId),
            as: StringResponse.self).commit
    }

    public func listPaths(prefix: String = "/", maxDepth: Int? = nil,
                          ref: String = "main") throws -> [String] {
        struct Request: Encodable {
            let prefix: String; let maxDepth: Int?; let ref: String
            enum CodingKeys: String, CodingKey { case prefix, ref; case maxDepth = "max_depth" }
        }
        return try advanced("explore.list_paths", Request(prefix: prefix, maxDepth: maxDepth, ref: ref))
    }

    public func tree(prefix: String = "/", ref: String = "main") throws -> JSONValue {
        struct Request: Encodable { let prefix: String; let ref: String }
        return try advanced("explore.get_tree", Request(prefix: prefix, ref: ref))
    }

    public func search(_ query: String, maxResults: Int? = nil,
                       ref: String = "main") throws -> [SearchResult] {
        struct Request: Encodable {
            let query: String; let maxResults: Int?; let ref: String
            enum CodingKeys: String, CodingKey { case query, ref; case maxResults = "max_results" }
        }
        return try advanced("explore.search_values", Request(query: query,
            maxResults: maxResults, ref: ref))
    }

    public func stats(ref: String = "main") throws -> JSONValue {
        struct Request: Encodable { let ref: String }
        return try advanced("explore.stats", Request(ref: ref))
    }

    public func commitGraph(depth: Int = 100, ref: String = "main") throws -> [JSONValue] {
        struct Request: Encodable { let depth: Int; let ref: String }
        return try advanced("explore.commit_graph", Request(depth: depth, ref: ref))
    }

    public func intentTree(rootCommitId: String? = nil, ref: String = "main") throws -> JSONValue {
        struct Request: Encodable {
            let rootCommitId: String?; let ref: String
            enum CodingKeys: String, CodingKey { case ref; case rootCommitId = "root_commit_id" }
        }
        return try advanced("explore.intent_tree", Request(rootCommitId: rootCommitId, ref: ref))
    }

    public func speculate(from ref: String = "main", label: String? = nil) throws -> UInt64 {
        struct Request: Encodable { let ref: String; let label: String? }
        return try advanced("spec.create", Request(ref: ref, label: label), as: HandleResponse.self).handle
    }

    public func setSpeculation<T: Encodable>(_ handle: UInt64, path: String, value: T) throws {
        let _: JSONValue = try advanced("spec.set", SpecSetRequest(handle: handle, path: path, value: value))
    }

    public func deleteSpeculation(_ handle: UInt64, path: String) throws {
        struct Request: Encodable { let handle: UInt64; let path: String }
        let _: JSONValue = try advanced("spec.delete", Request(handle: handle, path: path))
    }

    public func compareSpeculations(_ handles: [UInt64]) throws -> SpeculationComparison {
        struct Request: Encodable { let handles: [UInt64] }
        return try advanced("spec.compare", Request(handles: handles))
    }

    @discardableResult public func commitSpeculation(
        _ handle: UInt64, category: IntentCategory, description: String,
        agentId: String = "swift", reasoning: String? = nil,
        confidence: Double? = nil, tags: [String]? = nil
    ) throws -> String {
        struct Request: Encodable {
            let handle: UInt64; let category: String; let description: String; let agentId: String
            let reasoning: String?; let confidence: Double?; let tags: [String]?
            enum CodingKeys: String, CodingKey {
                case handle, category, description, reasoning, confidence, tags
                case agentId = "agent_id"
            }
        }
        return try advanced("spec.commit", Request(handle: handle, category: category.rawValue,
            description: description, agentId: agentId, reasoning: reasoning,
            confidence: confidence, tags: tags), as: StringResponse.self).commit
    }

    public func discardSpeculation(_ handle: UInt64) throws {
        struct Request: Encodable { let handle: UInt64 }
        let _: JSONValue = try advanced("spec.discard", Request(handle: handle))
    }

    public func listSpeculations() throws -> [SpeculationEntry] {
        try advanced("spec.list", EmptyRequest())
    }

    public func createSession(
        agentId: String, workingBranch: String = "main", parentSession: String? = nil,
        delegatedIntent: String? = nil, reportTo: String? = nil,
        pathScope: String? = nil, scopeNamespace: String? = nil
    ) throws -> Session {
        struct Request: Encodable {
            let agentId: String; let workingBranch: String; let parentSession: String?
            let delegatedIntent: String?; let reportTo: String?; let pathScope: String?
            let scopeNamespace: String?
            enum CodingKeys: String, CodingKey {
                case agentId = "agent_id"; case workingBranch = "working_branch"
                case parentSession = "parent_session"; case delegatedIntent = "delegated_intent"
                case reportTo = "report_to"; case pathScope = "path_scope"
                case scopeNamespace = "scope_namespace"
            }
        }
        return try advanced("session.create", Request(agentId: agentId,
            workingBranch: workingBranch, parentSession: parentSession,
            delegatedIntent: delegatedIntent, reportTo: reportTo,
            pathScope: pathScope, scopeNamespace: scopeNamespace))
    }

    public func session(id: String) throws -> Session? {
        struct Request: Encodable { let id: String }
        return try advanced("session.get", Request(id: id))
    }

    public func sessions(agentId: String? = nil) throws -> [Session] {
        struct Request: Encodable {
            let agentId: String?
            enum CodingKeys: String, CodingKey { case agentId = "agent_id" }
        }
        return try advanced("session.list", Request(agentId: agentId))
    }

    public func childSessions(parentId: String) throws -> [Session] {
        struct Request: Encodable {
            let parentId: String
            enum CodingKeys: String, CodingKey { case parentId = "parent_id" }
        }
        return try advanced("session.children", Request(parentId: parentId))
    }

    public func updateSession(_ id: String, head: String) throws {
        struct Request: Encodable { let id: String; let head: String }
        let _: JSONValue = try advanced("session.update_head", Request(id: id, head: head))
    }

    public func endSession(_ id: String, status: SessionStatus) throws {
        guard status != .active else {
            throw AgentStateGraphError.native("session end status cannot be Active")
        }
        struct Request: Encodable { let id: String; let status: String }
        let _: JSONValue = try advanced("session.end", Request(id: id, status: status.rawValue))
    }

    public func activeSession() throws -> String? {
        try advanced("session.active.get", EmptyRequest(), as: OptionalSessionResponse.self).session
    }

    public func setActiveSession(_ id: String?) throws {
        struct Request: Encodable { let id: String? }
        let _: JSONValue = try advanced("session.active.set", Request(id: id))
    }

    @discardableResult public func createEpoch(
        id: String, description: String, rootIntents: [String] = []
    ) throws -> Epoch {
        struct Request: Encodable {
            let id: String; let description: String; let rootIntents: [String]
            enum CodingKeys: String, CodingKey { case id, description; case rootIntents = "root_intents" }
        }
        return try advanced("epoch.create", Request(id: id, description: description,
                                                     rootIntents: rootIntents))
    }

    public func epoch(id: String) throws -> Epoch {
        struct Request: Encodable { let id: String }
        return try advanced("epoch.get", Request(id: id))
    }

    public func epochs() throws -> [EpochEntry] { try advanced("epoch.list", EmptyRequest()) }

    public func sealEpoch(_ id: String, summary: String) throws {
        struct Request: Encodable { let id: String; let summary: String }
        let _: JSONValue = try advanced("epoch.seal", Request(id: id, summary: summary))
    }

    public func archiveEpoch(_ id: String) throws {
        struct Request: Encodable { let id: String }
        let _: JSONValue = try advanced("epoch.archive", Request(id: id))
    }

    public func exportEpoch(_ id: String) throws -> JSONValue {
        struct Request: Encodable { let id: String }
        return try advanced("epoch.export", Request(id: id))
    }

    public func activeEpoch() throws -> String? {
        try advanced("epoch.active.get", EmptyRequest(), as: OptionalEpochResponse.self).epoch
    }

    public func setActiveEpoch(_ id: String?) throws {
        struct Request: Encodable { let id: String? }
        let _: JSONValue = try advanced("epoch.active.set", Request(id: id))
    }
}
