package agentstategraph

// PolicyStore Go binding on top of the AgentStateGraph C FFI.
//
// Mirrors the surface of the Python (bindings/python) and TypeScript
// (bindings/typescript) PolicyStore wrappers from 0.7.0-beta.1. Every
// op round-trips JSON through the C ABI; Go callers receive typed
// structs (or json.RawMessage for variant-tagged sub-documents the
// wrapper deliberately doesn't decode).

/*
#include <stdlib.h>
#include "agentstategraph.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"unsafe"
)

// Severity — advisory severity on a Policy. Purely metadata; does not
// change decision semantics.
type Severity string

const (
	SeverityLow      Severity = "low"
	SeverityMedium   Severity = "medium"
	SeverityHigh     Severity = "high"
	SeverityCritical Severity = "critical"
)

// AuthorizedAction — one allow / deny rule.
type AuthorizedAction struct {
	Action        string   `json:"action"`
	Condition     *string  `json:"condition,omitempty"`
	Preconditions []string `json:"preconditions,omitempty"`
}

// ApprovalRule — one require-approval rule. `Fallback` is left as a
// raw JSON document (tagged `{"kind": ...}`) so callers can unmarshal
// into the variant they care about.
type ApprovalRule struct {
	Action    string          `json:"action"`
	Approvers []string        `json:"approvers"`
	// Timeout — duration encoded as milliseconds (see
	// agentstategraph-policy duration_opt serde helper).
	Timeout  *uint64         `json:"timeout,omitempty"`
	Fallback json.RawMessage `json:"fallback"`
}

// ProcedureStep — one step in a Policy's procedure.
type ProcedureStep struct {
	Action           string  `json:"action"`
	IfPreviousFailed *string `json:"if_previous_failed,omitempty"`
}

// PolicySignature — optional detached signature metadata recorded on a
// Policy when `agentstategraph_policy_sign` is invoked (POLICY_V1.md
// §5c signing). The fields mirror the Rust `PolicySignature` serde
// payload produced by the FFI.
type PolicySignature struct {
	Algorithm    string `json:"algorithm"`
	SignerKeyID  string `json:"signer_key_id"`
	SignatureHex string `json:"signature_hex"`
}

// EvaluatorSource — tagged union describing where an external
// evaluator's body comes from. `Kind` selects one of:
//   - "inline":      Body is set to the evaluator source text.
//   - "file_path":   Path is set to a filesystem path.
//   - "commit_ref":  Path is set to a repo-relative path resolved
//                    against a commit.
type EvaluatorSource struct {
	Kind string  `json:"kind"`
	Body *string `json:"body,omitempty"`
	Path *string `json:"path,omitempty"`
}

// ExternalEvaluatorRef — optional reference to an external evaluator
// engine that replaces the built-in rule matcher for a Policy. `Kind`
// is one of "rego", "cedar", "wasm"; `Source` locates the body.
type ExternalEvaluatorRef struct {
	Kind   string          `json:"kind"`
	Source EvaluatorSource `json:"source"`
}

// Policy — the unit of authorization + procedure. Matches
// agentstategraph-policy::Policy.
type Policy struct {
	Path              string             `json:"path"`
	Version           uint64             `json:"version"`
	Situation         string             `json:"situation"`
	SituationSelector json.RawMessage    `json:"situation_selector"`
	Allow             []AuthorizedAction `json:"allow,omitempty"`
	Deny              []AuthorizedAction `json:"deny,omitempty"`
	RequireApproval   []ApprovalRule     `json:"require_approval,omitempty"`
	Procedure         []ProcedureStep    `json:"procedure,omitempty"`
	Triggers          []string           `json:"triggers,omitempty"`
	RequiredFields    []string           `json:"required_fields,omitempty"`
	Severity          Severity           `json:"severity"`
	ProposedBy        string             `json:"proposed_by"`
	ProposedAt        string             `json:"proposed_at"`
	RatifiedBy        *string            `json:"ratified_by,omitempty"`
	RatifiedAt        *string            `json:"ratified_at,omitempty"`
	RatificationReasoning *string        `json:"ratification_reasoning,omitempty"`
	ActiveFrom        string             `json:"active_from"`
	ExpiresAt         *string            `json:"expires_at,omitempty"`
	Supersedes        *string            `json:"supersedes,omitempty"`
	// Signature — optional detached signature (0.7.5-beta.1 §5c).
	Signature *PolicySignature `json:"signature,omitempty"`
	// TenantID — optional multi-tenant scope. When set, the policy is
	// only evaluated for sessions whose `scope_tenant` matches.
	TenantID *string `json:"tenant_id,omitempty"`
	// ExternalEvaluator — optional pointer to an external evaluator
	// (Rego/Cedar/Wasm) that replaces the built-in matcher.
	ExternalEvaluator *ExternalEvaluatorRef `json:"external_evaluator,omitempty"`
}

// Session — advisory session context carried by higher-level
// transports (MCP, REST) when invoking policy evaluation. The Go FFI
// externs don't yet accept a Session directly; this struct is provided
// for callers who marshal their own session-scoped payloads or
// post-filter results themselves.
type Session struct {
	// ScopeTenant — tenant id used to filter policies whose
	// `tenant_id` is non-nil. A nil value means "no filter".
	ScopeTenant *string `json:"scope_tenant,omitempty"`
}

// DecisionKind — one of the four Decision variants.
type DecisionKind string

const (
	DecisionAllow           DecisionKind = "allow"
	DecisionDeny            DecisionKind = "deny"
	DecisionRequireApproval DecisionKind = "require_approval"
	DecisionNoPolicyMatch   DecisionKind = "no_policy_match"
)

// Decision — result of evaluate / evaluate_change. The decoded shape
// merges every variant's fields; consult Kind first, then only read
// the fields relevant to that variant. Fallback is raw JSON.
type Decision struct {
	Kind             DecisionKind    `json:"kind"`
	MatchedPolicy    string          `json:"matched_policy,omitempty"`
	Reason           string          `json:"reason,omitempty"`
	Preconditions    []string        `json:"preconditions,omitempty"`
	Approvers        []string        `json:"approvers,omitempty"`
	Timeout          *uint64         `json:"timeout,omitempty"`
	Fallback         json.RawMessage `json:"fallback,omitempty"`
	ApprovalTaskPath *string         `json:"approval_task_path,omitempty"`
}

// ChangeProposal — a proposed change evaluated against change-cost
// policies via EvaluateChange (POLICY_V1.md §22.2).
type ChangeProposal struct {
	Action          string            `json:"action"`
	AgentID         string            `json:"agent_id"`
	Intent          string            `json:"intent"`
	PreferredOption string            `json:"preferred_option"`
	Alternatives    []string          `json:"alternatives,omitempty"`
	Tokens          []string          `json:"tokens,omitempty"`
	AttachedFields  map[string]string `json:"attached_fields,omitempty"`
}

// PolicyStore — handle bound to an AgentStateGraph repository, path
// prefix, and agent id. All policy writes commit as
// `IntentCategory::Plan`.
type PolicyStore struct {
	handle C.SgPolicyStore
	// repo back-reference: the repository is shared and refcounted.
	// Closing the PolicyStore does NOT close the repository; the
	// field is retained so GC can't reclaim it out from under us.
	repo *AgentStateGraph
}

// NewPolicyStore constructs a PolicyStore on top of an existing
// AgentStateGraph. The repository is shared (refcounted); closing the
// PolicyStore does NOT close the repository.
func NewPolicyStore(asg *AgentStateGraph, prefix, agentID string) (*PolicyStore, error) {
	if asg == nil || asg.repo == nil {
		return nil, errors.New("nil repository")
	}
	cPrefix := C.CString(prefix)
	defer C.free(unsafe.Pointer(cPrefix))
	cAgent := C.CString(agentID)
	defer C.free(unsafe.Pointer(cAgent))

	h := C.agentstategraph_policy_store_new(asg.repo, cPrefix, cAgent)
	if h == nil {
		return nil, errors.New("failed to create policy store")
	}
	return &PolicyStore{handle: h, repo: asg}, nil
}

// Close frees the PolicyStore handle. The underlying repository is
// unaffected.
func (ps *PolicyStore) Close() {
	if ps.handle != nil {
		C.agentstategraph_policy_store_free(ps.handle)
		ps.handle = nil
	}
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

// Propose registers a new (unratified) policy and returns its
// `path@version` handle.
func (ps *PolicyStore) Propose(ref string, policy Policy) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	b, err := json.Marshal(policy)
	if err != nil {
		return "", fmt.Errorf("propose: marshal: %w", err)
	}
	cPolicy := C.CString(string(b))
	defer C.free(unsafe.Pointer(cPolicy))
	raw, err := consume(
		C.agentstategraph_policy_propose(ps.handle, cRef, cPolicy),
		"propose",
	)
	if err != nil {
		return "", err
	}
	var handle string
	if err := decodeOrErr(raw, &handle); err != nil {
		return "", err
	}
	return handle, nil
}

// Ratify promotes an unratified proposal. Reasoning is captured on the
// stored Policy and must be non-empty (enforced by the Rust store).
func (ps *PolicyStore) Ratify(ref, path, ratifier, reasoning string) error {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cRatifier := C.CString(ratifier)
	defer C.free(unsafe.Pointer(cRatifier))
	cReason := C.CString(reasoning)
	defer C.free(unsafe.Pointer(cReason))
	raw, err := consume(
		C.agentstategraph_policy_ratify(ps.handle, cRef, cPath, cRatifier, cReason),
		"ratify",
	)
	if err != nil {
		return err
	}
	// FFI returns {"ok": true} on success or {"error": "..."} on
	// failure. decodeOrErr(nil) surfaces the error envelope.
	return decodeOrErr(raw, nil)
}

// Supersede replaces the active policy at `path` with `newPolicy` and
// returns the new `path@version` handle.
func (ps *PolicyStore) Supersede(ref, path string, newPolicy Policy) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	b, err := json.Marshal(newPolicy)
	if err != nil {
		return "", fmt.Errorf("supersede: marshal: %w", err)
	}
	cPolicy := C.CString(string(b))
	defer C.free(unsafe.Pointer(cPolicy))
	raw, err := consume(
		C.agentstategraph_policy_supersede(ps.handle, cRef, cPath, cPolicy),
		"supersede",
	)
	if err != nil {
		return "", err
	}
	var handle string
	if err := decodeOrErr(raw, &handle); err != nil {
		return "", err
	}
	return handle, nil
}

// List returns every policy under `prefix` (or all when prefix is
// empty). Unratified proposals are included.
func (ps *PolicyStore) List(ref, prefix string) ([]Policy, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	var cPrefix *C.char
	if prefix != "" {
		cPrefix = C.CString(prefix)
		defer C.free(unsafe.Pointer(cPrefix))
	}
	raw, err := consume(
		C.agentstategraph_policy_list(ps.handle, cRef, cPrefix),
		"list",
	)
	if err != nil {
		return nil, err
	}
	var out []Policy
	if err := decodeOrErr(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// Active returns currently-active policies (ratified AND
// `active_from <= now`). `prefix` is optional.
func (ps *PolicyStore) Active(ref, prefix string) ([]Policy, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	var cPrefix *C.char
	if prefix != "" {
		cPrefix = C.CString(prefix)
		defer C.free(unsafe.Pointer(cPrefix))
	}
	raw, err := consume(
		C.agentstategraph_policy_active(ps.handle, cRef, cPrefix),
		"active",
	)
	if err != nil {
		return nil, err
	}
	var out []Policy
	if err := decodeOrErr(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// Get fetches the active (or latest proposed) policy at `path`.
func (ps *PolicyStore) Get(ref, path string) (*Policy, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	raw, err := consume(
		C.agentstategraph_policy_get(ps.handle, cRef, cPath),
		"get",
	)
	if err != nil {
		return nil, err
	}
	var p Policy
	if err := decodeOrErr(raw, &p); err != nil {
		return nil, err
	}
	return &p, nil
}

// History walks the supersedes chain for `path`, returning entries
// oldest-first through the current version.
func (ps *PolicyStore) History(ref, path string) ([]Policy, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	raw, err := consume(
		C.agentstategraph_policy_history(ps.handle, cRef, cPath),
		"history",
	)
	if err != nil {
		return nil, err
	}
	var out []Policy
	if err := decodeOrErr(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}

// Evaluate runs the authorization evaluator (POLICY_V1.md §5).
// `situation` is a flat fact map; `action` and `agentID` identify the
// candidate call site.
func (ps *PolicyStore) Evaluate(ref string, situation map[string]string, action, agentID string) (*Decision, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	if situation == nil {
		situation = map[string]string{}
	}
	sitBytes, err := json.Marshal(situation)
	if err != nil {
		return nil, fmt.Errorf("evaluate: marshal situation: %w", err)
	}
	cSit := C.CString(string(sitBytes))
	defer C.free(unsafe.Pointer(cSit))
	cAction := C.CString(action)
	defer C.free(unsafe.Pointer(cAction))
	cAgent := C.CString(agentID)
	defer C.free(unsafe.Pointer(cAgent))
	raw, err := consume(
		C.agentstategraph_policy_evaluate(ps.handle, cRef, cSit, cAction, cAgent),
		"evaluate",
	)
	if err != nil {
		return nil, err
	}
	var d Decision
	if err := decodeOrErr(raw, &d); err != nil {
		return nil, err
	}
	return &d, nil
}

// EvaluateChange runs the change-proposal evaluator
// (POLICY_V1.md §22.2).
func (ps *PolicyStore) EvaluateChange(ref string, proposal ChangeProposal) (*Decision, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	b, err := json.Marshal(proposal)
	if err != nil {
		return nil, fmt.Errorf("evaluate_change: marshal: %w", err)
	}
	cProp := C.CString(string(b))
	defer C.free(unsafe.Pointer(cProp))
	raw, err := consume(
		C.agentstategraph_policy_evaluate_change(ps.handle, cRef, cProp),
		"evaluate_change",
	)
	if err != nil {
		return nil, err
	}
	var d Decision
	if err := decodeOrErr(raw, &d); err != nil {
		return nil, err
	}
	return &d, nil
}

// Sign invokes the `agentstategraph_policy_sign` FFI extern, which
// (as of 0.7.5-beta.1) returns a stub JSON envelope describing the
// signature that would be computed. The raw JSON string is returned
// unchanged so callers can parse whichever shape the FFI ships with
// (either `{"signature": {...}}` or `{"error": "..."}`).
//
// signerKeyID is optional; pass nil to let the FFI pick a default.
func (ps *PolicyStore) Sign(ref, path string, signerKeyID *string) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var cKey *C.char
	if signerKeyID != nil {
		cKey = C.CString(*signerKeyID)
		defer C.free(unsafe.Pointer(cKey))
	}
	raw, err := consume(
		C.agentstategraph_policy_sign(ps.handle, cRef, cPath, cKey),
		"policy_sign",
	)
	if err != nil {
		return "", err
	}
	return raw, nil
}

// Verify invokes the `agentstategraph_policy_verify` FFI extern. The
// raw JSON envelope is returned unchanged; callers parse the shape
// that matches the currently-shipped stub.
func (ps *PolicyStore) Verify(ref, path string) (string, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	raw, err := consume(
		C.agentstategraph_policy_verify(ps.handle, cRef, cPath),
		"policy_verify",
	)
	if err != nil {
		return "", err
	}
	return raw, nil
}

// SetExternalEvaluator invokes the
// `agentstategraph_policy_set_external_evaluator` FFI extern. The
// current Rust implementation is a stub that returns an
// `{"error": "..."}` envelope; the raw string is returned unchanged.
func (ps *PolicyStore) SetExternalEvaluator(configJSON string) (string, error) {
	cCfg := C.CString(configJSON)
	defer C.free(unsafe.Pointer(cCfg))
	raw, err := consume(
		C.agentstategraph_policy_set_external_evaluator(ps.handle, cCfg),
		"policy_set_external_evaluator",
	)
	if err != nil {
		return "", err
	}
	return raw, nil
}

// ---------------------------------------------------------------------------
// Tenant-scoped variants (0.7.5-beta.1 §5c)
//
// The FFI externs for evaluate / evaluate_change / list / active do
// not yet accept a `tenant_filter` argument — only the Rust MCP layer
// consumes it today. As a pass-through, the Scoped variants call the
// existing externs and filter Go-side by `tenant_id`.
//
// A policy matches the filter when:
//   - tenantFilter is nil (no filter), OR
//   - policy.TenantID is nil (policy applies to all tenants), OR
//   - *policy.TenantID == *tenantFilter.
//
// TODO(0.7.6+): when the FFI grows tenant-aware externs, switch these
// methods to pass tenantFilter through the C ABI directly.
// ---------------------------------------------------------------------------

// tenantMatches returns true iff `p` is visible under `filter`.
func tenantMatches(p *Policy, filter *string) bool {
	if filter == nil || p.TenantID == nil {
		return true
	}
	return *p.TenantID == *filter
}

// EvaluateScoped dispatches to the scoped FFI extern
// (agentstategraph_policy_evaluate_scoped, 0.7.5 §3b). `tenantFilter
// = nil` is equivalent to Evaluate; `tenantFilter = &"acme"` matches
// policies whose `tenant_id` is either `"acme"` or absent (global
// fallback). Pre-0.7.5 bindings had to post-filter a single decision
// client-side, which couldn't redirect to a global fallback when the
// matched policy failed the filter — this variant solves that by
// filtering candidates before evaluation.
func (ps *PolicyStore) EvaluateScoped(ref string, situation map[string]string, action, agentID string, tenantFilter *string) (*Decision, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	if situation == nil {
		situation = map[string]string{}
	}
	sitBytes, err := json.Marshal(situation)
	if err != nil {
		return nil, fmt.Errorf("marshal situation: %w", err)
	}
	cSit := C.CString(string(sitBytes))
	defer C.free(unsafe.Pointer(cSit))
	cAction := C.CString(action)
	defer C.free(unsafe.Pointer(cAction))
	cAgent := C.CString(agentID)
	defer C.free(unsafe.Pointer(cAgent))
	var cTenant *C.char
	if tenantFilter != nil {
		cTenant = C.CString(*tenantFilter)
		defer C.free(unsafe.Pointer(cTenant))
	}
	raw, err := consume(
		C.agentstategraph_policy_evaluate_scoped(ps.handle, cRef, cSit, cAction, cAgent, cTenant),
		"evaluate_scoped",
	)
	if err != nil {
		return nil, err
	}
	var d Decision
	if err := decodeOrErr(raw, &d); err != nil {
		return nil, err
	}
	return &d, nil
}

// EvaluateChangeScoped dispatches to the scoped FFI extern
// (agentstategraph_policy_evaluate_change_scoped, 0.7.5 §3b). See
// EvaluateScoped for tenantFilter semantics.
func (ps *PolicyStore) EvaluateChangeScoped(ref string, proposal ChangeProposal, tenantFilter *string) (*Decision, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	proposalBytes, err := json.Marshal(proposal)
	if err != nil {
		return nil, fmt.Errorf("marshal proposal: %w", err)
	}
	cProp := C.CString(string(proposalBytes))
	defer C.free(unsafe.Pointer(cProp))
	var cTenant *C.char
	if tenantFilter != nil {
		cTenant = C.CString(*tenantFilter)
		defer C.free(unsafe.Pointer(cTenant))
	}
	raw, err := consume(
		C.agentstategraph_policy_evaluate_change_scoped(ps.handle, cRef, cProp, cTenant),
		"evaluate_change_scoped",
	)
	if err != nil {
		return nil, err
	}
	var d Decision
	if err := decodeOrErr(raw, &d); err != nil {
		return nil, err
	}
	return &d, nil
}

// ActiveScoped wraps Active and drops policies whose `tenant_id` is
// non-nil and does not match `tenantFilter`.
func (ps *PolicyStore) ActiveScoped(ref, prefix string, tenantFilter *string) ([]Policy, error) {
	all, err := ps.Active(ref, prefix)
	if err != nil {
		return nil, err
	}
	return filterByTenant(all, tenantFilter), nil
}

// ListScoped wraps List with the same post-load tenant filter.
func (ps *PolicyStore) ListScoped(ref, prefix string, tenantFilter *string) ([]Policy, error) {
	all, err := ps.List(ref, prefix)
	if err != nil {
		return nil, err
	}
	return filterByTenant(all, tenantFilter), nil
}

func filterByTenant(in []Policy, filter *string) []Policy {
	if filter == nil {
		return in
	}
	out := make([]Policy, 0, len(in))
	for i := range in {
		if tenantMatches(&in[i], filter) {
			out = append(out, in[i])
		}
	}
	return out
}

// CheckTokens returns the active policies whose `triggers` intersect
// `tokens`. Mirrors the internal filter used by EvaluateChange.
func (ps *PolicyStore) CheckTokens(ref string, tokens []string) ([]Policy, error) {
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	if tokens == nil {
		tokens = []string{}
	}
	b, err := json.Marshal(tokens)
	if err != nil {
		return nil, fmt.Errorf("check_tokens: marshal: %w", err)
	}
	cTokens := C.CString(string(b))
	defer C.free(unsafe.Pointer(cTokens))
	raw, err := consume(
		C.agentstategraph_policy_check_tokens(ps.handle, cRef, cTokens),
		"check_tokens",
	)
	if err != nil {
		return nil, err
	}
	var out []Policy
	if err := decodeOrErr(raw, &out); err != nil {
		return nil, err
	}
	return out, nil
}
