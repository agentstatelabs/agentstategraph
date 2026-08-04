import Foundation
import CAgentStateGraph

/// Advisory severity on a `Policy`. Metadata only — does not change
/// decision semantics.
public enum Severity: String, Codable, Sendable {
    case low, medium, high, critical
}

/// One allow / deny rule.
public struct AuthorizedAction: Codable, Sendable, Equatable {
    public var action: String
    public var condition: String?
    public var preconditions: [String]?

    public init(action: String, condition: String? = nil, preconditions: [String]? = nil) {
        self.action = action
        self.condition = condition
        self.preconditions = preconditions
    }
}

/// One require-approval rule. `fallback` is left as arbitrary JSON
/// (variant-tagged `{"kind": …}`) so callers decode the variant they need.
public struct ApprovalRule: Codable, Sendable, Equatable {
    public var action: String
    public var approvers: [String]
    /// Duration encoded as milliseconds.
    public var timeout: UInt64?
    public var fallback: JSONValue?

    public init(action: String, approvers: [String], timeout: UInt64? = nil, fallback: JSONValue? = nil) {
        self.action = action
        self.approvers = approvers
        self.timeout = timeout
        self.fallback = fallback
    }
}

/// One step in a policy's procedure.
public struct ProcedureStep: Codable, Sendable, Equatable {
    public var action: String
    public var ifPreviousFailed: String?

    enum CodingKeys: String, CodingKey {
        case action
        case ifPreviousFailed = "if_previous_failed"
    }

    public init(action: String, ifPreviousFailed: String? = nil) {
        self.action = action
        self.ifPreviousFailed = ifPreviousFailed
    }
}

/// Detached signature metadata recorded when a policy is signed.
public struct PolicySignature: Codable, Sendable, Equatable {
    public let algorithm: String
    public let signerKeyId: String
    public let signatureHex: String

    enum CodingKeys: String, CodingKey {
        case algorithm
        case signerKeyId = "signer_key_id"
        case signatureHex = "signature_hex"
    }
}

/// Where an external evaluator's body comes from. `kind` is one of
/// `inline` / `file_path` / `commit_ref`.
public struct EvaluatorSource: Codable, Sendable, Equatable {
    public var kind: String
    public var body: String?
    public var path: String?

    public init(kind: String, body: String? = nil, path: String? = nil) {
        self.kind = kind
        self.body = body
        self.path = path
    }
}

/// Reference to an external evaluator engine (`rego` / `cedar` / `wasm`).
public struct ExternalEvaluatorRef: Codable, Sendable, Equatable {
    public var kind: String
    public var source: EvaluatorSource

    public init(kind: String, source: EvaluatorSource) {
        self.kind = kind
        self.source = source
    }
}

/// The unit of authorization + procedure. Mirrors
/// `agentstategraph-policy::Policy`.
public struct Policy: Codable, Sendable, Equatable {
    public var path: String
    public var version: UInt64
    public var situation: String
    public var situationSelector: JSONValue?
    public var allow: [AuthorizedAction]?
    public var deny: [AuthorizedAction]?
    public var requireApproval: [ApprovalRule]?
    public var procedure: [ProcedureStep]?
    public var triggers: [String]?
    public var requiredFields: [String]?
    public var severity: Severity
    public var proposedBy: String
    public var proposedAt: String
    public var ratifiedBy: String?
    public var ratifiedAt: String?
    public var ratificationReasoning: String?
    public var activeFrom: String
    public var expiresAt: String?
    public var supersedes: String?
    public var signature: PolicySignature?
    public var tenantId: String?
    public var externalEvaluator: ExternalEvaluatorRef?

    enum CodingKeys: String, CodingKey {
        case path, version, situation, allow, deny, procedure, triggers, severity, supersedes, signature
        case situationSelector = "situation_selector"
        case requireApproval = "require_approval"
        case requiredFields = "required_fields"
        case proposedBy = "proposed_by"
        case proposedAt = "proposed_at"
        case ratifiedBy = "ratified_by"
        case ratifiedAt = "ratified_at"
        case ratificationReasoning = "ratification_reasoning"
        case activeFrom = "active_from"
        case expiresAt = "expires_at"
        case tenantId = "tenant_id"
        case externalEvaluator = "external_evaluator"
    }
}

/// One of the four `Decision` variants.
public enum DecisionKind: String, Codable, Sendable {
    case allow
    case deny
    case requireApproval = "require_approval"
    case noPolicyMatch = "no_policy_match"
}

/// Result of `evaluate` / `evaluateChange`. Fields are the union of every
/// variant; consult `kind` first, then read only the relevant fields.
public struct Decision: Codable, Sendable, Equatable {
    public let kind: DecisionKind
    public let matchedPolicy: String?
    public let reason: String?
    public let preconditions: [String]?
    public let approvers: [String]?
    public let timeout: UInt64?
    public let fallback: JSONValue?
    public let approvalTaskPath: String?

    enum CodingKeys: String, CodingKey {
        case kind, reason, preconditions, approvers, timeout, fallback
        case matchedPolicy = "matched_policy"
        case approvalTaskPath = "approval_task_path"
    }
}

/// A proposed change evaluated against change-cost policies.
public struct ChangeProposal: Codable, Sendable, Equatable {
    public var action: String
    public var agentId: String
    public var intent: String
    public var preferredOption: String
    public var alternatives: [String]?
    public var tokens: [String]?
    public var attachedFields: [String: String]?

    enum CodingKeys: String, CodingKey {
        case action, intent, alternatives, tokens
        case agentId = "agent_id"
        case preferredOption = "preferred_option"
        case attachedFields = "attached_fields"
    }

    public init(
        action: String, agentId: String, intent: String, preferredOption: String,
        alternatives: [String]? = nil, tokens: [String]? = nil,
        attachedFields: [String: String]? = nil
    ) {
        self.action = action
        self.agentId = agentId
        self.intent = intent
        self.preferredOption = preferredOption
        self.alternatives = alternatives
        self.tokens = tokens
        self.attachedFields = attachedFields
    }
}

/// A handle bound to a repository, path prefix, and agent id. All policy
/// writes commit under the `Plan` intent category. The repository is
/// shared (refcounted): closing a `PolicyStore` does not close it.
public final class PolicyStore {
    private var handle: UnsafeMutableRawPointer?
    private let repo: AgentStateGraph

    /// Create a policy store over an existing repository.
    public init(_ repo: AgentStateGraph, prefix: String, agentId: String) throws {
        self.repo = repo
        let r = try repo.rawHandle()
        let cPrefix = sgDup(prefix); defer { free(cPrefix) }
        let cAgent = sgDup(agentId); defer { free(cAgent) }
        guard let h = agentstategraph_policy_store_new(r, cPrefix, cAgent) else {
            throw AgentStateGraphError.operationFailed("create policy store")
        }
        self.handle = h
    }

    deinit { close() }

    /// Free the policy store handle. The repository is unaffected. Idempotent.
    public func close() {
        if let h = handle {
            agentstategraph_policy_store_free(h)
            handle = nil
        }
    }

    private func h() throws -> UnsafeMutableRawPointer {
        guard let h = handle else { throw AgentStateGraphError.closed("policy store") }
        return h
    }

    // MARK: - Write

    /// Register a new (unratified) policy; returns its `path@version` handle.
    @discardableResult
    public func propose(_ policy: Policy, ref: String = "main") throws -> String {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPolicy = sgDup(try encodeJSON(policy)); defer { free(cPolicy) }
        let raw = try consume(agentstategraph_policy_propose(s, cRef, cPolicy), "propose")
        return try decodeJSON(raw)
    }

    /// Promote an unratified proposal. `reasoning` must be non-empty.
    public func ratify(path: String, ratifier: String, reasoning: String, ref: String = "main") throws {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cRat = sgDup(ratifier); defer { free(cRat) }
        let cReason = sgDup(reasoning); defer { free(cReason) }
        _ = try consume(agentstategraph_policy_ratify(s, cRef, cPath, cRat, cReason), "ratify")
    }

    /// Replace the active policy at `path`; returns the new handle.
    @discardableResult
    public func supersede(path: String, with newPolicy: Policy, ref: String = "main") throws -> String {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cPolicy = sgDup(try encodeJSON(newPolicy)); defer { free(cPolicy) }
        let raw = try consume(agentstategraph_policy_supersede(s, cRef, cPath, cPolicy), "supersede")
        return try decodeJSON(raw)
    }

    // MARK: - Read

    /// Every policy under `prefix` (or all when `nil`), including proposals.
    public func list(prefix: String? = nil, ref: String = "main") throws -> [Policy] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPrefix = sgDup(prefix); defer { free(cPrefix) }
        return try decodeJSON(try consume(agentstategraph_policy_list(s, cRef, cPrefix), "list"))
    }

    /// Currently-active policies (ratified and `active_from <= now`).
    public func active(prefix: String? = nil, ref: String = "main") throws -> [Policy] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPrefix = sgDup(prefix); defer { free(cPrefix) }
        return try decodeJSON(try consume(agentstategraph_policy_active(s, cRef, cPrefix), "active"))
    }

    /// The active (or latest proposed) policy at `path`.
    public func get(path: String, ref: String = "main") throws -> Policy {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        return try decodeJSON(try consume(agentstategraph_policy_get(s, cRef, cPath), "get"))
    }

    /// The supersedes chain for `path`, oldest-first through the current version.
    public func history(path: String, ref: String = "main") throws -> [Policy] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        return try decodeJSON(try consume(agentstategraph_policy_history(s, cRef, cPath), "history"))
    }

    /// Active policies whose `triggers` intersect `tokens`.
    public func checkTokens(_ tokens: [String], ref: String = "main") throws -> [Policy] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cTokens = sgDup(try encodeJSON(tokens)); defer { free(cTokens) }
        return try decodeJSON(try consume(agentstategraph_policy_check_tokens(s, cRef, cTokens), "check_tokens"))
    }

    // MARK: - Evaluate

    /// Run the authorization evaluator.
    public func evaluate(
        situation: [String: String], action: String, agentId: String, ref: String = "main"
    ) throws -> Decision {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cSit = sgDup(try encodeJSON(situation)); defer { free(cSit) }
        let cAction = sgDup(action); defer { free(cAction) }
        let cAgent = sgDup(agentId); defer { free(cAgent) }
        let raw = try consume(
            agentstategraph_policy_evaluate(s, cRef, cSit, cAction, cAgent), "evaluate")
        return try decodeJSON(raw)
    }

    /// Run the change-proposal evaluator.
    public func evaluateChange(_ proposal: ChangeProposal, ref: String = "main") throws -> Decision {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cProp = sgDup(try encodeJSON(proposal)); defer { free(cProp) }
        let raw = try consume(
            agentstategraph_policy_evaluate_change(s, cRef, cProp), "evaluate_change")
        return try decodeJSON(raw)
    }

    /// Tenant-scoped `evaluate`. `tenantFilter == nil` behaves like `evaluate`.
    public func evaluateScoped(
        situation: [String: String], action: String, agentId: String,
        tenantFilter: String?, ref: String = "main"
    ) throws -> Decision {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cSit = sgDup(try encodeJSON(situation)); defer { free(cSit) }
        let cAction = sgDup(action); defer { free(cAction) }
        let cAgent = sgDup(agentId); defer { free(cAgent) }
        let cTenant = sgDup(tenantFilter); defer { free(cTenant) }
        let raw = try consume(
            agentstategraph_policy_evaluate_scoped(s, cRef, cSit, cAction, cAgent, cTenant),
            "evaluate_scoped")
        return try decodeJSON(raw)
    }

    /// Tenant-scoped `evaluateChange`.
    public func evaluateChangeScoped(
        _ proposal: ChangeProposal, tenantFilter: String?, ref: String = "main"
    ) throws -> Decision {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cProp = sgDup(try encodeJSON(proposal)); defer { free(cProp) }
        let cTenant = sgDup(tenantFilter); defer { free(cTenant) }
        let raw = try consume(
            agentstategraph_policy_evaluate_change_scoped(s, cRef, cProp, cTenant),
            "evaluate_change_scoped")
        return try decodeJSON(raw)
    }

    /// `active`, dropping policies whose `tenantId` is set and mismatched.
    public func activeScoped(prefix: String? = nil, tenantFilter: String?, ref: String = "main") throws -> [Policy] {
        filterByTenant(try active(prefix: prefix, ref: ref), tenantFilter)
    }

    /// `list`, dropping policies whose `tenantId` is set and mismatched.
    public func listScoped(prefix: String? = nil, tenantFilter: String?, ref: String = "main") throws -> [Policy] {
        filterByTenant(try list(prefix: prefix, ref: ref), tenantFilter)
    }

    // MARK: - Signing / external evaluator (raw JSON envelopes)

    /// Invoke the `policy_sign` extern; returns the raw JSON envelope.
    public func sign(path: String, signerKeyId: String? = nil, ref: String = "main") throws -> String {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        let cKey = sgDup(signerKeyId); defer { free(cKey) }
        return try consume(agentstategraph_policy_sign(s, cRef, cPath, cKey), "policy_sign")
    }

    /// Invoke the `policy_verify` extern; returns the raw JSON envelope.
    public func verify(path: String, ref: String = "main") throws -> String {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPath = sgDup(path); defer { free(cPath) }
        return try consume(agentstategraph_policy_verify(s, cRef, cPath), "policy_verify")
    }

    /// Configure an external evaluator; returns the raw JSON envelope.
    public func setExternalEvaluator(configJSON: String) throws -> String {
        let s = try h()
        let cCfg = sgDup(configJSON); defer { free(cCfg) }
        return try consume(agentstategraph_policy_set_external_evaluator(s, cCfg), "policy_set_external_evaluator")
    }

    private func filterByTenant(_ policies: [Policy], _ filter: String?) -> [Policy] {
        guard let filter = filter else { return policies }
        return policies.filter { $0.tenantId == nil || $0.tenantId == filter }
    }
}
