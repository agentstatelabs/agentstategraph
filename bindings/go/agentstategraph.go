// Package agentstategraph provides Go bindings for AgentStateGraph,
// an AI-native versioned state store for intent-based systems.
//
// Usage:
//
//	asg := agentstategraph.NewMemory()
//	defer asg.Close()
//	asg.Set("/name", `"my-cluster"`, "Checkpoint", "init")
//	val := asg.Get("/name")
package agentstategraph

/*
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -lagentstategraph_ffi
#include <stdlib.h>

typedef void* SgRepo;

extern SgRepo agentstategraph_new_memory();
extern SgRepo agentstategraph_new_sqlite(const char* path);
extern void agentstategraph_free(SgRepo repo);
extern void agentstategraph_free_string(char* s);

extern char* agentstategraph_get(SgRepo repo, const char* ref_name, const char* path);
extern char* agentstategraph_set(SgRepo repo, const char* ref_name, const char* path,
    const char* json_value, const char* intent_category, const char* intent_description);
extern char* agentstategraph_delete(SgRepo repo, const char* ref_name, const char* path,
    const char* intent_category, const char* intent_description);
extern char* agentstategraph_branch(SgRepo repo, const char* name, const char* from);
extern char* agentstategraph_diff(SgRepo repo, const char* ref_a, const char* ref_b);
extern char* agentstategraph_merge(SgRepo repo, const char* source, const char* target,
    const char* description);
extern char* agentstategraph_log(SgRepo repo, const char* ref_name, unsigned int limit);
extern char* agentstategraph_blame(SgRepo repo, const char* ref_name, const char* path);
*/
import "C"
import (
	"encoding/json"
	"errors"
	"strings"
	"unsafe"
)

// AgentStateGraph is a handle to an AgentStateGraph repository.
type AgentStateGraph struct {
	repo C.SgRepo
}

// NewMemory creates a new in-memory AgentStateGraph (ephemeral).
func NewMemory() (*AgentStateGraph, error) {
	repo := C.agentstategraph_new_memory()
	if repo == nil {
		return nil, errors.New("failed to create memory repository")
	}
	return &AgentStateGraph{repo: repo}, nil
}

// NewSQLite creates a new SQLite-backed AgentStateGraph (durable).
func NewSQLite(path string) (*AgentStateGraph, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	repo := C.agentstategraph_new_sqlite(cPath)
	if repo == nil {
		return nil, errors.New("failed to create SQLite repository")
	}
	return &AgentStateGraph{repo: repo}, nil
}

// Close frees the repository handle.
func (asg *AgentStateGraph) Close() {
	if asg.repo != nil {
		C.agentstategraph_free(asg.repo)
		asg.repo = nil
	}
}

// Get returns the JSON value at a path.
func (asg *AgentStateGraph) Get(path string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))

	result := C.agentstategraph_get(asg.repo, cRef, cPath)
	if result == nil {
		return "", errors.New("get failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Set writes a JSON value at a path, creating a commit.
func (asg *AgentStateGraph) Set(path, jsonValue, category, description string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cVal := C.CString(jsonValue)
	defer C.free(unsafe.Pointer(cVal))
	cCat := C.CString(category)
	defer C.free(unsafe.Pointer(cCat))
	cDesc := C.CString(description)
	defer C.free(unsafe.Pointer(cDesc))

	result := C.agentstategraph_set(asg.repo, cRef, cPath, cVal, cCat, cDesc)
	if result == nil {
		return "", errors.New("set failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Delete removes a value at a path, creating a commit.
func (asg *AgentStateGraph) Delete(path, category, description string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	cCat := C.CString(category)
	defer C.free(unsafe.Pointer(cCat))
	cDesc := C.CString(description)
	defer C.free(unsafe.Pointer(cDesc))

	result := C.agentstategraph_delete(asg.repo, cRef, cPath, cCat, cDesc)
	if result == nil {
		return "", errors.New("delete failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Branch creates a new branch from a ref.
func (asg *AgentStateGraph) Branch(name, from string) (string, error) {
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	cFrom := C.CString(from)
	defer C.free(unsafe.Pointer(cFrom))

	result := C.agentstategraph_branch(asg.repo, cName, cFrom)
	if result == nil {
		return "", errors.New("branch failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Diff computes a structured diff between two refs.
func (asg *AgentStateGraph) Diff(refA, refB string) (string, error) {
	cA := C.CString(refA)
	defer C.free(unsafe.Pointer(cA))
	cB := C.CString(refB)
	defer C.free(unsafe.Pointer(cB))

	result := C.agentstategraph_diff(asg.repo, cA, cB)
	if result == nil {
		return "", errors.New("diff failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Merge merges source branch into target.
func (asg *AgentStateGraph) Merge(source, target, description string) (string, error) {
	cSrc := C.CString(source)
	defer C.free(unsafe.Pointer(cSrc))
	cTgt := C.CString(target)
	defer C.free(unsafe.Pointer(cTgt))
	cDesc := C.CString(description)
	defer C.free(unsafe.Pointer(cDesc))

	result := C.agentstategraph_merge(asg.repo, cSrc, cTgt, cDesc)
	if result == nil {
		return "", errors.New("merge failed")
	}
	defer C.agentstategraph_free_string(result)
	s := C.GoString(result)
	if strings.HasPrefix(s, "error:") {
		return "", errors.New(s)
	}
	return s, nil
}

// Log returns the commit log as a JSON string.
func (asg *AgentStateGraph) Log(limit uint32, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))

	result := C.agentstategraph_log(asg.repo, cRef, C.uint(limit))
	if result == nil {
		return "", errors.New("log failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// Blame returns who last modified a path and why.
func (asg *AgentStateGraph) Blame(path string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))

	result := C.agentstategraph_blame(asg.repo, cRef, cPath)
	if result == nil {
		return "", errors.New("blame failed")
	}
	defer C.agentstategraph_free_string(result)
	return C.GoString(result), nil
}

// SetJSON is a convenience method that marshals a Go value to JSON before setting.
func (asg *AgentStateGraph) SetJSON(path string, value interface{}, category, description string, refs ...string) (string, error) {
	jsonBytes, err := json.Marshal(value)
	if err != nil {
		return "", err
	}
	return asg.Set(path, string(jsonBytes), category, description, refs...)
}
