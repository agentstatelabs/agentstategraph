package agentstategraph

// Taint / Quarantine / Watch Go binding tests (0.7.75-beta.1 §9c).
//
// Each test spins a fresh in-memory repository and exercises one
// end-to-end scenario against the FFI. Mirrors the shapes covered by
// the Rust integration tests in
// `crates/agentstategraph/tests/taint_*.rs` and the smoke tests in
// `crates/agentstategraph-ffi/tests/taint_ffi.rs`.

import (
	"testing"
)

func newRepoForTaint(t *testing.T) *AgentStateGraph {
	t.Helper()
	asg, err := NewMemory()
	if err != nil {
		t.Fatalf("NewMemory: %v", err)
	}
	t.Cleanup(func() { asg.Close() })
	return asg
}

func ptrString(s string) *string { return &s }
func ptrFloat(f float64) *float64 { return &f }

// TestTaint_RoundTrip — apply → list → untaint → list-resolved.
func TestTaint_RoundTrip(t *testing.T) {
	asg := newRepoForTaint(t)

	id, err := asg.Taint("main", "/cluster/picoup2", TaintParams{
		Name:     "disk-pressure",
		Effect:   EffectWarn,
		Reason:   "disk > 80%",
		Severity: TaintSeverityMedium,
		AgentID:  "ops",
	})
	if err != nil {
		t.Fatalf("Taint: %v", err)
	}
	if id == "" {
		t.Fatal("Taint returned empty id")
	}

	list, err := asg.ListTaints(nil, nil, false)
	if err != nil {
		t.Fatalf("ListTaints: %v", err)
	}
	if len(list) != 1 {
		t.Fatalf("expected 1 active taint, got %d", len(list))
	}
	if list[0].ID != id || list[0].Name != "disk-pressure" {
		t.Fatalf("unexpected taint row: %+v", list[0])
	}
	if list[0].Kind != KindTaint || list[0].Effect != EffectWarn {
		t.Fatalf("kind/effect mismatch: %+v", list[0])
	}

	if err := asg.Untaint("main", "/cluster/picoup2", "disk-pressure", UntaintParams{
		Reason:  "resolved",
		Proof:   ptrString("commit-abc"),
		AgentID: "ops",
	}); err != nil {
		t.Fatalf("Untaint: %v", err)
	}

	active, err := asg.ListTaints(nil, nil, false)
	if err != nil {
		t.Fatalf("ListTaints active: %v", err)
	}
	if len(active) != 0 {
		t.Fatalf("expected 0 active after untaint, got %d", len(active))
	}

	all, err := asg.ListTaints(nil, nil, true)
	if err != nil {
		t.Fatalf("ListTaints include_resolved: %v", err)
	}
	if len(all) != 1 {
		t.Fatalf("expected 1 resolved row, got %d", len(all))
	}
	if all[0].ResolvedAt == nil {
		t.Fatal("resolved_at not populated")
	}
}

// TestTaint_BlockEffectRejectsSet — a block-effect taint on a path
// causes AgentStateGraph.Set on that same path to fail.
func TestTaint_BlockEffectRejectsSet(t *testing.T) {
	asg := newRepoForTaint(t)

	if _, err := asg.Taint("main", "/guarded", TaintParams{
		Name:    "locked",
		Effect:  EffectBlock,
		Reason:  "under review",
		AgentID: "ops",
	}); err != nil {
		t.Fatalf("Taint: %v", err)
	}

	if _, err := asg.Set("/guarded", `"nope"`, "Refine", "try-write"); err == nil {
		t.Fatal("expected Set on blocked path to fail")
	}

	// Writes outside the guarded subtree still succeed.
	if _, err := asg.Set("/elsewhere", `"ok"`, "Refine", "adjacent-write"); err != nil {
		t.Fatalf("expected unrelated Set to succeed, got %v", err)
	}
}

// TestTaint_ReviewConfidenceGate — a review-effect taint requires
// confidence >= 0.9; CheckTaint reports required_confidence = 0.9 and
// rejects a 0.5 caller while admitting a 0.95 caller.
func TestTaint_ReviewConfidenceGate(t *testing.T) {
	asg := newRepoForTaint(t)

	if _, err := asg.Taint("main", "/review-me", TaintParams{
		Name:    "needs-review",
		Effect:  EffectReview,
		Reason:  "audit",
		AgentID: "ops",
	}); err != nil {
		t.Fatalf("Taint: %v", err)
	}

	low, err := asg.CheckTaint("/review-me", "agent-1", 0.5)
	if err != nil {
		t.Fatalf("CheckTaint low: %v", err)
	}
	if !low.Tainted {
		t.Fatal("expected tainted=true")
	}
	if low.RequiredConfidence != 0.9 {
		t.Fatalf("required_confidence = %v, want 0.9", low.RequiredConfidence)
	}
	if low.CanWrite {
		t.Fatal("expected can_write=false at confidence 0.5")
	}

	high, err := asg.CheckTaint("/review-me", "agent-1", 0.95)
	if err != nil {
		t.Fatalf("CheckTaint high: %v", err)
	}
	if !high.CanWrite {
		t.Fatal("expected can_write=true at confidence 0.95")
	}
}

// TestTaint_Quarantine — apply a quarantine, confirm unauthorized
// agents are blocked, authorized agents pass, then release.
func TestTaint_Quarantine(t *testing.T) {
	asg := newRepoForTaint(t)

	id, err := asg.Quarantine("main", "/secret", QuarantineParams{
		Name:             "leak-response",
		Reason:           "possible exfil",
		Severity:         TaintSeverityHigh,
		AuthorizedAgents: []string{"agent/security"},
		AgentID:          "agent/security",
	})
	if err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if id == "" {
		t.Fatal("Quarantine returned empty id")
	}

	// Unauthorized agent can't write.
	unauth, err := asg.CheckTaint("/secret", "agent-1", 1.0)
	if err != nil {
		t.Fatalf("CheckTaint unauth: %v", err)
	}
	if !unauth.Quarantined {
		t.Fatal("expected quarantined=true")
	}
	if unauth.CanWrite {
		t.Fatal("expected can_write=false for unauthorized agent")
	}
	if len(unauth.AuthorizedAgents) != 1 || unauth.AuthorizedAgents[0] != "agent/security" {
		t.Fatalf("authorized_agents = %v, want [agent/security]", unauth.AuthorizedAgents)
	}

	// Authorized agent can.
	auth, err := asg.CheckTaint("/secret", "agent/security", 1.0)
	if err != nil {
		t.Fatalf("CheckTaint auth: %v", err)
	}
	if !auth.CanWrite {
		t.Fatal("expected can_write=true for authorized agent")
	}

	// Release.
	if err := asg.Unquarantine("main", "/secret", "leak-response", UntaintParams{
		Reason:  "cleared",
		AgentID: "agent/security",
	}); err != nil {
		t.Fatalf("Unquarantine: %v", err)
	}
	active, err := asg.ListTaints(nil, ptrString("quarantine"), false)
	if err != nil {
		t.Fatalf("ListTaints quarantine: %v", err)
	}
	if len(active) != 0 {
		t.Fatalf("expected 0 active quarantines after release, got %d", len(active))
	}
}

// TestTaint_WatchAutoEscalation — a watch on a numeric metric that
// the caller subsequently breaches must produce an auto-escalated
// taint (see crates/agentstategraph/tests/taint_auto_escalation.rs).
func TestTaint_WatchAutoEscalation(t *testing.T) {
	asg := newRepoForTaint(t)

	if _, err := asg.Watch("main", "/cluster/disk", WatchParams{
		Name:      "disk-80",
		Reason:    "perf",
		Metric:    ptrString("disk_used_pct"),
		Threshold: ptrFloat(80.0),
		Direction: WatchAbove,
		AgentID:   "ops",
	}); err != nil {
		t.Fatalf("Watch: %v", err)
	}

	// Writing a value above the threshold must trigger the
	// auto-escalation path, which creates a fresh taint under the
	// same prefix.
	if _, err := asg.Set("/cluster/disk", `{"disk_used_pct": 82.0}`, "Refine", "breach"); err != nil {
		t.Fatalf("Set triggering watch: %v", err)
	}

	prefix := "/cluster"
	kind := "taint"
	taints, err := asg.ListTaints(&prefix, &kind, false)
	if err != nil {
		t.Fatalf("ListTaints after breach: %v", err)
	}
	if len(taints) != 1 {
		t.Fatalf("expected 1 auto-escalated taint, got %d (%+v)", len(taints), taints)
	}
	if taints[0].Kind != KindTaint {
		t.Fatalf("auto-taint kind = %v, want taint", taints[0].Kind)
	}
	if got := string(taints[0].Name); len(got) < len("watch-threshold-exceeded-") ||
		got[:len("watch-threshold-exceeded-")] != "watch-threshold-exceeded-" {
		t.Fatalf("auto-taint name %q does not start with watch-threshold-exceeded-", got)
	}
}

// TestTaint_CheckAggregates — multiple records on the same prefix
// must all show up in CheckTaint's bucketed slices, propagating from
// a parent path.
func TestTaint_CheckAggregates(t *testing.T) {
	asg := newRepoForTaint(t)

	// Warn-effect taint + quarantine + watch on a parent path.
	if _, err := asg.Taint("main", "/cluster", TaintParams{
		Name:    "soft-warn",
		Effect:  EffectWarn,
		Reason:  "monitoring",
		AgentID: "ops",
	}); err != nil {
		t.Fatalf("Taint: %v", err)
	}
	if _, err := asg.Quarantine("main", "/cluster", QuarantineParams{
		Name:             "audit",
		Reason:           "spot check",
		AuthorizedAgents: []string{"agent/security"},
		AgentID:          "agent/security",
	}); err != nil {
		t.Fatalf("Quarantine: %v", err)
	}
	if _, err := asg.Watch("main", "/cluster", WatchParams{
		Name:    "observer",
		Reason:  "telemetry",
		AgentID: "ops",
	}); err != nil {
		t.Fatalf("Watch: %v", err)
	}

	// Check a child path — propagate=true by default means all three
	// records apply.
	check, err := asg.CheckTaint("/cluster/nodeA", "agent-1", 1.0)
	if err != nil {
		t.Fatalf("CheckTaint: %v", err)
	}
	if !check.Tainted || !check.Quarantined || !check.Watched {
		t.Fatalf("expected all three flags true, got %+v", check)
	}
	if len(check.Taints) != 1 || len(check.Quarantines) != 1 || len(check.Watches) != 1 {
		t.Fatalf("bucket lengths = (%d,%d,%d), want 1/1/1",
			len(check.Taints), len(check.Quarantines), len(check.Watches))
	}
	if check.CanWrite {
		t.Fatal("expected can_write=false (quarantine blocks unauthorized agent)")
	}
	if len(check.AuthorizedAgents) != 1 || check.AuthorizedAgents[0] != "agent/security" {
		t.Fatalf("authorized_agents = %v, want [agent/security]", check.AuthorizedAgents)
	}
}
