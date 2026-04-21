package agentstategraph

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestNewMemory(t *testing.T) {
	asg, err := NewMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer asg.Close()
}

func TestSetAndGet(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	_, err := asg.Set("/name", `"my-cluster"`, "Checkpoint", "init")
	if err != nil {
		t.Fatal(err)
	}

	val, err := asg.Get("/name")
	if err != nil {
		t.Fatal(err)
	}
	if val != `"my-cluster"` {
		t.Fatalf("expected \"my-cluster\", got %s", val)
	}
}

func TestSetJSON(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	config := map[string]interface{}{
		"nodes": 3,
		"gpu":   true,
	}
	_, err := asg.SetJSON("/config", config, "Checkpoint", "set config")
	if err != nil {
		t.Fatal(err)
	}

	val, err := asg.Get("/config")
	if err != nil {
		t.Fatal(err)
	}

	var result map[string]interface{}
	json.Unmarshal([]byte(val), &result)
	if result["gpu"] != true {
		t.Fatalf("expected gpu=true, got %v", result["gpu"])
	}
}

func TestBranchAndDiff(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	asg.Set("/x", "1", "Checkpoint", "init")
	asg.Branch("feature", "main")
	asg.Set("/x", "2", "Explore", "try new value", "feature")

	diff, err := asg.Diff("main", "feature")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(diff, "SetValue") {
		t.Fatalf("expected SetValue in diff, got %s", diff)
	}
}

func TestMerge(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	asg.Set("/a", "1", "Checkpoint", "init a")
	asg.Set("/b", "2", "Checkpoint", "init b")
	asg.Branch("feature", "main")

	asg.Set("/a", "10", "Refine", "update a on main")
	asg.Set("/b", "20", "Refine", "update b on feature", "feature")

	_, err := asg.Merge("feature", "main", "merge feature")
	if err != nil {
		t.Fatal(err)
	}

	a, _ := asg.Get("/a")
	b, _ := asg.Get("/b")
	if a != "10" {
		t.Fatalf("expected a=10, got %s", a)
	}
	if b != "20" {
		t.Fatalf("expected b=20, got %s", b)
	}
}

func TestLog(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	asg.Set("/a", "1", "Checkpoint", "first")
	asg.Set("/b", "2", "Checkpoint", "second")

	log, err := asg.Log(10)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(log, "second") {
		t.Fatalf("expected 'second' in log, got %s", log)
	}
}

func TestBlame(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	asg.Set("/status", `"healthy"`, "Fix", "mark healthy")

	blame, err := asg.Blame("/status")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(blame, "mark healthy") {
		t.Fatalf("expected 'mark healthy' in blame, got %s", blame)
	}
}

func TestDelete(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	asg.Set("/temp", `"value"`, "Checkpoint", "add")
	asg.Delete("/temp", "Fix", "remove temp")

	_, err := asg.Get("/temp")
	if err == nil {
		t.Fatal("expected error getting deleted path")
	}
}

func TestListBranches(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	if _, err := asg.Branch("feature-x", "main"); err != nil {
		t.Fatal(err)
	}
	if _, err := asg.Branch("feature-y", "main"); err != nil {
		t.Fatal(err)
	}

	entries, err := asg.ListBranches("")
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) < 3 {
		t.Fatalf("expected at least 3 branches (main, feature-x, feature-y), got %+v", entries)
	}

	// Prefix filter
	feats, err := asg.ListBranches("feature")
	if err != nil {
		t.Fatal(err)
	}
	if len(feats) != 2 {
		t.Fatalf("expected 2 feature branches, got %+v", feats)
	}
}

func TestDeleteBranch(t *testing.T) {
	asg, _ := NewMemory()
	defer asg.Close()

	if _, err := asg.Branch("temp", "main"); err != nil {
		t.Fatal(err)
	}

	deleted, err := asg.DeleteBranch("temp")
	if err != nil {
		t.Fatal(err)
	}
	if !deleted {
		t.Fatal("expected deleted=true for existing branch")
	}

	// Second delete returns false, no error
	deleted, err = asg.DeleteBranch("temp")
	if err != nil {
		t.Fatal(err)
	}
	if deleted {
		t.Fatal("expected deleted=false for missing branch")
	}
}
