import XCTest
@testable import AgentStateGraph

final class PolicyTests: XCTestCase {
    private let now = "2026-01-01T00:00:00Z"

    private func policy(_ path: String) -> Policy {
        Policy(
            path: path,
            version: 1,
            situation: "situation for \(path)",
            situationSelector: .object(["kind": .string("always")]),
            severity: .low,
            proposedBy: "swifttest",
            proposedAt: now,
            activeFrom: now
        )
    }

    private func store() throws -> (AgentStateGraph, PolicyStore) {
        let asg = try AgentStateGraph()
        let ps = try PolicyStore(asg, prefix: "/policies", agentId: "swifttest")
        return (asg, ps)
    }

    func testProposeCreatesUnratified() throws {
        let (asg, ps) = try store()
        defer { asg.close(); ps.close() }
        let handle = try ps.propose(policy("infra/k8s/pod-failing"))
        XCTAssertEqual(handle, "infra/k8s/pod-failing@1")
        let got = try ps.get(path: "infra/k8s/pod-failing")
        XCTAssertEqual(got.version, 1)
        XCTAssertNil(got.ratifiedBy)
        XCTAssertEqual(got.proposedBy, "swifttest")
    }

    func testRatifyPromotes() throws {
        let (asg, ps) = try store()
        defer { asg.close(); ps.close() }
        var p = policy("infra/restart")
        p.allow = [AuthorizedAction(action: "restart_pod")]
        _ = try ps.propose(p)
        try ps.ratify(path: "infra/restart", ratifier: "ops-lead", reasoning: "approved after review")
        let got = try ps.get(path: "infra/restart")
        XCTAssertEqual(got.ratifiedBy, "ops-lead")
        XCTAssertEqual(got.ratificationReasoning, "approved after review")
        XCTAssertNotNil(got.ratifiedAt)
    }

    func testSupersedeChainAndHistory() throws {
        let (asg, ps) = try store()
        defer { asg.close(); ps.close() }
        var p = policy("infra/scale")
        p.allow = [AuthorizedAction(action: "scale_up")]
        _ = try ps.propose(p)
        try ps.ratify(path: "infra/scale", ratifier: "ops", reasoning: "v1")
        var v2 = policy("infra/scale")
        v2.allow = [AuthorizedAction(action: "scale_up"), AuthorizedAction(action: "scale_down")]
        let handle = try ps.supersede(path: "infra/scale", with: v2)
        XCTAssertEqual(handle, "infra/scale@2")
        let hist = try ps.history(path: "infra/scale")
        XCTAssertEqual(hist.map(\.version), [1, 2])
    }

    func testEvaluateAllows() throws {
        let (asg, ps) = try store()
        defer { asg.close(); ps.close() }
        var p = policy("infra/restart")
        p.allow = [AuthorizedAction(action: "restart_pod")]
        _ = try ps.propose(p)
        try ps.ratify(path: "infra/restart", ratifier: "ops", reasoning: "ok")
        let decision = try ps.evaluate(situation: [:], action: "restart_pod", agentId: "swifttest")
        XCTAssertEqual(decision.kind, .allow)
    }
}
