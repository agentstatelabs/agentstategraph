import XCTest
@testable import AgentStateGraph

final class AdvancedRepositoryTests: XCTestCase {
    func testCapabilitiesAdvertiseContract() throws {
        let capabilities = try AgentStateGraph.capabilities()
        XCTAssertEqual(capabilities.contractVersion, 1)
        XCTAssertTrue(capabilities.operations.contains("namespace.list"))
        XCTAssertTrue(capabilities.operations.contains("session.create"))
        XCTAssertTrue(capabilities.operations.contains("query.commits"))
    }

    func testNamespaceIsolation() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        let project = try asg.scoped(to: "project-one")
        defer { project.close() }

        try project.set("/name", json: "\"one\"", category: .checkpoint,
                        description: "project value")
        XCTAssertEqual(try project.get("/name"), "\"one\"")
        XCTAssertThrowsError(try asg.get("/name"))
        XCTAssertTrue(try asg.listNamespaces().contains("project-one"))
    }

    func testCASRejectsStaleHead() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        let original = try asg.head()
        _ = try asg.setCAS("/first", value: 1, expectedHead: original,
                           category: .checkpoint, description: "first")
        XCTAssertThrowsError(try asg.setCAS("/stale", value: 2, expectedHead: original,
                                             category: .correction, description: "stale"))
    }

    func testQueryAndIntentAliases() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/fix", json: "true", category: .correction, description: "repair")
        let matches = try asg.queryCommits(CommitQuery(intentCategory: "Fix"))
        XCTAssertTrue(matches.contains { $0.intentDescription == "repair" })
    }

    func testSpeculationCommitsAtomically() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        let handle = try asg.speculate(label: "two writes")
        try asg.setSpeculation(handle, path: "/a", value: 1)
        try asg.setSpeculation(handle, path: "/b", value: 2)
        _ = try asg.commitSpeculation(handle, category: .checkpoint,
                                      description: "atomic pair")
        XCTAssertEqual(try asg.get("/a"), "1")
        XCTAssertEqual(try asg.get("/b"), "2")
    }

    func testSessionLifecycleAndNamespaceScope() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.createNamespace("session-space")
        let session = try asg.createSession(agentId: "agent/swift",
                                             pathScope: "/work",
                                             scopeNamespace: "session-space")
        XCTAssertEqual(session.scopeNamespace, "session-space")
        try asg.setActiveSession(session.id)
        XCTAssertEqual(try asg.activeSession(), session.id)
        try asg.endSession(session.id, status: .completed)
        XCTAssertEqual(try asg.session(id: session.id)?.status, .completed)
    }

    func testEpochLifecycle() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        _ = try asg.createEpoch(id: "sprint-1", description: "first sprint",
                                rootIntents: ["ship"])
        try asg.setActiveEpoch("sprint-1")
        try asg.set("/work", json: "true", category: .checkpoint,
                    description: "epoch work")
        try asg.sealEpoch("sprint-1", summary: "done")
        XCTAssertEqual(try asg.epoch(id: "sprint-1").status, .sealed)
        XCTAssertTrue(try asg.epochs().contains { $0.id == "sprint-1" })
    }

    func testExplorerAndMergePreview() throws {
        let asg = try AgentStateGraph()
        defer { asg.close() }
        try asg.set("/messages/one", json: "\"searchable phrase\"",
                    category: .checkpoint, description: "seed")
        _ = try asg.branch("feature", from: "main")
        try asg.set("/feature", json: "true", category: .exploration,
                    description: "branch", ref: "feature")
        XCTAssertTrue(try asg.listPaths(prefix: "/messages").contains("/messages/one"))
        XCTAssertEqual(try asg.search("searchable").first?.path, "/messages/one")
        XCTAssertTrue(try asg.previewMerge(source: "feature", target: "main")
            .added.contains("feature"))
    }
}
