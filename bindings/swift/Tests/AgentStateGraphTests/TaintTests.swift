import XCTest
@testable import AgentStateGraph

final class TaintTests: XCTestCase {
    func testApplyListAndCheck() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/svc/db", json: "{}", category: .checkpoint, description: "seed")

        let id = try asg.taint(
            "/svc/db",
            params: TaintParams(
                name: "leak", effect: .warn, reason: "possible secret leak",
                severity: .high, agentId: "scanner"))
        XCTAssertFalse(id.isEmpty)

        let taints = try asg.listTaints(pathPrefix: "/svc")
        XCTAssertTrue(taints.contains { $0.name == "leak" && $0.effect == .warn })

        let check = try asg.checkTaint("/svc/db", agentId: "worker", confidence: 0.9)
        XCTAssertTrue(check.tainted)
    }

    func testUntaintResolves() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/x", json: "1", category: .checkpoint, description: "seed")
        _ = try asg.taint("/x", params: TaintParams(name: "t", effect: .warn, reason: "r", agentId: "a"))
        try asg.untaint("/x", name: "t", params: UntaintParams(reason: "cleared", agentId: "a"))
        let active = try asg.listTaints(pathPrefix: "/x")
        XCTAssertFalse(active.contains { $0.name == "t" })
    }

    func testQuarantineBlocksUnauthorized() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/vault", json: "{}", category: .checkpoint, description: "seed")
        _ = try asg.quarantine(
            "/vault",
            params: QuarantineParams(
                name: "lockdown", reason: "incident", authorizedAgents: ["oncall"], agentId: "sec"))
        let check = try asg.checkTaint("/vault", agentId: "random", confidence: 1.0)
        XCTAssertTrue(check.quarantined)
        XCTAssertFalse(check.canWrite)
    }
}
