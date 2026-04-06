package stategraph

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestNewMemory(t *testing.T) {
	sg, err := NewMemory()
	if err != nil {
		t.Fatal(err)
	}
	defer sg.Close()
}

func TestSetAndGet(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	_, err := sg.Set("/name", `"my-cluster"`, "Checkpoint", "init")
	if err != nil {
		t.Fatal(err)
	}

	val, err := sg.Get("/name")
	if err != nil {
		t.Fatal(err)
	}
	if val != `"my-cluster"` {
		t.Fatalf("expected \"my-cluster\", got %s", val)
	}
}

func TestSetJSON(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	config := map[string]interface{}{
		"nodes": 3,
		"gpu":   true,
	}
	_, err := sg.SetJSON("/config", config, "Checkpoint", "set config")
	if err != nil {
		t.Fatal(err)
	}

	val, err := sg.Get("/config")
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
	sg, _ := NewMemory()
	defer sg.Close()

	sg.Set("/x", "1", "Checkpoint", "init")
	sg.Branch("feature", "main")
	sg.Set("/x", "2", "Explore", "try new value", "feature")

	diff, err := sg.Diff("main", "feature")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(diff, "SetValue") {
		t.Fatalf("expected SetValue in diff, got %s", diff)
	}
}

func TestMerge(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	sg.Set("/a", "1", "Checkpoint", "init a")
	sg.Set("/b", "2", "Checkpoint", "init b")
	sg.Branch("feature", "main")

	sg.Set("/a", "10", "Refine", "update a on main")
	sg.Set("/b", "20", "Refine", "update b on feature", "feature")

	_, err := sg.Merge("feature", "main", "merge feature")
	if err != nil {
		t.Fatal(err)
	}

	a, _ := sg.Get("/a")
	b, _ := sg.Get("/b")
	if a != "10" {
		t.Fatalf("expected a=10, got %s", a)
	}
	if b != "20" {
		t.Fatalf("expected b=20, got %s", b)
	}
}

func TestLog(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	sg.Set("/a", "1", "Checkpoint", "first")
	sg.Set("/b", "2", "Checkpoint", "second")

	log, err := sg.Log(10)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(log, "second") {
		t.Fatalf("expected 'second' in log, got %s", log)
	}
}

func TestBlame(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	sg.Set("/status", `"healthy"`, "Fix", "mark healthy")

	blame, err := sg.Blame("/status")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(blame, "mark healthy") {
		t.Fatalf("expected 'mark healthy' in blame, got %s", blame)
	}
}

func TestDelete(t *testing.T) {
	sg, _ := NewMemory()
	defer sg.Close()

	sg.Set("/temp", `"value"`, "Checkpoint", "add")
	sg.Delete("/temp", "Fix", "remove temp")

	_, err := sg.Get("/temp")
	if err == nil {
		t.Fatal("expected error getting deleted path")
	}
}
