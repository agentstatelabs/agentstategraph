import Foundation
import CAgentStateGraph

/// Task urgency, ordered `low < medium < high < critical`.
public enum Priority: String, Codable, Sendable {
    case low, medium, high, critical
}

/// Lifecycle state of a task.
public enum TaskStatus: String, Codable, Sendable {
    case pending
    case inProgress = "in_progress"
    case done
    case abandoned
}

/// Lifecycle state of a plan.
public enum PlanStatus: String, Codable, Sendable {
    case active, completed, archived
}

/// Category of evidence attached to a completed task.
public enum ProofKind: String, Codable, Sendable {
    case commit, file, test, text
}

/// Evidence attached to a `done` task.
public struct Proof: Codable, Sendable, Equatable {
    public var kind: ProofKind
    public var value: String
    public var note: String?

    public init(kind: ProofKind, value: String, note: String? = nil) {
        self.kind = kind
        self.value = value
        self.note = note
    }
}

/// A named container of tasks.
public struct Plan: Codable, Sendable, Equatable {
    public let name: String
    public let description: String?
    public let status: PlanStatus
    public let createdAt: String
    public let createdBy: String
    public let archivedAt: String?

    enum CodingKeys: String, CodingKey {
        case name, description, status
        case createdAt = "created_at"
        case createdBy = "created_by"
        case archivedAt = "archived_at"
    }
}

/// A unit of work within a plan.
public struct Task: Codable, Sendable, Equatable {
    public let id: String
    public let title: String
    public let status: TaskStatus
    public let priority: Priority
    public let parentId: String?
    public let blockedBy: [String]?
    public let createdAt: String
    public let createdBy: String
    public let startedAt: String?
    public let startedBy: String?
    public let completedAt: String?
    public let completedBy: String?
    public let proof: Proof?
    public let abandonedAt: String?
    public let abandonedReason: String?
    public let assignedTo: String?
    public let payload: JSONValue?
    public let parentChange: String?
    public let onComplete: JSONValue?

    enum CodingKeys: String, CodingKey {
        case id, title, status, priority, proof, payload
        case parentId = "parent_id"
        case blockedBy = "blocked_by"
        case createdAt = "created_at"
        case createdBy = "created_by"
        case startedAt = "started_at"
        case startedBy = "started_by"
        case completedAt = "completed_at"
        case completedBy = "completed_by"
        case abandonedAt = "abandoned_at"
        case abandonedReason = "abandoned_reason"
        case assignedTo = "assigned_to"
        case parentChange = "parent_change"
        case onComplete = "on_complete"
    }
}

/// Optional fields for `addTask`.
public struct AddTaskOptions: Sendable {
    public var parentId: String?
    public var blockers: [String]?
    public var assignedTo: String?

    public init(parentId: String? = nil, blockers: [String]? = nil, assignedTo: String? = nil) {
        self.parentId = parentId
        self.blockers = blockers
        self.assignedTo = assignedTo
    }
}

/// Optional fields for `addTask(withExtensions:)` — `AddTaskOptions` plus
/// the extended task fields (`payload`, `parentChange`, `onComplete`).
public struct AddTaskExtOptions: Sendable {
    public var base: AddTaskOptions
    public var payload: JSONValue?
    public var parentChange: String?
    public var onComplete: JSONValue?

    public init(
        base: AddTaskOptions = AddTaskOptions(),
        payload: JSONValue? = nil,
        parentChange: String? = nil,
        onComplete: JSONValue? = nil
    ) {
        self.base = base
        self.payload = payload
        self.parentChange = parentChange
        self.onComplete = onComplete
    }
}

/// A handle bound to a repository, path prefix, and agent id. All plan and
/// task writes commit under the `Plan` intent category.
///
/// The repository is shared (refcounted): closing a `TaskStore` does **not**
/// close its `AgentStateGraph`.
public final class TaskStore {
    private var handle: UnsafeMutableRawPointer?
    // Keep the repo alive for at least as long as the store.
    private let repo: AgentStateGraph

    /// Create a task store over an existing repository.
    public init(_ repo: AgentStateGraph, prefix: String, agentId: String) throws {
        self.repo = repo
        let r = try repo.rawHandle()
        let cPrefix = sgDup(prefix); defer { free(cPrefix) }
        let cAgent = sgDup(agentId); defer { free(cAgent) }
        guard let h = agentstategraph_taskstore_new(r, cPrefix, cAgent) else {
            throw AgentStateGraphError.operationFailed("create task store")
        }
        self.handle = h
    }

    deinit { close() }

    /// Free the task store handle. The repository is unaffected. Idempotent.
    public func close() {
        if let h = handle {
            agentstategraph_taskstore_free(h)
            handle = nil
        }
    }

    private func h() throws -> UnsafeMutableRawPointer {
        guard let h = handle else { throw AgentStateGraphError.closed("task store") }
        return h
    }

    // MARK: - Plans

    /// Create a new plan under this store's prefix.
    @discardableResult
    public func createPlan(_ name: String, description: String? = nil, ref: String = "main") throws -> Plan {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cName = sgDup(name); defer { free(cName) }
        let cDesc = sgDup(description); defer { free(cDesc) }
        let raw = try consume(agentstategraph_taskstore_create_plan(s, cRef, cName, cDesc), "create_plan")
        return try decodeJSON(raw)
    }

    /// Every plan under the store's prefix.
    public func listPlans(ref: String = "main") throws -> [Plan] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        return try decodeJSON(try consume(agentstategraph_taskstore_list_plans(s, cRef), "list_plans"))
    }

    /// Plans filtered by status; pass `nil` for all.
    public func listPlans(status: PlanStatus?, ref: String = "main") throws -> [Plan] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cStatus = sgDup(status?.rawValue); defer { free(cStatus) }
        let raw = try consume(
            agentstategraph_taskstore_list_plans_by_status(s, cRef, cStatus), "list_plans_by_status")
        return try decodeJSON(raw)
    }

    /// Fetch a plan by name.
    public func getPlan(_ name: String, ref: String = "main") throws -> Plan {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cName = sgDup(name); defer { free(cName) }
        return try decodeJSON(try consume(agentstategraph_taskstore_get_plan(s, cRef, cName), "get_plan"))
    }

    /// Soft-archive a plan.
    @discardableResult
    public func archivePlan(_ name: String, ref: String = "main") throws -> Plan {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cName = sgDup(name); defer { free(cName) }
        return try decodeJSON(try consume(agentstategraph_taskstore_archive_plan(s, cRef, cName), "archive_plan"))
    }

    /// Permanently delete a plan and its tasks.
    public func deletePlan(_ name: String, ref: String = "main") throws {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cName = sgDup(name); defer { free(cName) }
        _ = try consume(agentstategraph_taskstore_delete_plan(s, cRef, cName), "delete_plan")
    }

    // MARK: - Tasks

    /// Append a new task to a plan.
    @discardableResult
    public func addTask(
        plan: String,
        title: String,
        priority: Priority,
        options: AddTaskOptions = AddTaskOptions(),
        ref: String = "main"
    ) throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cTitle = sgDup(title); defer { free(cTitle) }
        let cPrio = sgDup(priority.rawValue); defer { free(cPrio) }
        let cParent = sgDup(options.parentId); defer { free(cParent) }
        let blockersJSON = try options.blockers.map { try encodeJSON($0) }
        let cBlockers = sgDup(blockersJSON); defer { free(cBlockers) }
        let cAssigned = sgDup(options.assignedTo); defer { free(cAssigned) }
        let raw = try consume(
            agentstategraph_taskstore_add_task(
                s, cRef, cPlan, cTitle, cPrio, cParent, cBlockers, cAssigned),
            "add_task")
        return try decodeJSON(raw)
    }

    /// Append a task, threading the extended fields (`payload`,
    /// `parentChange`, `onComplete`) through the FFI.
    @discardableResult
    public func addTask(
        plan: String,
        title: String,
        priority: Priority,
        withExtensions options: AddTaskExtOptions,
        ref: String = "main"
    ) throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cTitle = sgDup(title); defer { free(cTitle) }
        let cPrio = sgDup(priority.rawValue); defer { free(cPrio) }
        let cParent = sgDup(options.base.parentId); defer { free(cParent) }
        let blockersJSON = try options.base.blockers.map { try encodeJSON($0) }
        let cBlockers = sgDup(blockersJSON); defer { free(cBlockers) }
        let cAssigned = sgDup(options.base.assignedTo); defer { free(cAssigned) }
        let payloadJSON = try options.payload.map { try encodeJSON($0) }
        let cPayload = sgDup(payloadJSON); defer { free(cPayload) }
        let cParentChange = sgDup(options.parentChange); defer { free(cParentChange) }
        let onCompleteJSON = try options.onComplete.map { try encodeJSON($0) }
        let cOnComplete = sgDup(onCompleteJSON); defer { free(cOnComplete) }
        let raw = try consume(
            agentstategraph_taskstore_add_task_ex(
                s, cRef, cPlan, cTitle, cPrio, cParent, cBlockers, cAssigned,
                cPayload, cParentChange, cOnComplete),
            "add_task_ex")
        return try decodeJSON(raw)
    }

    /// Every task in a plan.
    public func listTasks(plan: String, ref: String = "main") throws -> [Task] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        return try decodeJSON(try consume(agentstategraph_taskstore_list_tasks(s, cRef, cPlan), "list_tasks"))
    }

    /// Every task id in a plan, without deserializing bodies.
    public func taskIds(plan: String, ref: String = "main") throws -> [String] {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        return try decodeJSON(try consume(agentstategraph_taskstore_task_ids(s, cRef, cPlan), "task_ids"))
    }

    /// Fetch a single task.
    public func getTask(plan: String, id: String, ref: String = "main") throws -> Task {
        try decodeJSON(try call3(agentstategraph_taskstore_get_task, plan, id, ref, "get_task"))
    }

    /// Transition `pending → in_progress`.
    @discardableResult
    public func startTask(plan: String, id: String, ref: String = "main") throws -> Task {
        try decodeJSON(try call3(agentstategraph_taskstore_start_task, plan, id, ref, "start_task"))
    }

    /// Transition `in_progress → done` with attached proof.
    @discardableResult
    public func completeTask(plan: String, id: String, proof: Proof, ref: String = "main") throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        let cKind = sgDup(proof.kind.rawValue); defer { free(cKind) }
        let cValue = sgDup(proof.value); defer { free(cValue) }
        let cNote = sgDup(proof.note); defer { free(cNote) }
        let raw = try consume(
            agentstategraph_taskstore_complete_task(s, cRef, cPlan, cID, cKind, cValue, cNote),
            "complete_task")
        return try decodeJSON(raw)
    }

    /// Transition `pending|in_progress → abandoned` with a reason.
    @discardableResult
    public func abandonTask(plan: String, id: String, reason: String, ref: String = "main") throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        let cReason = sgDup(reason); defer { free(cReason) }
        let raw = try consume(
            agentstategraph_taskstore_abandon_task(s, cRef, cPlan, cID, cReason), "abandon_task")
        return try decodeJSON(raw)
    }

    /// Update a task's priority.
    @discardableResult
    public func setPriority(plan: String, id: String, priority: Priority, ref: String = "main") throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        let cPrio = sgDup(priority.rawValue); defer { free(cPrio) }
        let raw = try consume(
            agentstategraph_taskstore_set_priority(s, cRef, cPlan, cID, cPrio), "set_priority")
        return try decodeJSON(raw)
    }

    /// Replace a task's blocker list.
    @discardableResult
    public func setBlockers(plan: String, id: String, blockers: [String], ref: String = "main") throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        let cBlockers = sgDup(try encodeJSON(blockers)); defer { free(cBlockers) }
        let raw = try consume(
            agentstategraph_taskstore_set_blockers(s, cRef, cPlan, cID, cBlockers), "set_blockers")
        return try decodeJSON(raw)
    }

    /// Set the task's `assignedTo` field.
    @discardableResult
    public func assignTask(plan: String, id: String, agent: String, ref: String = "main") throws -> Task {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        let cAgent = sgDup(agent); defer { free(cAgent) }
        let raw = try consume(
            agentstategraph_taskstore_assign_task(s, cRef, cPlan, cID, cAgent), "assign_task")
        return try decodeJSON(raw)
    }

    /// Clear a task's `assignedTo` field.
    @discardableResult
    public func unassignTask(plan: String, id: String, ref: String = "main") throws -> Task {
        try decodeJSON(try call3(agentstategraph_taskstore_unassign_task, plan, id, ref, "unassign_task"))
    }

    /// The next unblocked pending task, or `nil` if none remain.
    public func nextTask(plan: String, ref: String = "main") throws -> Task? {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let raw = try consume(agentstategraph_taskstore_next_task(s, cRef, cPlan), "next_task")
        return try decodeOptionalTask(raw)
    }

    /// `nextTask` with assignment filtering. `agent == nil` means any; when
    /// set, `includeUnassigned` controls fallback to unassigned tasks.
    public func nextTask(
        plan: String,
        for agent: String?,
        includeUnassigned: Bool,
        ref: String = "main"
    ) throws -> Task? {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cAgent = sgDup(agent); defer { free(cAgent) }
        let raw = try consume(
            agentstategraph_taskstore_next_task_for(
                s, cRef, cPlan, cAgent, includeUnassigned ? 1 : 0),
            "next_task_for")
        return try decodeOptionalTask(raw)
    }

    /// The rollup status of a parent task derived from its children.
    public func derivedStatus(plan: String, parentId: String, ref: String = "main") throws -> TaskStatus {
        let raw = try call3(agentstategraph_taskstore_derived_status, plan, parentId, ref, "derived_status")
        return try decodeJSON(raw)
    }

    // MARK: - internals

    /// Shared shape: `fn(store, ref, plan, id)` — note the FFI takes
    /// `(store, ref, plan, id)`, so we pass ref first.
    private func call3(
        _ fn: (UnsafeMutableRawPointer?, UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
        _ plan: String, _ id: String, _ ref: String, _ op: String
    ) throws -> String {
        let s = try h()
        let cRef = sgDup(ref); defer { free(cRef) }
        let cPlan = sgDup(plan); defer { free(cPlan) }
        let cID = sgDup(id); defer { free(cID) }
        return try consume(fn(s, cRef, cPlan, cID), op)
    }

    private func decodeOptionalTask(_ raw: String) throws -> Task? {
        if raw == "null" || raw.isEmpty { return nil }
        return try decodeJSON(raw)
    }
}
