package agentstategraph

// Cross-binding policy parity runner — Go side.
//
// §7 of the 0.7.0-beta.1 plan. Loads the shared fixture at
// spec/policy_parity_fixture.json relative to this test file, seeds
// the scenario via the cgo-backed PolicyStore, and asserts the same
// decision.kind + matched_policy prefix as every other binding's
// runner.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// parityFixture mirrors spec/policy_parity_fixture.json. We keep
// per-entry proposal/situation as json.RawMessage so we can hand the
// policy-store a struct that matches the Go binding's own types
// without re-stating every field.
type parityFixture struct {
	Prefix         string                 `json:"prefix"`
	AgentID        string                 `json:"agent_id"`
	Ref            string                 `json:"ref"`
	Policies       []Policy               `json:"policies"`
	Ratify         []parityRatify         `json:"ratify"`
	Changes        []parityChange         `json:"change_proposals"`
	Evals          []parityEval           `json:"evaluate"`
	ExtraPolicies  []Policy               `json:"extra_policies,omitempty"`
	RatifyExtra    []parityRatify         `json:"ratify_extra,omitempty"`
	TenantEvaluate []parityTenantEval     `json:"tenant_evaluate,omitempty"`
	ExternalEval   []parityEval           `json:"external_evaluate,omitempty"`
}

type parityRatify struct {
	Path      string `json:"path"`
	Ratifier  string `json:"ratifier"`
	Reasoning string `json:"reasoning"`
}

type parityChange struct {
	Label                       string          `json:"label"`
	Proposal                    ChangeProposal  `json:"proposal"`
	ExpectedKind                string          `json:"expected_decision_kind"`
	ExpectedMatchedPolicyPrefix string          `json:"expected_matched_policy_prefix,omitempty"`
}

type parityEval struct {
	Label        string            `json:"label"`
	Situation    map[string]string `json:"situation"`
	Action       string            `json:"action"`
	AgentID      string            `json:"agent_id"`
	ExpectedKind string            `json:"expected_decision_kind"`
}

type parityTenantEval struct {
	Label                       string            `json:"label"`
	Situation                   map[string]string `json:"situation"`
	Action                      string            `json:"action"`
	AgentID                     string            `json:"agent_id"`
	TenantFilter                *string           `json:"tenant_filter"`
	ExpectedKind                string            `json:"expected_decision_kind"`
	ExpectedMatchedPolicyPrefix string            `json:"expected_matched_policy_prefix,omitempty"`
}

func loadParityFixture(t *testing.T) parityFixture {
	t.Helper()
	// runtime.Caller(0) = this file. Fixture lives at
	// <repo>/spec/policy_parity_fixture.json; this file lives at
	// <repo>/bindings/go/parity_test.go.
	_, here, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}
	p := filepath.Join(filepath.Dir(here), "..", "..", "spec", "policy_parity_fixture.json")
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatalf("read fixture %s: %v", p, err)
	}
	var fx parityFixture
	if err := json.Unmarshal(b, &fx); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	if fx.Prefix == "" {
		fx.Prefix = "/policies"
	}
	if fx.AgentID == "" {
		fx.AgentID = "parity-runner"
	}
	if fx.Ref == "" {
		fx.Ref = "main"
	}
	return fx
}

func TestPolicy_ParityFixtureMatchesGoBinding(t *testing.T) {
	fx := loadParityFixture(t)

	asg, err := NewMemory()
	if err != nil {
		t.Fatalf("NewMemory: %v", err)
	}
	t.Cleanup(func() { asg.Close() })
	ps, err := NewPolicyStore(asg, fx.Prefix, fx.AgentID)
	if err != nil {
		t.Fatalf("NewPolicyStore: %v", err)
	}
	t.Cleanup(func() { ps.Close() })

	for _, pol := range fx.Policies {
		if _, err := ps.Propose(fx.Ref, pol); err != nil {
			t.Fatalf("Propose %s: %v", pol.Path, err)
		}
	}
	for _, r := range fx.Ratify {
		if err := ps.Ratify(fx.Ref, r.Path, r.Ratifier, r.Reasoning); err != nil {
			t.Fatalf("Ratify %s: %v", r.Path, err)
		}
	}

	for _, entry := range fx.Changes {
		d, err := ps.EvaluateChange(fx.Ref, entry.Proposal)
		if err != nil {
			t.Fatalf("EvaluateChange %s: %v", entry.Label, err)
		}
		if string(d.Kind) != entry.ExpectedKind {
			t.Fatalf("%s: decision.kind = %q, want %q", entry.Label, d.Kind, entry.ExpectedKind)
		}
		if entry.ExpectedMatchedPolicyPrefix != "" {
			if !strings.HasPrefix(d.MatchedPolicy, entry.ExpectedMatchedPolicyPrefix) {
				t.Fatalf("%s: matched_policy %q should start with %q",
					entry.Label, d.MatchedPolicy, entry.ExpectedMatchedPolicyPrefix)
			}
		}
	}

	for _, entry := range fx.Evals {
		d, err := ps.Evaluate(fx.Ref, entry.Situation, entry.Action, entry.AgentID)
		if err != nil {
			t.Fatalf("Evaluate %s: %v", entry.Label, err)
		}
		if string(d.Kind) != entry.ExpectedKind {
			t.Fatalf("%s: decision.kind = %q, want %q", entry.Label, d.Kind, entry.ExpectedKind)
		}
	}

	// 5. (0.7.5 §6) Optional extra_policies + ratify_extra + tenant/external
	//    evaluate blocks. Unmarshal uses omitempty so fixtures without these
	//    keys produce nil slices and these loops no-op.
	for _, pol := range fx.ExtraPolicies {
		if _, err := ps.Propose(fx.Ref, pol); err != nil {
			t.Fatalf("Propose extra %s: %v", pol.Path, err)
		}
	}
	for _, r := range fx.RatifyExtra {
		if err := ps.Ratify(fx.Ref, r.Path, r.Ratifier, r.Reasoning); err != nil {
			t.Fatalf("Ratify extra %s: %v", r.Path, err)
		}
	}

	for _, entry := range fx.TenantEvaluate {
		d, err := ps.EvaluateScoped(fx.Ref, entry.Situation, entry.Action, entry.AgentID, entry.TenantFilter)
		if err != nil {
			t.Fatalf("EvaluateScoped %s: %v", entry.Label, err)
		}
		if string(d.Kind) != entry.ExpectedKind {
			t.Fatalf("tenant %s: decision.kind = %q, want %q", entry.Label, d.Kind, entry.ExpectedKind)
		}
		if entry.ExpectedMatchedPolicyPrefix != "" {
			if !strings.HasPrefix(d.MatchedPolicy, entry.ExpectedMatchedPolicyPrefix) {
				t.Fatalf("tenant %s: matched_policy %q should start with %q",
					entry.Label, d.MatchedPolicy, entry.ExpectedMatchedPolicyPrefix)
			}
		}
	}

	for _, entry := range fx.ExternalEval {
		// No external runner registered → policy with external_evaluator set
		// is skipped, falling through to no_policy_match.
		d, err := ps.Evaluate(fx.Ref, entry.Situation, entry.Action, entry.AgentID)
		if err != nil {
			t.Fatalf("Evaluate (external) %s: %v", entry.Label, err)
		}
		if string(d.Kind) != entry.ExpectedKind {
			t.Fatalf("external %s: decision.kind = %q, want %q", entry.Label, d.Kind, entry.ExpectedKind)
		}
	}
}
