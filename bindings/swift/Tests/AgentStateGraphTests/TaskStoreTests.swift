import XCTest
@testable import AgentStateGraph

final class TaskStoreTests: XCTestCase {
    private func store() throws -> (AgentStateGraph, TaskStore) {
        let asg = try AgentStateGraph()
        let ts = try TaskStore(asg, prefix: "/tasks", agentId: "tester")
        return (asg, ts)
    }

    func testPlanLifecycle() throws {
        let (asg, ts) = try store()
        defer { asg.close(); ts.close() }
        let plan = try ts.createPlan("launch", description: "ship it")
        XCTAssertEqual(plan.name, "launch")
        XCTAssertEqual(plan.status, .active)
        XCTAssertEqual(try ts.getPlan("launch").name, "launch")
        XCTAssertEqual(try ts.listPlans().count, 1)
        let archived = try ts.archivePlan("launch")
        XCTAssertEqual(archived.status, .archived)
        XCTAssertEqual(try ts.listPlans(status: .archived).count, 1)
    }

    func testTaskLifecycleWithProof() throws {
        let (asg, ts) = try store()
        defer { asg.close(); ts.close() }
        try ts.createPlan("work")
        let task = try ts.addTask(plan: "work", title: "do thing", priority: .high)
        XCTAssertEqual(task.status, .pending)
        XCTAssertEqual(task.priority, .high)

        let started = try ts.startTask(plan: "work", id: task.id)
        XCTAssertEqual(started.status, .inProgress)

        let proof = Proof(kind: .commit, value: "abc123", note: "landed")
        let done = try ts.completeTask(plan: "work", id: task.id, proof: proof)
        XCTAssertEqual(done.status, .done)
        XCTAssertEqual(done.proof?.value, "abc123")
    }

    func testNextTaskRespectsBlockers() throws {
        let (asg, ts) = try store()
        defer { asg.close(); ts.close() }
        try ts.createPlan("p")
        let a = try ts.addTask(plan: "p", title: "a", priority: .medium)
        let b = try ts.addTask(
            plan: "p", title: "b", priority: .critical,
            options: AddTaskOptions(blockers: [a.id]))
        // b is higher priority but blocked by a, so next should be a.
        let next = try ts.nextTask(plan: "p")
        XCTAssertEqual(next?.id, a.id)
        _ = b
    }

    func testAssignAndFilter() throws {
        let (asg, ts) = try store()
        defer { asg.close(); ts.close() }
        try ts.createPlan("p")
        let t = try ts.addTask(plan: "p", title: "t", priority: .low)
        let assigned = try ts.assignTask(plan: "p", id: t.id, agent: "alice")
        XCTAssertEqual(assigned.assignedTo, "alice")
        let forBob = try ts.nextTask(plan: "p", for: "bob", includeUnassigned: false)
        XCTAssertNil(forBob)
        let forAlice = try ts.nextTask(plan: "p", for: "alice", includeUnassigned: false)
        XCTAssertEqual(forAlice?.id, t.id)
    }

    func testExtendedTaskFields() throws {
        let (asg, ts) = try store()
        defer { asg.close(); ts.close() }
        try ts.createPlan("p")
        let payload = JSONValue.object(["k": .string("v")])
        let ext = AddTaskExtOptions(payload: payload)
        let t = try ts.addTask(plan: "p", title: "ext", priority: .medium, withExtensions: ext)
        XCTAssertEqual(t.payload, payload)
    }
}
