package agentstategraph

// TaskStore + Migrate Go bindings on top of the AgentStateGraph C FFI.
//
// The Rust substrate returns task/plan data as JSON strings; these helpers
// deserialize into strongly typed Go values. On error, the FFI returns a
// JSON document of the form {"error": "..."} which is surfaced as a Go
// error.

/*
#include <stdlib.h>
#include <stdint.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"time"
	"unsafe"
)

// Priority — task urgency (ordered low < medium < high < critical).
type Priority string

const (
	PriorityLow      Priority = "low"
	PriorityMedium   Priority = "medium"
	PriorityHigh     Priority = "high"
	PriorityCritical Priority = "critical"
)

// TaskStatus — lifecycle state of a task.
type TaskStatus string

const (
	TaskStatusPending    TaskStatus = "pending"
	TaskStatusInProgress TaskStatus = "in_progress"
	TaskStatusDone       TaskStatus = "done"
	TaskStatusAbandoned  TaskStatus = "abandoned"
)

// PlanStatus — lifecycle state of a plan.
type PlanStatus string

const (
	PlanStatusActive    PlanStatus = "active"
	PlanStatusCompleted PlanStatus = "completed"
	PlanStatusArchived  PlanStatus = "archived"
)

// ProofKind — category of evidence attached to a completed task.
type ProofKind string

const (
	ProofKindCommit ProofKind = "commit"
	ProofKindFile   ProofKind = "file"
	ProofKindTest   ProofKind = "test"
	ProofKindText   ProofKind = "text"
)

// Proof — evidence attached to a `done` task.
type Proof struct {
	Kind  ProofKind `json:"kind"`
	Value string    `json:"value"`
	Note  *string   `json:"note,omitempty"`
}

// Plan — a named container of tasks.
type Plan struct {
	Name        string     `json:"name"`
	Description *string    `json:"description,omitempty"`
	Status      PlanStatus `json:"status"`
	CreatedAt   time.Time  `json:"created_at"`
	CreatedBy   string     `json:"created_by"`
	ArchivedAt  *time.Time `json:"archived_at,omitempty"`
}

// Task — a unit of work in a plan.
type Task struct {
	ID              string     `json:"id"`
	Title           string     `json:"title"`
	Status          TaskStatus `json:"status"`
	Priority        Priority   `json:"priority"`
	ParentID        *string    `json:"parent_id,omitempty"`
	BlockedBy       []string   `json:"blocked_by,omitempty"`
	CreatedAt       time.Time  `json:"created_at"`
	CreatedBy       string     `json:"created_by"`
	StartedAt       *time.Time `json:"started_at,omitempty"`
	StartedBy       *string    `json:"started_by,omitempty"`
	CompletedAt     *time.Time `json:"completed_at,omitempty"`
	CompletedBy     *string    `json:"completed_by,omitempty"`
	Proof           *Proof     `json:"proof,omitempty"`
	AbandonedAt     *time.Time `json:"abandoned_at,omitempty"`
	AbandonedReason *string    `json:"abandoned_reason,omitempty"`
	AssignedTo      *string    `json:"assigned_to,omitempty"`
}

// TaskStore — handle bound to an AgentStateGraph repository, path prefix,
// and agent id. All task and plan writes commit as `IntentCategory::Plan`.
type TaskStore struct {
	handle C.SgTaskStore
}

// NewTaskStore constructs a TaskStore on top of an existing AgentStateGraph.
// The repository is shared (refcounted); closing the TaskStore does NOT
// close the repository.
func NewTaskStore(asg *AgentStateGraph, prefix, agentID string) (*TaskStore, error) {
	if asg == nil || asg.repo == nil {
		return nil, errors.New("nil repository")
	}
	cPrefix := C.CString(prefix)
	defer C.free(unsafe.Pointer(cPrefix))
	cAgent := C.CString(agentID)
	defer C.free(unsafe.Pointer(cAgent))

	h := C.agentstategraph_taskstore_new(asg.repo, cPrefix, cAgent)
	if h == nil {
		return nil, errors.New("failed to create task store")
	}
	return &TaskStore{handle: h}, nil
}

// Close frees the TaskStore handle. The underlying repository is unaffected.
func (ts *TaskStore) Close() {
	if ts.handle != nil {
		C.agentstategraph_taskstore_free(ts.handle)
		ts.handle = nil
	}
}

// ---------------------------------------------------------------------------
// JSON result helpers
// ---------------------------------------------------------------------------

// ffiErr is used to decode {"error": "..."} documents returned by the FFI.
type ffiErr struct {
	Error string `json:"error"`
}

func consume(p *C.char, op string) (string, error) {
	if p == nil {
		return "", fmt.Errorf("%s: ffi returned null", op)
	}
	s := C.GoString(p)
	C.agentstategraph_free_string(p)
	return s, nil
}

func decodeOrErr(raw string, out interface{}) error {
	// Attempt to decode an {"error": ...} envelope first.
	var e ffiErr
	if err := json.Unmarshal([]byte(raw), &e); err == nil && e.Error != "" {
		return errors.New(e.Error)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal([]byte(raw), out)
}

// ---------------------------------------------------------------------------
// Plan operations
// ---------------------------------------------------------------------------

// CreatePlan creates a new plan under this store's prefix.
func (ts *TaskStore) CreatePlan(ref, name string, description *string) (*Plan, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	var cDesc *C.char
	if description != nil {
		cDesc = C.CString(*description)
		defer C.free(unsafe.Pointer(cDesc))
	}
	raw, err := consume(
		C.agentstategraph_taskstore_create_plan(ts.handle, cRef, cName, cDesc),
		"create_plan",
	)
	if err != nil {
		return nil, err
	}
	var plan Plan
	if err := decodeOrErr(raw, &plan); err != nil {
		return nil, err
	}
	return &plan, nil
}

// ListPlans returns every plan under the store's prefix.
func (ts *TaskStore) ListPlans(ref string) ([]Plan, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	raw, err := consume(C.agentstategraph_taskstore_list_plans(ts.handle, cRef), "list_plans")
	if err != nil {
		return nil, err
	}
	var plans []Plan
	if err := decodeOrErr(raw, &plans); err != nil {
		return nil, err
	}
	return plans, nil
}

// ListPlansByStatus returns plans filtered by status. Pass nil for "all".
func (ts *TaskStore) ListPlansByStatus(ref string, status *PlanStatus) ([]Plan, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	statusStr := ""
	if status != nil {
		statusStr = string(*status)
	}
	cStatus := C.CString(statusStr)
	defer C.free(unsafe.Pointer(cStatus))
	raw, err := consume(
		C.agentstategraph_taskstore_list_plans_by_status(ts.handle, cRef, cStatus),
		"list_plans_by_status",
	)
	if err != nil {
		return nil, err
	}
	var plans []Plan
	if err := decodeOrErr(raw, &plans); err != nil {
		return nil, err
	}
	return plans, nil
}

// GetPlan fetches a plan by name.
func (ts *TaskStore) GetPlan(ref, name string) (*Plan, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	raw, err := consume(
		C.agentstategraph_taskstore_get_plan(ts.handle, cRef, cName),
		"get_plan",
	)
	if err != nil {
		return nil, err
	}
	var p Plan
	if err := decodeOrErr(raw, &p); err != nil {
		return nil, err
	}
	return &p, nil
}

// ArchivePlan soft-archives a plan.
func (ts *TaskStore) ArchivePlan(ref, name string) (*Plan, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	raw, err := consume(
		C.agentstategraph_taskstore_archive_plan(ts.handle, cRef, cName),
		"archive_plan",
	)
	if err != nil {
		return nil, err
	}
	var p Plan
	if err := decodeOrErr(raw, &p); err != nil {
		return nil, err
	}
	return &p, nil
}

// DeletePlan permanently removes a plan and its tasks.
func (ts *TaskStore) DeletePlan(ref, name string) error {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	raw, err := consume(
		C.agentstategraph_taskstore_delete_plan(ts.handle, cRef, cName),
		"delete_plan",
	)
	if err != nil {
		return err
	}
	return decodeOrErr(raw, nil)
}

// ---------------------------------------------------------------------------
// Task operations
// ---------------------------------------------------------------------------

// AddTaskOptions — optional fields for AddTask.
type AddTaskOptions struct {
	ParentID   *string
	Blockers   []string
	AssignedTo *string
}

// AddTask appends a new task to a plan.
func (ts *TaskStore) AddTask(ref, plan, title string, priority Priority, opts *AddTaskOptions) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cTitle := C.CString(title)
	defer C.free(unsafe.Pointer(cTitle))
	cPrio := C.CString(string(priority))
	defer C.free(unsafe.Pointer(cPrio))

	var cParent *C.char
	var cBlockers *C.char
	var cAssigned *C.char
	if opts != nil {
		if opts.ParentID != nil {
			cParent = C.CString(*opts.ParentID)
			defer C.free(unsafe.Pointer(cParent))
		}
		if len(opts.Blockers) > 0 {
			b, err := json.Marshal(opts.Blockers)
			if err != nil {
				return nil, err
			}
			cBlockers = C.CString(string(b))
			defer C.free(unsafe.Pointer(cBlockers))
		}
		if opts.AssignedTo != nil {
			cAssigned = C.CString(*opts.AssignedTo)
			defer C.free(unsafe.Pointer(cAssigned))
		}
	}

	raw, err := consume(
		C.agentstategraph_taskstore_add_task(
			ts.handle, cRef, cPlan, cTitle, cPrio, cParent, cBlockers, cAssigned,
		),
		"add_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// ListTasks returns every task in a plan.
func (ts *TaskStore) ListTasks(ref, plan string) ([]Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	raw, err := consume(
		C.agentstategraph_taskstore_list_tasks(ts.handle, cRef, cPlan),
		"list_tasks",
	)
	if err != nil {
		return nil, err
	}
	var tasks []Task
	if err := decodeOrErr(raw, &tasks); err != nil {
		return nil, err
	}
	return tasks, nil
}

// TaskIDs returns every task id in a plan, without deserializing bodies.
func (ts *TaskStore) TaskIDs(ref, plan string) ([]string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	raw, err := consume(
		C.agentstategraph_taskstore_task_ids(ts.handle, cRef, cPlan),
		"task_ids",
	)
	if err != nil {
		return nil, err
	}
	var ids []string
	if err := decodeOrErr(raw, &ids); err != nil {
		return nil, err
	}
	return ids, nil
}

// GetTask fetches a single task.
func (ts *TaskStore) GetTask(ref, plan, id string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	raw, err := consume(
		C.agentstategraph_taskstore_get_task(ts.handle, cRef, cPlan, cID),
		"get_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// StartTask transitions pending → in_progress.
func (ts *TaskStore) StartTask(ref, plan, id string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	raw, err := consume(
		C.agentstategraph_taskstore_start_task(ts.handle, cRef, cPlan, cID),
		"start_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// CompleteTask transitions in_progress → done with attached proof.
func (ts *TaskStore) CompleteTask(ref, plan, id string, proof Proof) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	cKind := C.CString(string(proof.Kind))
	defer C.free(unsafe.Pointer(cKind))
	cValue := C.CString(proof.Value)
	defer C.free(unsafe.Pointer(cValue))
	var cNote *C.char
	if proof.Note != nil {
		cNote = C.CString(*proof.Note)
		defer C.free(unsafe.Pointer(cNote))
	}
	raw, err := consume(
		C.agentstategraph_taskstore_complete_task(
			ts.handle, cRef, cPlan, cID, cKind, cValue, cNote,
		),
		"complete_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// AbandonTask transitions pending|in_progress → abandoned with a reason.
func (ts *TaskStore) AbandonTask(ref, plan, id, reason string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	cReason := C.CString(reason)
	defer C.free(unsafe.Pointer(cReason))
	raw, err := consume(
		C.agentstategraph_taskstore_abandon_task(ts.handle, cRef, cPlan, cID, cReason),
		"abandon_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// SetPriority updates a task's priority.
func (ts *TaskStore) SetPriority(ref, plan, id string, priority Priority) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	cPrio := C.CString(string(priority))
	defer C.free(unsafe.Pointer(cPrio))
	raw, err := consume(
		C.agentstategraph_taskstore_set_priority(ts.handle, cRef, cPlan, cID, cPrio),
		"set_priority",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// SetBlockers replaces a task's blocker list.
func (ts *TaskStore) SetBlockers(ref, plan, id string, blockers []string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	b, err := json.Marshal(blockers)
	if err != nil {
		return nil, err
	}
	cBlockers := C.CString(string(b))
	defer C.free(unsafe.Pointer(cBlockers))
	raw, err := consume(
		C.agentstategraph_taskstore_set_blockers(ts.handle, cRef, cPlan, cID, cBlockers),
		"set_blockers",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// AssignTask sets the task's `assigned_to` field.
func (ts *TaskStore) AssignTask(ref, plan, id, agent string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	cAgent := C.CString(agent)
	defer C.free(unsafe.Pointer(cAgent))
	raw, err := consume(
		C.agentstategraph_taskstore_assign_task(ts.handle, cRef, cPlan, cID, cAgent),
		"assign_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// UnassignTask clears a task's `assigned_to` field.
func (ts *TaskStore) UnassignTask(ref, plan, id string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(id)
	defer C.free(unsafe.Pointer(cID))
	raw, err := consume(
		C.agentstategraph_taskstore_unassign_task(ts.handle, cRef, cPlan, cID),
		"unassign_task",
	)
	if err != nil {
		return nil, err
	}
	var t Task
	if err := decodeOrErr(raw, &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// NextTask returns the next unblocked pending task, or nil if none.
func (ts *TaskStore) NextTask(ref, plan string) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	raw, err := consume(
		C.agentstategraph_taskstore_next_task(ts.handle, cRef, cPlan),
		"next_task",
	)
	if err != nil {
		return nil, err
	}
	return decodeOptionalTask(raw)
}

// NextTaskFor is NextTask with assignment filtering. `agent` nil means any;
// when set, `includeUnassigned` controls fallback to unassigned tasks.
func (ts *TaskStore) NextTaskFor(ref, plan string, agent *string, includeUnassigned bool) (*Task, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	var cAgent *C.char
	if agent != nil {
		cAgent = C.CString(*agent)
		defer C.free(unsafe.Pointer(cAgent))
	}
	inc := C.uint8_t(0)
	if includeUnassigned {
		inc = 1
	}
	raw, err := consume(
		C.agentstategraph_taskstore_next_task_for(ts.handle, cRef, cPlan, cAgent, inc),
		"next_task_for",
	)
	if err != nil {
		return nil, err
	}
	return decodeOptionalTask(raw)
}

// DerivedStatus returns the rollup status of a parent task.
func (ts *TaskStore) DerivedStatus(ref, plan, parentID string) (TaskStatus, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPlan := C.CString(plan)
	defer C.free(unsafe.Pointer(cPlan))
	cID := C.CString(parentID)
	defer C.free(unsafe.Pointer(cID))
	raw, err := consume(
		C.agentstategraph_taskstore_derived_status(ts.handle, cRef, cPlan, cID),
		"derived_status",
	)
	if err != nil {
		return "", err
	}
	var s TaskStatus
	if err := decodeOrErr(raw, &s); err != nil {
		return "", err
	}
	return s, nil
}

func decodeOptionalTask(raw string) (*Task, error) {
	var e ffiErr
	if err := json.Unmarshal([]byte(raw), &e); err == nil && e.Error != "" {
		return nil, errors.New(e.Error)
	}
	if raw == "null" || raw == "" {
		return nil, nil
	}
	var t Task
	if err := json.Unmarshal([]byte(raw), &t); err != nil {
		return nil, err
	}
	return &t, nil
}

// ---------------------------------------------------------------------------
// Migrate
// ---------------------------------------------------------------------------

// MigrateCheck inspects a repository and returns the schema status as JSON.
// `target` may be empty to use the binary's own `SCHEMA_VERSION`.
func (asg *AgentStateGraph) MigrateCheck(ref, target string) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	var cTarget *C.char
	if target != "" {
		cTarget = C.CString(target)
		defer C.free(unsafe.Pointer(cTarget))
	}
	raw, err := consume(
		C.agentstategraph_migrate_check(asg.repo, cRef, cTarget),
		"migrate_check",
	)
	if err != nil {
		return "", err
	}
	// decodeOrErr with nil still surfaces {"error":..} but we want the JSON
	// blob on success. Check error envelope manually.
	var e ffiErr
	if err := json.Unmarshal([]byte(raw), &e); err == nil && e.Error != "" {
		return "", errors.New(e.Error)
	}
	return raw, nil
}

// MigrateRun executes the migration plan. `mode` is "apply" or "dry-run".
func (asg *AgentStateGraph) MigrateRun(ref, target, mode string) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	var cTarget *C.char
	if target != "" {
		cTarget = C.CString(target)
		defer C.free(unsafe.Pointer(cTarget))
	}
	cMode := C.CString(mode)
	defer C.free(unsafe.Pointer(cMode))
	raw, err := consume(
		C.agentstategraph_migrate_run(asg.repo, cRef, cTarget, cMode),
		"migrate_run",
	)
	if err != nil {
		return "", err
	}
	var e ffiErr
	if err := json.Unmarshal([]byte(raw), &e); err == nil && e.Error != "" {
		return "", errors.New(e.Error)
	}
	return raw, nil
}
