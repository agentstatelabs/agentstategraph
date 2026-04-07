// Package stategraph provides Go bindings for StateGraph,
// an AI-native versioned state store for intent-based systems.
//
// Usage:
//
//	sg := stategraph.NewMemory()
//	defer sg.Close()
//	sg.Set("/name", `"my-cluster"`, "Checkpoint", "init")
//	val := sg.Get("/name")
package stategraph

/*
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -lagentstategraph_ffi
#include <stdlib.h>

typedef void* SgRepo;

extern SgRepo stategraph_new_memory();
extern SgRepo stategraph_new_sqlite(const char* path);
extern void stategraph_free(SgRepo repo);
extern void stategraph_free_string(char* s);

extern char* stategraph_get(SgRepo repo, const char* ref_name, const char* path);
extern char* stategraph_set(SgRepo repo, const char* ref_name, const char* path,
    const char* json_value, const char* intent_category, const char* intent_description);
extern char* stategraph_delete(SgRepo repo, const char* ref_name, const char* path,
    const char* intent_category, const char* intent_description);
extern char* stategraph_branch(SgRepo repo, const char* name, const char* from);
extern char* stategraph_diff(SgRepo repo, const char* ref_a, const char* ref_b);
extern char* stategraph_merge(SgRepo repo, const char* source, const char* target,
    const char* description);
extern char* stategraph_log(SgRepo repo, const char* ref_name, unsigned int limit);
extern char* stategraph_blame(SgRepo repo, const char* ref_name, const char* path);
*/
import "C"
import (
	"encoding/json"
	"errors"
	"strings"
	"unsafe"
)

// StateGraph is a handle to a StateGraph repository.
type StateGraph struct {
	repo C.SgRepo
}

// NewMemory creates a new in-memory StateGraph (ephemeral).
func NewMemory() (*StateGraph, error) {
	repo := C.stategraph_new_memory()
	if repo == nil {
		return nil, errors.New("failed to create memory repository")
	}
	return &StateGraph{repo: repo}, nil
}

// NewSQLite creates a new SQLite-backed StateGraph (durable).
func NewSQLite(path string) (*StateGraph, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	repo := C.stategraph_new_sqlite(cPath)
	if repo == nil {
		return nil, errors.New("failed to create SQLite repository")
	}
	return &StateGraph{repo: repo}, nil
}

// Close frees the repository handle.
func (sg *StateGraph) Close() {
	if sg.repo != nil {
		C.stategraph_free(sg.repo)
		sg.repo = nil
	}
}

// Get returns the JSON value at a path.
func (sg *StateGraph) Get(path string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))

	result := C.stategraph_get(sg.repo, cRef, cPath)
	if result == nil {
		return "", errors.New("get failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Set writes a JSON value at a path, creating a commit.
func (sg *StateGraph) Set(path, jsonValue, category, description string, refs ...string) (string, error) {
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

	result := C.stategraph_set(sg.repo, cRef, cPath, cVal, cCat, cDesc)
	if result == nil {
		return "", errors.New("set failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Delete removes a value at a path, creating a commit.
func (sg *StateGraph) Delete(path, category, description string, refs ...string) (string, error) {
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

	result := C.stategraph_delete(sg.repo, cRef, cPath, cCat, cDesc)
	if result == nil {
		return "", errors.New("delete failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Branch creates a new branch from a ref.
func (sg *StateGraph) Branch(name, from string) (string, error) {
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	cFrom := C.CString(from)
	defer C.free(unsafe.Pointer(cFrom))

	result := C.stategraph_branch(sg.repo, cName, cFrom)
	if result == nil {
		return "", errors.New("branch failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Diff computes a structured diff between two refs.
func (sg *StateGraph) Diff(refA, refB string) (string, error) {
	cA := C.CString(refA)
	defer C.free(unsafe.Pointer(cA))
	cB := C.CString(refB)
	defer C.free(unsafe.Pointer(cB))

	result := C.stategraph_diff(sg.repo, cA, cB)
	if result == nil {
		return "", errors.New("diff failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Merge merges source branch into target.
func (sg *StateGraph) Merge(source, target, description string) (string, error) {
	cSrc := C.CString(source)
	defer C.free(unsafe.Pointer(cSrc))
	cTgt := C.CString(target)
	defer C.free(unsafe.Pointer(cTgt))
	cDesc := C.CString(description)
	defer C.free(unsafe.Pointer(cDesc))

	result := C.stategraph_merge(sg.repo, cSrc, cTgt, cDesc)
	if result == nil {
		return "", errors.New("merge failed")
	}
	defer C.stategraph_free_string(result)
	s := C.GoString(result)
	if strings.HasPrefix(s, "error:") {
		return "", errors.New(s)
	}
	return s, nil
}

// Log returns the commit log as a JSON string.
func (sg *StateGraph) Log(limit uint32, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))

	result := C.stategraph_log(sg.repo, cRef, C.uint(limit))
	if result == nil {
		return "", errors.New("log failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// Blame returns who last modified a path and why.
func (sg *StateGraph) Blame(path string, refs ...string) (string, error) {
	ref := "main"
	if len(refs) > 0 {
		ref = refs[0]
	}
	cRef := C.CString(ref)
	defer C.free(unsafe.Pointer(cRef))
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))

	result := C.stategraph_blame(sg.repo, cRef, cPath)
	if result == nil {
		return "", errors.New("blame failed")
	}
	defer C.stategraph_free_string(result)
	return C.GoString(result), nil
}

// SetJSON is a convenience method that marshals a Go value to JSON before setting.
func (sg *StateGraph) SetJSON(path string, value interface{}, category, description string, refs ...string) (string, error) {
	jsonBytes, err := json.Marshal(value)
	if err != nil {
		return "", err
	}
	return sg.Set(path, string(jsonBytes), category, description, refs...)
}
