package agentstategraph

// Taint / Quarantine / Watch Go binding on top of the AgentStateGraph
// C FFI (0.7.75-beta.1 §9c). Mirrors the surface used by the Python
// and TypeScript bindings, routing every op through the eight
// `agentstategraph_{taint,quarantine,watch,list_taints,check_taint}_*`
// externs declared in `agentstategraph.h`.
//
// Every op round-trips JSON through the C ABI; callers receive typed
// Go structs or a plain `string` id for applies.

/*
#include <stdlib.h>
#include <stdbool.h>
#include "agentstategraph.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Typed Taint JSON shapes (mirrors agentstategraph-taint::types).
// ---------------------------------------------------------------------------

// TaintKind — "taint" | "quarantine" | "watch".
type TaintKind string

const (
	KindTaint      TaintKind = "taint"
	KindQuarantine TaintKind = "quarantine"
	KindWatch      TaintKind = "watch"
)

// TaintEffect — pre-commit-hook behavior.
type TaintEffect string

const (
	EffectWarn     TaintEffect = "warn"
	EffectBlock    TaintEffect = "block"
	EffectReview   TaintEffect = "review"
	EffectIsolate  TaintEffect = "isolate"
	EffectAdvisory TaintEffect = "advisory"
)

// TaintSeverity — advisory severity.
type TaintSeverity string

const (
	TaintSeverityLow      TaintSeverity = "low"
	TaintSeverityMedium   TaintSeverity = "medium"
	TaintSeverityHigh     TaintSeverity = "high"
	TaintSeverityCritical TaintSeverity = "critical"
)

// WatchDirection — "above" | "below".
type WatchDirection string

const (
	WatchAbove WatchDirection = "above"
	WatchBelow WatchDirection = "below"
)

// Taint — on-disk / over-wire shape of a single taint record.
type Taint struct {
	ID             string                     `json:"id"`
	Path           string                     `json:"path"`
	Name           string                     `json:"name"`
	Kind           TaintKind                  `json:"kind"`
	Effect         TaintEffect                `json:"effect"`
	Severity       TaintSeverity              `json:"severity"`
	Reason         string                     `json:"reason"`
	AgentID        string                     `json:"agent_id"`
	CommitID       string                     `json:"commit_id"`
	CreatedAt      string                     `json:"created_at"`
	ExpiresAt      *string                    `json:"expires_at,omitempty"`
	ResolvedAt     *string                    `json:"resolved_at,omitempty"`
	ResolvedBy     *string                    `json:"resolved_by,omitempty"`
	ResolvedReason *string                    `json:"resolved_reason,omitempty"`
	ResolvedProof  *string                    `json:"resolved_proof,omitempty"`
	Propagate      bool                       `json:"propagate"`
	Metadata       map[string]json.RawMessage `json:"metadata,omitempty"`
}

// TaintCheck — return shape of CheckTaint.
type TaintCheck struct {
	Tainted             bool     `json:"tainted"`
	Quarantined         bool     `json:"quarantined"`
	Watched             bool     `json:"watched"`
	Taints              []Taint  `json:"taints"`
	Quarantines         []Taint  `json:"quarantines"`
	Watches             []Taint  `json:"watches"`
	CanWrite            bool     `json:"can_write"`
	RequiredConfidence  float64  `json:"required_confidence"`
	AuthorizedAgents    []string `json:"authorized_agents,omitempty"`
	Isolated            bool     `json:"isolated"`
}

// ---------------------------------------------------------------------------
// Parameter shapes. Field names match what the FFI param parser reads
// in crates/agentstategraph-ffi/src/lib.rs (taint_apply / quarantine_apply
// / watch_apply / *_remove). `expires` (RFC3339) is the FFI key —
// not `expires_at` — hence the explicit tag here.
// ---------------------------------------------------------------------------

// TaintParams — input to Taint().
type TaintParams struct {
	Name      string        `json:"name"`
	Effect    TaintEffect   `json:"effect"`
	Reason    string        `json:"reason"`
	Severity  TaintSeverity `json:"severity,omitempty"`
	Expires   *string       `json:"expires,omitempty"`
	Propagate *bool         `json:"propagate,omitempty"`
	AgentID   string        `json:"agent_id"`
}

// QuarantineParams — input to Quarantine().
type QuarantineParams struct {
	Name              string        `json:"name"`
	Reason            string        `json:"reason"`
	Severity          TaintSeverity `json:"severity,omitempty"`
	AuthorizedAgents  []string      `json:"authorized_agents"`
	Expires           *string       `json:"expires,omitempty"`
	Propagate         *bool         `json:"propagate,omitempty"`
	AgentID           string        `json:"agent_id"`
}

// WatchParams — input to Watch().
type WatchParams struct {
	Name              string         `json:"name"`
	Reason            string         `json:"reason"`
	Metric            *string        `json:"metric,omitempty"`
	Threshold         *float64       `json:"threshold,omitempty"`
	Direction         WatchDirection `json:"direction,omitempty"`
	CheckIntervalSecs *uint64        `json:"check_interval_secs,omitempty"`
	Expires           *string        `json:"expires,omitempty"`
	Severity          TaintSeverity  `json:"severity,omitempty"`
	Propagate         *bool          `json:"propagate,omitempty"`
	AgentID           string         `json:"agent_id"`
}

// UntaintParams — input to Untaint() and Unquarantine(). The FFI
// extern reads `name` out of the params payload, so callers pass it
// via the method arg and this struct carries the remainder.
type UntaintParams struct {
	Reason  string  `json:"reason"`
	Proof   *string `json:"proof,omitempty"`
	AgentID string  `json:"agent_id"`
}

// UnwatchParams — input to Unwatch(). Watches are lightweight so
// reason is optional.
type UnwatchParams struct {
	Reason  *string `json:"reason,omitempty"`
	AgentID string  `json:"agent_id"`
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

// applyEnvelope decodes the `{"ok":true,"id":"<uuid>"}` envelope used
// by taint_apply / quarantine_apply / watch_apply.
type applyEnvelope struct {
	OK    bool   `json:"ok"`
	ID    string `json:"id"`
	Error string `json:"error"`
}

func decodeApply(raw string) (string, error) {
	var env applyEnvelope
	if err := json.Unmarshal([]byte(raw), &env); err != nil {
		return "", fmt.Errorf("decode apply envelope: %w", err)
	}
	if env.Error != "" {
		return "", errors.New(env.Error)
	}
	if !env.OK {
		return "", errors.New("ffi returned ok=false without error")
	}
	return env.ID, nil
}

// decodeOK decodes the `{"ok":true}` / `{"error":"..."}` envelope used
// by *_remove / release / unwatch.
func decodeOK(raw string) error {
	var env applyEnvelope
	if err := json.Unmarshal([]byte(raw), &env); err != nil {
		return fmt.Errorf("decode ok envelope: %w", err)
	}
	if env.Error != "" {
		return errors.New(env.Error)
	}
	if !env.OK {
		return errors.New("ffi returned ok=false without error")
	}
	return nil
}

// mergeNameIntoParams re-marshals the UntaintParams / UnwatchParams
// payload with an injected `name` field so the Rust FFI param parser
// (which reads `name` from the JSON body, not a dedicated arg) sees it.
func mergeNameIntoParams(params interface{}, name string) (string, error) {
	b, err := json.Marshal(params)
	if err != nil {
		return "", fmt.Errorf("marshal params: %w", err)
	}
	var m map[string]json.RawMessage
	if err := json.Unmarshal(b, &m); err != nil {
		return "", fmt.Errorf("remarshal params: %w", err)
	}
	nb, err := json.Marshal(name)
	if err != nil {
		return "", err
	}
	m["name"] = nb
	out, err := json.Marshal(m)
	if err != nil {
		return "", err
	}
	return string(out), nil
}

// ---------------------------------------------------------------------------
// Repository methods
// ---------------------------------------------------------------------------

// Taint applies a taint on `path` at `refName` with the given
// parameters. Returns the new taint's uuid.
func (r *AgentStateGraph) Taint(refName, path string, params TaintParams) (string, error) {
	b, err := json.Marshal(params)
	if err != nil {
		return "", fmt.Errorf("taint: marshal: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(string(b))
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_taint_apply(r.repo, cRef, cPath, cParams),
		"taint_apply",
	)
	if err != nil {
		return "", err
	}
	return decodeApply(raw)
}

// Untaint removes an active taint by name.
func (r *AgentStateGraph) Untaint(refName, path, name string, params UntaintParams) error {
	payload, err := mergeNameIntoParams(params, name)
	if err != nil {
		return fmt.Errorf("untaint: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(payload)
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_taint_remove(r.repo, cRef, cPath, cParams),
		"taint_remove",
	)
	if err != nil {
		return err
	}
	return decodeOK(raw)
}

// Quarantine applies a quarantine on `path`. Returns the new taint id.
func (r *AgentStateGraph) Quarantine(refName, path string, params QuarantineParams) (string, error) {
	if params.AuthorizedAgents == nil {
		params.AuthorizedAgents = []string{}
	}
	b, err := json.Marshal(params)
	if err != nil {
		return "", fmt.Errorf("quarantine: marshal: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(string(b))
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_quarantine_apply(r.repo, cRef, cPath, cParams),
		"quarantine_apply",
	)
	if err != nil {
		return "", err
	}
	return decodeApply(raw)
}

// Unquarantine releases an active quarantine by name.
func (r *AgentStateGraph) Unquarantine(refName, path, name string, params UntaintParams) error {
	payload, err := mergeNameIntoParams(params, name)
	if err != nil {
		return fmt.Errorf("unquarantine: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(payload)
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_quarantine_release(r.repo, cRef, cPath, cParams),
		"quarantine_release",
	)
	if err != nil {
		return err
	}
	return decodeOK(raw)
}

// Watch attaches a watch to `path`. Returns the new taint id.
func (r *AgentStateGraph) Watch(refName, path string, params WatchParams) (string, error) {
	b, err := json.Marshal(params)
	if err != nil {
		return "", fmt.Errorf("watch: marshal: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(string(b))
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_watch_apply(r.repo, cRef, cPath, cParams),
		"watch_apply",
	)
	if err != nil {
		return "", err
	}
	return decodeApply(raw)
}

// Unwatch removes an active watch by name.
func (r *AgentStateGraph) Unwatch(refName, path, name string, params UnwatchParams) error {
	payload, err := mergeNameIntoParams(params, name)
	if err != nil {
		return fmt.Errorf("unwatch: %w", err)
	}
	cRef := C.CString(refName)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cParams := C.CString(payload)
	defer C.free(unsafe.Pointer(cParams))
	raw, err := consume(
		C.agentstategraph_watch_remove(r.repo, cRef, cPath, cParams),
		"watch_remove",
	)
	if err != nil {
		return err
	}
	return decodeOK(raw)
}

// listTaintsEnvelope matches `{"ok":true,"taints":[...]}`.
type listTaintsEnvelope struct {
	OK     bool    `json:"ok"`
	Taints []Taint `json:"taints"`
	Error  string  `json:"error"`
}

// ListTaints returns every active taint (or all if includeResolved).
// Both filters are optional — pass nil to skip.
func (r *AgentStateGraph) ListTaints(pathPrefix, kind *string, includeResolved bool) ([]Taint, error) {
	var cPrefix *C.char
	if pathPrefix != nil {
		cPrefix = C.CString(*pathPrefix)
		defer C.free(unsafe.Pointer(cPrefix))
	}
	var cKind *C.char
	if kind != nil {
		cKind = C.CString(*kind)
		defer C.free(unsafe.Pointer(cKind))
	}
	raw, err := consume(
		C.agentstategraph_list_taints(r.repo, cPrefix, cKind, C.bool(includeResolved)),
		"list_taints",
	)
	if err != nil {
		return nil, err
	}
	var env listTaintsEnvelope
	if err := json.Unmarshal([]byte(raw), &env); err != nil {
		return nil, fmt.Errorf("list_taints: decode: %w", err)
	}
	if env.Error != "" {
		return nil, errors.New(env.Error)
	}
	return env.Taints, nil
}

// checkTaintEnvelope matches `{"ok":true,"check":{...}}`.
type checkTaintEnvelope struct {
	OK    bool        `json:"ok"`
	Check *TaintCheck `json:"check"`
	Error string      `json:"error"`
}

// CheckTaint returns the aggregated taint/quarantine/watch status for
// `path` under the supplied `agentID` + `confidence`.
func (r *AgentStateGraph) CheckTaint(path, agentID string, confidence float64) (*TaintCheck, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cAgent := C.CString(agentID)
	defer C.free(unsafe.Pointer(cAgent))
	raw, err := consume(
		C.agentstategraph_check_taint(r.repo, cPath, cAgent, C.double(confidence)),
		"check_taint",
	)
	if err != nil {
		return nil, err
	}
	var env checkTaintEnvelope
	if err := json.Unmarshal([]byte(raw), &env); err != nil {
		return nil, fmt.Errorf("check_taint: decode: %w", err)
	}
	if env.Error != "" {
		return nil, errors.New(env.Error)
	}
	if env.Check == nil {
		return nil, errors.New("check_taint: missing check envelope")
	}
	return env.Check, nil
}
