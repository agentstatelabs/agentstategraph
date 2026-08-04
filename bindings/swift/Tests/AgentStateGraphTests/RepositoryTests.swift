import XCTest
@testable import AgentStateGraph

final class RepositoryTests: XCTestCase {
    func testSetGetRoundTrip() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        let commit = try asg.set("/name", json: "\"my-cluster\"", category: .checkpoint, description: "init")
        XCTAssertFalse(commit.isEmpty)
        XCTAssertEqual(try asg.get("/name"), "\"my-cluster\"")
    }

    func testTypedSetGet() throws {
        struct Node: Codable, Equatable { let host: String; let cores: Int }
        let asg = try AgentStateGraph()
        defer { asg.close() }
        let node = Node(host: "pico1", cores: 4)
        try asg.set("/nodes/pico1", value: node, category: .checkpoint, description: "add node")
        XCTAssertEqual(try asg.get("/nodes/pico1", as: Node.self), node)
    }

    func testDeleteThenGetFails() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/tmp", json: "1", category: .checkpoint, description: "set")
        try asg.delete("/tmp", category: .correction, description: "remove")
        XCTAssertThrowsError(try asg.get("/tmp"))
    }

    func testBranchesAndListAndDelete() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/x", json: "1", category: .checkpoint, description: "seed")
        _ = try asg.branch("feature", from: "main")
        let branches = try asg.listBranches()
        XCTAssertTrue(branches.contains { $0.name == "feature" })
        XCTAssertTrue(branches.contains { $0.name == "main" })
        XCTAssertTrue(try asg.deleteBranch("feature"))
        XCTAssertFalse(try asg.deleteBranch("does-not-exist"))
    }

    func testLogRecordsIntent() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/a", json: "1", category: .checkpoint, description: "first")
        try asg.set("/b", json: "2", category: .refinement, description: "second")
        let log = try asg.log(limit: 10)
        XCTAssertGreaterThanOrEqual(log.count, 2)
        XCTAssertTrue(log.contains { $0.intentDescription == "second" })
    }

    func testMergeBranch() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/base", json: "1", category: .checkpoint, description: "base")
        _ = try asg.branch("wip", from: "main")
        try asg.set("/feature", json: "true", category: .checkpoint, description: "on wip", ref: "wip")
        let mergeCommit = try asg.merge(source: "wip", target: "main", description: "land wip")
        XCTAssertFalse(mergeCommit.isEmpty)
        XCTAssertEqual(try asg.get("/feature"), "true")
    }

    func testUseAfterCloseThrows() throws {
        let asg = try AgentStateGraph()
        asg.close()
        XCTAssertThrowsError(try asg.get("/x")) { error in
            guard case AgentStateGraphError.closed = error else {
                return XCTFail("expected .closed, got \(error)")
            }
        }
    }
}
