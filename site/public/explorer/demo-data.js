// Demo data for the AgentStateGraph Explorer.
// Simulates the output of the 26 MCP tools against a real cluster management scenario.

const DEMO = {
  stats: {
    commit_count: 47,
    branch_count: 5,
    path_count: 23,
    epoch_count: 2,
    agents: ["agent/monitor", "agent/planner", "agent/setup", "agent/network", "agent/compliance"],
    categories: ["Checkpoint", "Explore", "Fix", "Merge", "Refine", "Rollback"],
    latest_commit: {
      id: "f5b2..17",
      agent: "agent/compliance",
      intent: "Seal Q1 operations epoch",
      timestamp: "2026-04-10T14:33:00Z",
    },
  },

  paths: [
    "/cluster/name",
    "/cluster/region",
    "/cluster/nodes/0/hostname",
    "/cluster/nodes/0/status",
    "/cluster/nodes/0/gpu_memory_mb",
    "/cluster/nodes/0/role",
    "/cluster/nodes/1/hostname",
    "/cluster/nodes/1/status",
    "/cluster/nodes/1/gpu_memory_mb",
    "/cluster/nodes/1/role",
    "/cluster/nodes/2/hostname",
    "/cluster/nodes/2/status",
    "/cluster/nodes/2/gpu_memory_mb",
    "/cluster/nodes/2/role",
    "/cluster/network/topology",
    "/cluster/network/subnet",
    "/cluster/network/dns",
    "/cluster/storage/type",
    "/cluster/storage/capacity_gb",
    "/cluster/config/log_level",
    "/cluster/config/auto_failover",
    "/cluster/config/max_retries",
    "/cluster/version",
  ],

  state: {
    cluster: {
      name: "picoclaw-prod",
      region: "us-west-2",
      version: "0.3.5-beta.2",
      nodes: [
        { hostname: "picorpi0", status: "healthy", gpu_memory_mb: 8192, role: "leader" },
        { hostname: "picorpi1", status: "healthy", gpu_memory_mb: 8192, role: "worker" },
        { hostname: "picorpi2", status: "draining", gpu_memory_mb: 4096, role: "worker" },
      ],
      network: { topology: "mesh", subnet: "10.0.0.0/24", dns: "1.1.1.1" },
      storage: { type: "nfs", capacity_gb: 4000 },
      config: { log_level: "info", auto_failover: true, max_retries: 3 },
    },
  },

  commits: [
    { id: "f5b2..17", agent: "agent/compliance", category: "Checkpoint", description: "Seal Q1 operations epoch", confidence: 0.99, timestamp: "2026-04-10T14:33:00Z", parents: ["d1e8..9a"], is_merge: false },
    { id: "d1e8..9a", agent: "agent/monitor", category: "Fix", description: "Reroute traffic from failed node-2", confidence: 0.73, timestamp: "2026-04-10T02:14:07Z", parents: ["c3f1..ab"], is_merge: false, reasoning: "Node-2 health check failed 3x in 60s. Rerouting to node-0 and node-1. Not restarting — disk I/O errors suggest hardware." },
    { id: "c3f1..ab", agent: "agent/planner", category: "Refine", description: "Apply mesh topology with failover", confidence: 0.89, timestamp: "2026-04-09T16:45:00Z", parents: ["b2a0..cd"], is_merge: true, reasoning: "Mesh topology selected over star — better fault tolerance for 3-node cluster." },
    { id: "b2a0..cd", agent: "agent/planner", category: "Explore", description: "Evaluate network topology options", confidence: 0.72, timestamp: "2026-04-09T16:30:00Z", parents: ["a1b2..ef"], is_merge: false },
    { id: "a1b2..ef", agent: "agent/network", category: "Refine", description: "Set DNS to Cloudflare", confidence: 0.95, timestamp: "2026-04-09T15:00:00Z", parents: ["9f3a..12"], is_merge: false },
    { id: "9f3a..12", agent: "agent/setup", category: "Checkpoint", description: "Configure NFS storage at 4TB", confidence: 0.98, timestamp: "2026-04-09T14:30:00Z", parents: ["8e2b..34"], is_merge: false },
    { id: "8e2b..34", agent: "agent/setup", category: "Checkpoint", description: "Add node-2 (picorpi2) as worker", confidence: 0.97, timestamp: "2026-04-09T14:15:00Z", parents: ["7d1c..56"], is_merge: false },
    { id: "7d1c..56", agent: "agent/setup", category: "Checkpoint", description: "Add node-1 (picorpi1) as worker", confidence: 0.97, timestamp: "2026-04-09T14:10:00Z", parents: ["6c0d..78"], is_merge: false },
    { id: "6c0d..78", agent: "agent/setup", category: "Checkpoint", description: "Initialize cluster with node-0 as leader", confidence: 0.99, timestamp: "2026-04-09T14:00:00Z", parents: [], is_merge: false },
  ],

  branches: [
    { name: "main", commit: "f5b2..17" },
    { name: "explore/star-topology", commit: "b2a0..cd" },
    { name: "explore/mesh-topology", commit: "c3f1..ab" },
    { name: "agents/monitor/workspace", commit: "d1e8..9a" },
    { name: "archive/q1-setup", commit: "9f3a..12" },
  ],

  epochs: [
    { id: "2026-Q1-setup", description: "Initial cluster setup and configuration", status: "Sealed", commits: 8, agents: ["agent/setup", "agent/network"], sealed_at: "2026-04-09T17:00:00Z", seal_hash: "a7f3c2e8..b91d" },
    { id: "2026-Q1-ops", description: "Q1 operational incidents and maintenance", status: "Active", commits: 39, agents: ["agent/monitor", "agent/planner", "agent/compliance"], sealed_at: null, seal_hash: null },
  ],

  blame: {
    "/cluster/nodes/2/status": {
      path: "/cluster/nodes/2/status",
      commit_id: "d1e8..9a",
      agent_id: "agent/monitor",
      intent_category: "Fix",
      intent_description: "Reroute traffic from failed node-2",
      reasoning: "Node-2 health check failed 3x in 60s. Rerouting to node-0 and node-1. Not restarting — disk I/O errors suggest hardware.",
      confidence: 0.73,
      timestamp: "2026-04-10T02:14:07Z",
      alternatives: "restart (rejected, 0.21), ignore (rejected, 0.06)",
      authority: "ops-policy/auto-failover",
    },
  },

  search_results: [
    { path: "/cluster/name", value: "picoclaw-prod" },
    { path: "/cluster/nodes/0/hostname", value: "picorpi0" },
    { path: "/cluster/nodes/1/hostname", value: "picorpi1" },
    { path: "/cluster/nodes/2/hostname", value: "picorpi2" },
    { path: "/cluster/network/topology", value: "mesh" },
  ],
};
