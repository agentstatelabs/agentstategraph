package agentstategraph

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestTaskStore_RoundTrip(t *testing.T) {
	asg, err := NewMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer asg.Close()

	ts, err := NewTaskStore(asg, "/plans", "go-test")
	if err != nil {
		t.Fatal(err)
	}
	defer ts.Close()

	desc := "Brand pivot"
	plan, err := ts.CreatePlan("main", "website-v2", &desc)
	if err != nil {
		t.Fatalf("CreatePlan: %v", err)
	}
	if plan.Name != "website-v2" || plan.Status != PlanStatusActive {
		t.Fatalf("unexpected plan: %+v", plan)
	}

	plans, err := ts.ListPlans("main")
	if err != nil {
		t.Fatal(err)
	}
	if len(plans) != 1 {
		t.Fatalf("expected 1 plan, got %d", len(plans))
	}

	task, err := ts.AddTask("main", "website-v2", "Rewrite hero", PriorityHigh, nil)
	if err != nil {
		t.Fatalf("AddTask: %v", err)
	}
	if task.ID != "t-001" {
		t.Fatalf("expected t-001, got %s", task.ID)
	}
	if task.Status != TaskStatusPending {
		t.Fatalf("expected pending, got %s", task.Status)
	}

	started, err := ts.StartTask("main", "website-v2", task.ID)
	if err != nil {
		t.Fatalf("StartTask: %v", err)
	}
	if started.Status != TaskStatusInProgress {
		t.Fatalf("expected in_progress, got %s", started.Status)
	}

	done, err := ts.CompleteTask("main", "website-v2", task.ID, Proof{
		Kind:  ProofKindCommit,
		Value: "deadbeef",
	})
	if err != nil {
		t.Fatalf("CompleteTask: %v", err)
	}
	if done.Status != TaskStatusDone {
		t.Fatalf("expected done, got %s", done.Status)
	}
	if done.Proof == nil || done.Proof.Value != "deadbeef" {
		t.Fatalf("unexpected proof: %+v", done.Proof)
	}

	// Migrate check on a fresh in-memory repo should be up_to_date.
	report, err := asg.MigrateCheck("main", "")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(report, "up_to_date") {
		t.Fatalf("expected up_to_date, got %s", report)
	}

	// Dry-run migrate.
	run, err := asg.MigrateRun("main", "", "dry-run")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(run, "dry-run") {
		t.Fatalf("expected dry-run mode in report, got %s", run)
	}
}

func TestTaskStore_NextTaskBlockers(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()
	ts, err := NewTaskStore(asg, "/plans", "go-test")
	if err != nil {
		t.Fatal(err)
	}
	defer ts.Close()

	if _, err := ts.CreatePlan("main", "p", nil); err != nil {
		t.Fatal(err)
	}
	a, err := ts.AddTask("main", "p", "a", PriorityHigh, nil)
	if err != nil {
		t.Fatal(err)
	}
	_, err = ts.AddTask("main", "p", "b", PriorityCritical, &AddTaskOptions{
		Blockers: []string{a.ID},
	})
	if err != nil {
		t.Fatal(err)
	}
	next, err := ts.NextTask("main", "p")
	if err != nil {
		t.Fatal(err)
	}
	if next == nil || next.ID != a.ID {
		t.Fatalf("expected blocked 'b' to defer to 'a', got %+v", next)
	}
}

func TestTaskStore_AddTaskWithExtensions(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()
	store, err := NewTaskStore(asg, "/plans", "agent/test")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	desc := "test plan"
	if _, err := store.CreatePlan("main", "p1", &desc); err != nil {
		t.Fatal(err)
	}

	parentChange := "change:spec-1"
	opts := &AddTaskExtOptions{
		Payload:      json.RawMessage(`{"preferred": "Option C"}`),
		ParentChange: &parentChange,
		OnComplete:   json.RawMessage(`{"kind": "promote_change"}`),
	}
	task, err := store.AddTaskWithExtensions("main", "p1", "Approve change", PriorityMedium, opts)
	if err != nil {
		t.Fatal(err)
	}

	if len(task.Payload) == 0 {
		t.Fatal("expected Payload to round-trip; got empty")
	}
	var p map[string]string
	if err := json.Unmarshal(task.Payload, &p); err != nil {
		t.Fatalf("payload decode: %v", err)
	}
	if p["preferred"] != "Option C" {
		t.Fatalf("payload round-trip: got %+v", p)
	}

	if task.ParentChange == nil || *task.ParentChange != parentChange {
		t.Fatalf("ParentChange round-trip: got %+v", task.ParentChange)
	}

	if len(task.OnComplete) == 0 {
		t.Fatal("expected OnComplete to round-trip; got empty")
	}
	var hook map[string]string
	if err := json.Unmarshal(task.OnComplete, &hook); err != nil {
		t.Fatalf("on_complete decode: %v", err)
	}
	if hook["kind"] != "promote_change" {
		t.Fatalf("on_complete round-trip: got %+v", hook)
	}
}
