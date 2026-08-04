import Foundation
import CAgentStateGraph

/// Kind of protective marker: `taint` / `quarantine` / `watch`.
public enum TaintKind: String, Codable, Sendable {
    case taint, quarantine, watch
}

/// Pre-commit-hook behavior of a taint.
public enum TaintEffect: String, Codable, Sendable {
    case warn, block, review, isolate, advisory
}

/// Advisory severity of a taint.
public enum TaintSeverity: String, Codable, Sendable {
    case low, medium, high, critical
}

/// Threshold direction for a watch: `above` / `below`.
public enum WatchDirection: String, Codable, Sendable {
    case above, below
}

/// The over-the-wire shape of a single taint record.
public struct Taint: Codable, Sendable, Equatable {
    public let id: String
    public let path: String
    public let name: String
    public let kind: TaintKind
    public let effect: TaintEffect
    public let severity: TaintSeverity
    public let reason: String
    public let agentId: String
    public let commitId: String
    public let createdAt: String
    public let expiresAt: String?
    public let resolvedAt: String?
    public let resolvedBy: String?
    public let resolvedReason: String?
    public let resolvedProof: String?
    public let propagate: Bool
    public let metadata: [String: JSONValue]?

    enum CodingKeys: String, CodingKey {
        case id, path, name, kind, effect, severity, reason, propagate, metadata
        case agentId = "agent_id"
        case commitId = "commit_id"
        case createdAt = "created_at"
        case expiresAt = "expires_at"
        case resolvedAt = "resolved_at"
        case resolvedBy = "resolved_by"
        case resolvedReason = "resolved_reason"
        case resolvedProof = "resolved_proof"
    }
}

/// Aggregated status returned by `checkTaint`.
public struct TaintCheck: Codable, Sendable, Equatable {
    public let tainted: Bool
    public let quarantined: Bool
    public let watched: Bool
    public let taints: [Taint]
    public let quarantines: [Taint]
    public let watches: [Taint]
    public let canWrite: Bool
    public let requiredConfidence: Double
    public let authorizedAgents: [String]?
    public let isolated: Bool

    enum CodingKeys: String, CodingKey {
        case tainted, quarantined, watched, taints, quarantines, watches, isolated
        case canWrite = "can_write"
        case requiredConfidence = "required_confidence"
        case authorizedAgents = "authorized_agents"
    }
}

// MARK: - Parameter shapes
//
// Field names match the FFI param parser. Note the FFI reads `expires`
// (RFC3339), not `expires_at`.

/// Input to `taint`.
public struct TaintParams: Codable, Sendable {
    public var name: String
    public var effect: TaintEffect
    public var reason: String
    public var severity: TaintSeverity?
    public var expires: String?
    public var propagate: Bool?
    public var agentId: String

    enum CodingKeys: String, CodingKey {
        case name, effect, reason, severity, expires, propagate
        case agentId = "agent_id"
    }

    public init(
        name: String, effect: TaintEffect, reason: String, severity: TaintSeverity? = nil,
        expires: String? = nil, propagate: Bool? = nil, agentId: String
    ) {
        self.name = name; self.effect = effect; self.reason = reason
        self.severity = severity; self.expires = expires; self.propagate = propagate
        self.agentId = agentId
    }
}

/// Input to `quarantine`.
public struct QuarantineParams: Codable, Sendable {
    public var name: String
    public var reason: String
    public var severity: TaintSeverity?
    public var authorizedAgents: [String]
    public var expires: String?
    public var propagate: Bool?
    public var agentId: String

    enum CodingKeys: String, CodingKey {
        case name, reason, severity, expires, propagate
        case authorizedAgents = "authorized_agents"
        case agentId = "agent_id"
    }

    public init(
        name: String, reason: String, severity: TaintSeverity? = nil,
        authorizedAgents: [String] = [], expires: String? = nil, propagate: Bool? = nil,
        agentId: String
    ) {
        self.name = name; self.reason = reason; self.severity = severity
        self.authorizedAgents = authorizedAgents; self.expires = expires
        self.propagate = propagate; self.agentId = agentId
    }
}

/// Input to `watch`.
public struct WatchParams: Codable, Sendable {
    public var name: String
    public var reason: String
    public var metric: String?
    public var threshold: Double?
    public var direction: WatchDirection?
    public var checkIntervalSecs: UInt64?
    public var expires: String?
    public var severity: TaintSeverity?
    public var propagate: Bool?
    public var agentId: String

    enum CodingKeys: String, CodingKey {
        case name, reason, metric, threshold, direction, expires, severity, propagate
        case checkIntervalSecs = "check_interval_secs"
        case agentId = "agent_id"
    }

    public init(
        name: String, reason: String, metric: String? = nil, threshold: Double? = nil,
        direction: WatchDirection? = nil, checkIntervalSecs: UInt64? = nil, expires: String? = nil,
        severity: TaintSeverity? = nil, propagate: Bool? = nil, agentId: String
    ) {
        self.name = name; self.reason = reason; self.metric = metric; self.threshold = threshold
        self.direction = direction; self.checkIntervalSecs = checkIntervalSecs; self.expires = expires
        self.severity = severity; self.propagate = propagate; self.agentId = agentId
    }
}

/// Input to `untaint` / `unquarantine`. `name` is supplied via the method
/// argument; this carries the rest.
public struct UntaintParams: Codable, Sendable {
    public var reason: String
    public var proof: String?
    public var agentId: String

    enum CodingKeys: String, CodingKey {
        case reason, proof
        case agentId = "agent_id"
    }

    public init(reason: String, proof: String? = nil, agentId: String) {
        self.reason = reason; self.proof = proof; self.agentId = agentId
    }
}

/// Input to `unwatch`. Watches are lightweight, so `reason` is optional.
public struct UnwatchParams: Codable, Sendable {
    public var reason: String?
    public var agentId: String

    enum CodingKeys: String, CodingKey {
        case reason
        case agentId = "agent_id"
    }

    public init(reason: String? = nil, agentId: String) {
        self.reason = reason; self.agentId = agentId
    }
}

// MARK: - Repository extensions

extension AgentStateGraph {
    /// Apply a taint on `path`. Returns the new taint's uuid.
    @discardableResult
    public func taint(_ path: String, params: TaintParams, ref: String = "main") throws -> String {
        try decodeApply(applyTaint(agentstategraph_taint_apply, path, try encodeJSON(params), ref, "taint_apply"))
    }

    /// Remove an active taint by name.
    public func untaint(_ path: String, name: String, params: UntaintParams, ref: String = "main") throws {
        let payload = try mergeName(params, name: name)
        _ = try decodeOK(applyTaint(agentstategraph_taint_remove, path, payload, ref, "taint_remove"))
    }

    /// Apply a quarantine on `path`. Returns the new taint id.
    @discardableResult
    public func quarantine(_ path: String, params: QuarantineParams, ref: String = "main") throws -> String {
        try decodeApply(applyTaint(agentstategraph_quarantine_apply, path, try encodeJSON(params), ref, "quarantine_apply"))
    }

    /// Release an active quarantine by name.
    public func unquarantine(_ path: String, name: String, params: UntaintParams, ref: String = "main") throws {
        let payload = try mergeName(params, name: name)
        _ = try decodeOK(applyTaint(agentstategraph_quarantine_release, path, payload, ref, "quarantine_release"))
    }

    /// Attach a watch to `path`. Returns the new taint id.
    @discardableResult
    public func watch(_ path: String, params: WatchParams, ref: String = "main") throws -> String {
        try decodeApply(applyTaint(agentstategraph_watch_apply, path, try encodeJSON(params), ref, "watch_apply"))
    }

    /// Remove an active watch by name.
    public func unwatch(_ path: String, name: String, params: UnwatchParams, ref: String = "main") throws {
        let payload = try mergeName(params, name: name)
        _ = try decodeOK(applyTaint(agentstategraph_watch_remove, path, payload, ref, "watch_remove"))
    }

    /// Every active taint (or all, if `includeResolved`). Both filters optional.
    public func listTaints(
        pathPrefix: String? = nil, kind: String? = nil, includeResolved: Bool = false
    ) throws -> [Taint] {
        let r = try rawHandle()
        let cPrefix = sgDup(pathPrefix); defer { free(cPrefix) }
        let cKind = sgDup(kind); defer { free(cKind) }
        let raw = try consume(
            agentstategraph_list_taints(r, cPrefix, cKind, includeResolved), "list_taints")
        struct Env: Decodable { let taints: [Taint] }
        return try decodeJSON(raw, as: Env.self).taints
    }

    /// Aggregated taint / quarantine / watch status for `path`.
    public func checkTaint(_ path: String, agentId: String, confidence: Double) throws -> TaintCheck {
        let r = try rawHandle()
        let cPath = sgDup(path); defer { free(cPath) }
        let cAgent = sgDup(agentId); defer { free(cAgent) }
        let raw = try consume(
            agentstategraph_check_taint(r, cPath, cAgent, confidence), "check_taint")
        struct Env: Decodable { let check: TaintCheck? }
        guard let c = try decodeJSON(raw, as: Env.self).check else {
            throw AgentStateGraphError.decode("check_taint: missing check envelope")
        }
        return c
    }

    // MARK: internals

    private func applyTaint(
        _ fn: (UnsafeMutableRawPointer?, UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
        _ path: String, _ paramsJSON: String, _ ref: String, _ op: String
    ) throws -> String {
        let r = try rawHandle()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cParams = sgDup(paramsJSON); defer { free(cParams) }
        return try consume(fn(r, cRef, cPath, cParams), op)
    }

    /// The `{"ok":true,"id":"<uuid>"}` envelope from *_apply.
    private func decodeApply(_ raw: String) throws -> String {
        struct Env: Decodable { let ok: Bool; let id: String? }
        let env = try decodeJSON(raw, as: Env.self)
        guard env.ok, let id = env.id else {
            throw AgentStateGraphError.native(raw)
        }
        return id
    }

    /// The `{"ok":true}` envelope from *_remove / release / unwatch.
    private func decodeOK(_ raw: String) throws {
        struct Env: Decodable { let ok: Bool }
        guard try decodeJSON(raw, as: Env.self).ok else {
            throw AgentStateGraphError.native(raw)
        }
    }

    /// Re-serialize an `Encodable` params payload with an injected `name`,
    /// which the FFI param parser reads from the JSON body.
    private func mergeName<T: Encodable>(_ params: T, name: String) throws -> String {
        let data = try JSONEncoder().encode(params)
        var obj = (try JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        obj["name"] = name
        let merged = try JSONSerialization.data(withJSONObject: obj)
        guard let s = String(data: merged, encoding: .utf8) else {
            throw AgentStateGraphError.decode("merge name: not utf-8")
        }
        return s
    }
}
