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

	// Finishing the last task leaves the plan open; closing it takes a
	// summary.
	open, err := ts.GetPlan("main", "website-v2")
	if err != nil {
		t.Fatal(err)
	}
	if open.Status != PlanStatusActive {
		t.Fatalf("expected the plan still active, got %s", open.Status)
	}
	if _, err := ts.ClosePlan("main", "website-v2", "  "); err == nil || !strings.Contains(err.Error(), "summary") {
		t.Fatalf("expected an empty summary refused, got %v", err)
	}
	closed, err := ts.ClosePlan("main", "website-v2", "Hero rewritten")
	if err != nil {
		t.Fatalf("ClosePlan: %v", err)
	}
	if closed.Status != PlanStatusCompleted || closed.Summary == nil || *closed.Summary != "Hero rewritten" {
		t.Fatalf("unexpected closed plan: %+v", closed)
	}
	if closed.ClosedBy == nil || *closed.ClosedBy != "go-test" || closed.ClosedAt == nil {
		t.Fatalf("unexpected close fields: %+v", closed)
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

func TestTaskStore_ClosePlanRefusesOpenWork(t *testing.T) {
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

	if _, err := ts.CreatePlan("main", "p", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := ts.ClosePlan("main", "p", "nothing to do"); err == nil || !strings.Contains(err.Error(), "no tasks") {
		t.Fatalf("expected a plan with no tasks refused, got %v", err)
	}
	task, err := ts.AddTask("main", "p", "x", PriorityMedium, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ts.ClosePlan("main", "p", "too early"); err == nil || !strings.Contains(err.Error(), task.ID) {
		t.Fatalf("expected an open task refused, got %v", err)
	}
	if _, err := ts.AbandonTask("main", "p", task.ID, "not needed"); err != nil {
		t.Fatal(err)
	}
	closed, err := ts.ClosePlan("main", "p", "dropped")
	if err != nil || closed.Status != PlanStatusCompleted {
		t.Fatalf("expected the plan closed, got %+v, %v", closed, err)
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

func TestTaskStore_EditTask(t *testing.T) {
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
	b, err := ts.AddTask("main", "p", "b", PriorityHigh, &AddTaskOptions{
		Blockers: []string{a.ID},
	})
	if err != nil {
		t.Fatal(err)
	}

	retitled, err := ts.SetTitle("main", "p", a.ID, "a, retitled")
	if err != nil {
		t.Fatalf("SetTitle: %v", err)
	}
	if retitled.Title != "a, retitled" || retitled.Status != TaskStatusPending {
		t.Fatalf("unexpected task after SetTitle: %+v", retitled)
	}
	if _, err := ts.SetTitle("main", "p", a.ID, "  "); err == nil {
		t.Fatal("SetTitle accepted a blank title")
	}

	// b waits on a, so a can't wait on b.
	_, err = ts.SetBlockers("main", "p", a.ID, []string{b.ID})
	if err == nil || !strings.Contains(err.Error(), "cycle") {
		t.Fatalf("expected a blocker cycle error, got %v", err)
	}
}

func TestTaskStore_Notes(t *testing.T) {
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
	brief := "Repo: infra/gitops.\n\n1. Write the manifests.\n2. Ask the user. ✓\n"
	a, err := ts.AddTask("main", "p", "a", PriorityHigh, &AddTaskOptions{Notes: brief})
	if err != nil {
		t.Fatal(err)
	}
	if a.Notes == nil || *a.Notes != brief {
		t.Fatalf("AddTask notes: %v", a.Notes)
	}
	b, err := ts.AddTask("main", "p", "b", PriorityHigh, nil)
	if err != nil {
		t.Fatal(err)
	}
	if b.Notes != nil {
		t.Fatalf("a task added without notes has notes: %q", *b.Notes)
	}

	if _, err := ts.StartTask("main", "p", a.ID); err != nil {
		t.Fatal(err)
	}
	edited, err := ts.SetNotes("main", "p", a.ID, "line one\nline two")
	if err != nil {
		t.Fatalf("SetNotes: %v", err)
	}
	if edited.Notes == nil || *edited.Notes != "line one\nline two" || edited.Status != TaskStatusInProgress {
		t.Fatalf("unexpected task after SetNotes: %+v", edited)
	}
	if _, err := ts.SetNotes("main", "p", a.ID, strings.Repeat("x", 16*1024+1)); err == nil ||
		!strings.Contains(err.Error(), "16384") {
		t.Fatalf("expected the notes cap to refuse 16385 bytes, got %v", err)
	}
	cleared, err := ts.SetNotes("main", "p", a.ID, "")
	if err != nil {
		t.Fatal(err)
	}
	if cleared.Notes != nil {
		t.Fatalf("SetNotes(\"\") left notes: %q", *cleared.Notes)
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
