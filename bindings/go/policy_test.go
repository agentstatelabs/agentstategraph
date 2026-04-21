package agentstategraph

// PolicyStore Go binding tests. Mirror the Python test_policy.py
// scenarios so every binding exercises the same behaviours end-to-end.

import (
	"encoding/json"
	"testing"
	"time"
)

// newPolicy — convenience constructor analogous to _policy() in
// bindings/python/tests/test_policy.py. Leaves every optional field
// unset so individual tests can override just what they need.
func newPolicy(path string) Policy {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	return Policy{
		Path:              path,
		Version:           1,
		Situation:         "situation for " + path,
		SituationSelector: json.RawMessage(`{"kind":"always"}`),
		Severity:          SeverityLow,
		ProposedBy:        "gotest",
		ProposedAt:        now,
		ActiveFrom:        now,
	}
}

func newStore(t *testing.T) (*AgentStateGraph, *PolicyStore) {
	t.Helper()
	asg, err := NewMemory()
	if err != nil {
		t.Fatalf("NewMemory: %v", err)
	}
	ps, err := NewPolicyStore(asg, "/policies", "gotest")
	if err != nil {
		asg.Close()
		t.Fatalf("NewPolicyStore: %v", err)
	}
	t.Cleanup(func() {
		ps.Close()
		asg.Close()
	})
	return asg, ps
}

func TestPolicy_ProposeCreatesUnratified(t *testing.T) {
	_, ps := newStore(t)
	handle, err := ps.Propose("main", newPolicy("infra/k8s/pod-failing"))
	if err != nil {
		t.Fatalf("Propose: %v", err)
	}
	if handle != "infra/k8s/pod-failing@1" {
		t.Fatalf("unexpected handle %q", handle)
	}
	got, err := ps.Get("main", "infra/k8s/pod-failing")
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	if got.Version != 1 {
		t.Fatalf("version: %d", got.Version)
	}
	if got.RatifiedBy != nil {
		t.Fatalf("expected unratified, got %v", *got.RatifiedBy)
	}
	if got.ProposedBy != "gotest" {
		t.Fatalf("proposed_by: %q", got.ProposedBy)
	}
}

func TestPolicy_RatifyPromotes(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/restart")
	p.Allow = []AuthorizedAction{{Action: "restart_pod"}}
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/restart", "ops-lead", "approved after review"); err != nil {
		t.Fatalf("Ratify: %v", err)
	}
	got, err := ps.Get("main", "infra/restart")
	if err != nil {
		t.Fatal(err)
	}
	if got.RatifiedBy == nil || *got.RatifiedBy != "ops-lead" {
		t.Fatalf("ratified_by: %+v", got.RatifiedBy)
	}
	if got.RatificationReasoning == nil || *got.RatificationReasoning != "approved after review" {
		t.Fatalf("reasoning: %+v", got.RatificationReasoning)
	}
	if got.RatifiedAt == nil {
		t.Fatalf("ratified_at nil")
	}
}

func TestPolicy_SupersedeChainAndHistory(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/scale")
	p.Allow = []AuthorizedAction{{Action: "scale_up"}}
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/scale", "ops", "v1"); err != nil {
		t.Fatal(err)
	}
	v2 := newPolicy("infra/scale")
	v2.Allow = []AuthorizedAction{{Action: "scale_up"}, {Action: "scale_down"}}
	handle, err := ps.Supersede("main", "infra/scale", v2)
	if err != nil {
		t.Fatalf("Supersede: %v", err)
	}
	if handle != "infra/scale@2" {
		t.Fatalf("handle: %q", handle)
	}
	hist, err := ps.History("main", "infra/scale")
	if err != nil {
		t.Fatal(err)
	}
	if len(hist) != 2 || hist[0].Version != 1 || hist[1].Version != 2 {
		t.Fatalf("history versions: %+v", hist)
	}
	if hist[1].Supersedes == nil || *hist[1].Supersedes != "infra/scale@1" {
		t.Fatalf("supersedes link: %+v", hist[1].Supersedes)
	}
}

func TestPolicy_EvaluateAllow(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/restart")
	p.Allow = []AuthorizedAction{{Action: "restart_pod"}}
	p.SituationSelector = json.RawMessage(`{"kind":"eq","key":"namespace","value":"prod"}`)
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/restart", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	d, err := ps.Evaluate("main", map[string]string{"namespace": "prod"}, "restart_pod", "agent-1")
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionAllow {
		t.Fatalf("kind: %q", d.Kind)
	}
	if d.MatchedPolicy != "infra/restart@1" {
		t.Fatalf("matched: %q", d.MatchedPolicy)
	}
}

func TestPolicy_EvaluateDeny(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/no-delete")
	cond := "always"
	p.Deny = []AuthorizedAction{{Action: "delete_node", Condition: &cond}}
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/no-delete", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	d, err := ps.Evaluate("main", nil, "delete_node", "agent-1")
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionDeny {
		t.Fatalf("kind: %q", d.Kind)
	}
}

func TestPolicy_EvaluateRequireApproval(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/risky")
	p.RequireApproval = []ApprovalRule{{
		Action:    "truncate_index",
		Approvers: []string{"human"},
		Fallback:  json.RawMessage(`{"kind":"block"}`),
	}}
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/risky", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	d, err := ps.Evaluate("main", nil, "truncate_index", "agent-1")
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionRequireApproval {
		t.Fatalf("kind: %q", d.Kind)
	}
	if len(d.Approvers) != 1 || d.Approvers[0] != "human" {
		t.Fatalf("approvers: %+v", d.Approvers)
	}
	var fb struct {
		Kind string `json:"kind"`
	}
	if err := json.Unmarshal(d.Fallback, &fb); err != nil {
		t.Fatalf("fallback unmarshal: %v", err)
	}
	if fb.Kind != "block" {
		t.Fatalf("fallback kind: %q", fb.Kind)
	}
}

func TestPolicy_EvaluateNoMatch(t *testing.T) {
	_, ps := newStore(t)
	d, err := ps.Evaluate("main", nil, "anything", "agent-1")
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionNoPolicyMatch {
		t.Fatalf("kind: %q", d.Kind)
	}
}

func TestPolicy_EvaluateChangeWithTriggersAndFallback(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/high-cost")
	p.Triggers = []string{"reindex", "downtime"}
	p.RequiredFields = []string{"estimated_downtime"}
	p.RequireApproval = []ApprovalRule{{
		Action:    "promote",
		Approvers: []string{"human"},
		Fallback:  json.RawMessage(`{"kind":"lowest_risk_alternative"}`),
	}}
	p.Severity = SeverityHigh
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/high-cost", "ops", "big changes need approval"); err != nil {
		t.Fatal(err)
	}
	proposal := ChangeProposal{
		Action:          "promote",
		AgentID:         "agent-1",
		Intent:          "merge option C",
		PreferredOption: "spec-7",
		Alternatives:    []string{"spec-1", "spec-3"},
		Tokens:          []string{"reindex"},
		AttachedFields:  map[string]string{"estimated_downtime": "5m"},
	}
	d, err := ps.EvaluateChange("main", proposal)
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionRequireApproval {
		t.Fatalf("kind: %q", d.Kind)
	}
	var fb struct {
		Kind string `json:"kind"`
	}
	if err := json.Unmarshal(d.Fallback, &fb); err != nil {
		t.Fatalf("fallback unmarshal: %v", err)
	}
	if fb.Kind != "lowest_risk_alternative" {
		t.Fatalf("fallback kind: %q", fb.Kind)
	}
}

func TestPolicy_EvaluateChangeMissingRequiredFields(t *testing.T) {
	_, ps := newStore(t)
	p := newPolicy("infra/needs-downtime")
	p.Triggers = []string{"reindex"}
	p.RequiredFields = []string{"estimated_downtime"}
	p.RequireApproval = []ApprovalRule{{
		Action:    "promote",
		Approvers: []string{"human"},
		Fallback:  json.RawMessage(`{"kind":"block"}`),
	}}
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/needs-downtime", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	proposal := ChangeProposal{
		Action:          "promote",
		AgentID:         "agent-1",
		PreferredOption: "x",
		Tokens:          []string{"reindex"},
		AttachedFields:  map[string]string{},
	}
	d, err := ps.EvaluateChange("main", proposal)
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionRequireApproval {
		t.Fatalf("kind: %q", d.Kind)
	}
}

func TestPolicy_EvaluateIgnoresNotYetActive(t *testing.T) {
	// §1 of the 0.7.0 plan: active_from in the future → skipped.
	_, ps := newStore(t)
	p := newPolicy("infra/future")
	p.Allow = []AuthorizedAction{{Action: "do_it"}}
	p.ActiveFrom = time.Now().UTC().Add(time.Hour).Format(time.RFC3339Nano)
	if _, err := ps.Propose("main", p); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/future", "ops", "scheduled"); err != nil {
		t.Fatal(err)
	}
	d, err := ps.Evaluate("main", nil, "do_it", "agent-1")
	if err != nil {
		t.Fatal(err)
	}
	if d.Kind != DecisionNoPolicyMatch {
		t.Fatalf("kind: %q", d.Kind)
	}
	actives, err := ps.Active("main", "")
	if err != nil {
		t.Fatal(err)
	}
	for _, a := range actives {
		if a.Path == "infra/future" {
			t.Fatalf("active filter leaked not-yet-active policy: %+v", a)
		}
	}
}

func TestPolicy_CheckTokensTriggerIntersection(t *testing.T) {
	_, ps := newStore(t)
	a := newPolicy("infra/with-reindex")
	a.Triggers = []string{"reindex"}
	if _, err := ps.Propose("main", a); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/with-reindex", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	b := newPolicy("infra/with-network")
	b.Triggers = []string{"network"}
	if _, err := ps.Propose("main", b); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/with-network", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	one, err := ps.CheckTokens("main", []string{"reindex"})
	if err != nil {
		t.Fatal(err)
	}
	if len(one) != 1 || one[0].Path != "infra/with-reindex" {
		t.Fatalf("single-token match: %+v", one)
	}
	both, err := ps.CheckTokens("main", []string{"reindex", "network"})
	if err != nil {
		t.Fatal(err)
	}
	if len(both) != 2 {
		t.Fatalf("both tokens: expected 2 policies, got %+v", both)
	}
}

func TestPolicy_ListAndActiveFilters(t *testing.T) {
	_, ps := newStore(t)
	if _, err := ps.Propose("main", newPolicy("infra/a")); err != nil {
		t.Fatal(err)
	}
	if _, err := ps.Propose("main", newPolicy("infra/b")); err != nil {
		t.Fatal(err)
	}
	if err := ps.Ratify("main", "infra/b", "ops", "ok"); err != nil {
		t.Fatal(err)
	}
	listed, err := ps.List("main", "")
	if err != nil {
		t.Fatal(err)
	}
	seen := map[string]bool{}
	for _, p := range listed {
		seen[p.Path] = true
	}
	if !seen["infra/a"] || !seen["infra/b"] {
		t.Fatalf("List missing entries: %+v", listed)
	}
	actives, err := ps.Active("main", "")
	if err != nil {
		t.Fatal(err)
	}
	if len(actives) != 1 || actives[0].Path != "infra/b" {
		t.Fatalf("active filter: %+v", actives)
	}
	// prefix filter
	onlyA, err := ps.List("main", "infra/a")
	if err != nil {
		t.Fatal(err)
	}
	if len(onlyA) != 1 || onlyA[0].Path != "infra/a" {
		t.Fatalf("prefix filter: %+v", onlyA)
	}
}

func TestPolicy_RatifyEmptyRatifierRejected(t *testing.T) {
	// PolicyStore::ratify rejects an empty ratifier (the trimmed
	// string must have content). Empty reasoning is stored as None
	// — not an error — so this test guards the one commit-time
	// check the Rust store actually enforces.
	_, ps := newStore(t)
	if _, err := ps.Propose("main", newPolicy("infra/x")); err != nil {
		t.Fatal(err)
	}
	err := ps.Ratify("main", "infra/x", "", "some reasoning")
	if err == nil {
		t.Fatalf("expected error for empty ratifier, got nil")
	}
}
