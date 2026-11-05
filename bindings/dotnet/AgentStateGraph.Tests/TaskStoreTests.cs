using System;
using System.Linq;
using System.Text.Json;
using AgentStateGraph;
using Xunit;
using AsgTask = AgentStateGraph.Task;

namespace AgentStateGraph.Tests;

/// <summary>
/// Task / plan coverage for the C# surface. The Python suite only
/// exercises the extension fields (payload / parent_change / on_complete);
/// the C# suite mirrors that plus covers the CRUD + assignment + blocker
/// surface the other bindings have test parity on.
/// </summary>
public sealed class TaskStoreTests
{
    private const string Ref = Repository.DefaultRef;

    [Fact]
    public void CreatePlan_ListPlans_GetPlan()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");

        var plan = ts.CreatePlan(Ref, "p-alpha", "first plan");
        Assert.Equal("p-alpha", plan.Name);
        Assert.Equal("first plan", plan.Description);
        Assert.Equal(PlanStatus.Active, plan.Status);

        var plans = ts.ListPlans(Ref);
        Assert.Contains(plans, x => x.Name == "p-alpha");

        var fetched = ts.GetPlan(Ref, "p-alpha");
        Assert.Equal("p-alpha", fetched.Name);
    }

    [Fact]
    public void ArchivePlan_FlipsStatus()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");

        ts.CreatePlan(Ref, "p", null);
        var archived = ts.ArchivePlan(Ref, "p");
        Assert.Equal(PlanStatus.Archived, archived.Status);
        Assert.NotNull(archived.ArchivedAt);
    }

    [Fact]
    public void DeletePlan_RemovesPlan()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");

        ts.CreatePlan(Ref, "gone", null);
        ts.DeletePlan(Ref, "gone");
        Assert.DoesNotContain(ts.ListPlans(Ref), p => p.Name == "gone");
    }

    [Fact]
    public void AddTask_ListTasks_RoundTrip()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);

        var t = ts.AddTask(Ref, "p", "write docs", Priority.Medium);
        Assert.Equal("write docs", t.Title);
        Assert.Equal(Priority.Medium, t.Priority);
        Assert.Equal(TaskStatus.Pending, t.Status);

        var tasks = ts.ListTasks(Ref, "p");
        Assert.Single(tasks);
        Assert.Equal(t.Id, tasks[0].Id);
    }

    [Fact]
    public void StartTask_FlipsToInProgress()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var t = ts.AddTask(Ref, "p", "run", Priority.Low);

        var started = ts.StartTask(Ref, "p", t.Id);
        Assert.Equal(TaskStatus.InProgress, started.Status);
        Assert.NotNull(started.StartedAt);
    }

    [Fact]
    public void CompleteTask_AttachesProofAndFlipsToDone()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var t = ts.AddTask(Ref, "p", "run", Priority.Low);
        ts.StartTask(Ref, "p", t.Id);

        var done = ts.CompleteTask(Ref, "p", t.Id, Proof.Test("policy_store_tests", "all green"));
        Assert.Equal(TaskStatus.Done, done.Status);
        Assert.NotNull(done.Proof);
        Assert.Equal(ProofKind.Test, done.Proof!.Kind);
        Assert.Equal("policy_store_tests", done.Proof.Value);

        // Finishing the last task leaves the plan open; closing it takes a
        // summary.
        Assert.Equal(PlanStatus.Active, ts.GetPlan(Ref, "p").Status);
        var refused = Assert.Throws<AgentStateGraphException>(() => ts.ClosePlan(Ref, "p", "  "));
        Assert.Contains("summary", refused.Message);
        var closed = ts.ClosePlan(Ref, "p", "ran it");
        Assert.Equal(PlanStatus.Completed, closed.Status);
        Assert.Equal("ran it", closed.Summary);
        Assert.Equal("xunit", closed.ClosedBy);
        Assert.NotNull(closed.ClosedAt);
    }

    [Fact]
    public void ClosePlan_RefusesOpenWork()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var empty = Assert.Throws<AgentStateGraphException>(() => ts.ClosePlan(Ref, "p", "nothing to do"));
        Assert.Contains("no tasks", empty.Message);
        var t = ts.AddTask(Ref, "p", "x", Priority.Medium);
        var open = Assert.Throws<AgentStateGraphException>(() => ts.ClosePlan(Ref, "p", "too early"));
        Assert.Contains(t.Id, open.Message);
        ts.AbandonTask(Ref, "p", t.Id, "not needed");
        Assert.Equal(PlanStatus.Completed, ts.ClosePlan(Ref, "p", "dropped").Status);
    }

    [Fact]
    public void AbandonTask_RecordsReason()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var t = ts.AddTask(Ref, "p", "stale", Priority.Low);

        var abandoned = ts.AbandonTask(Ref, "p", t.Id, "superseded");
        Assert.Equal(TaskStatus.Abandoned, abandoned.Status);
        Assert.Equal("superseded", abandoned.AbandonedReason);
    }

    [Fact]
    public void AssignTask_UnassignTask_RoundTrip()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var t = ts.AddTask(Ref, "p", "work", Priority.Low);

        var assigned = ts.AssignTask(Ref, "p", t.Id, "agent/worker");
        Assert.Equal("agent/worker", assigned.AssignedTo);

        var unassigned = ts.UnassignTask(Ref, "p", t.Id);
        Assert.Null(unassigned.AssignedTo);
    }

    [Fact]
    public void NextTaskFor_ReturnsAssignedMatch()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);

        var mine = ts.AddTask(Ref, "p", "mine", Priority.High);
        ts.AssignTask(Ref, "p", mine.Id, "agent/me");
        ts.AddTask(Ref, "p", "not mine", Priority.High,
            new AddTaskOptions(AssignedTo: "agent/other"));

        var next = ts.NextTaskFor(Ref, "p", "agent/me", includeUnassigned: false);
        Assert.NotNull(next);
        Assert.Equal(mine.Id, next!.Id);
    }

    [Fact]
    public void SetTitle_ReplacesTheTitle()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var a = ts.AddTask(Ref, "p", "a", Priority.Low);

        var updated = ts.SetTitle(Ref, "p", a.Id, "a, retitled");
        Assert.Equal("a, retitled", updated.Title);
        Assert.Equal(a.Status, updated.Status);
    }

    [Fact]
    public void Notes_AddSetAndClear()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        const string brief = "Repo: infra/gitops.\n\n1. Write the manifests.\n2. Ask the user. ✓\n";
        var a = ts.AddTask(Ref, "p", "a", Priority.Low, new AddTaskOptions(Notes: brief));
        Assert.Equal(brief, a.Notes);
        Assert.Null(ts.AddTask(Ref, "p", "b", Priority.Low).Notes);
        var ext = ts.AddTaskWithExtensions(
            Ref, "p", "c", Priority.Low, new AddTaskOptions(Notes: "ext notes"), parentChange: "spec@1");
        Assert.Equal("ext notes", ext.Notes);
        Assert.Equal("spec@1", ext.ParentChange);

        ts.StartTask(Ref, "p", a.Id);
        var edited = ts.SetNotes(Ref, "p", a.Id, "line one\nline two");
        Assert.Equal("line one\nline two", edited.Notes);
        Assert.Equal(TaskStatus.InProgress, edited.Status);
        var refused = Assert.Throws<AgentStateGraphException>(
            () => ts.SetNotes(Ref, "p", a.Id, new string('x', 16 * 1024 + 1)));
        Assert.Contains("16384", refused.Message);
        Assert.Null(ts.SetNotes(Ref, "p", a.Id, null).Notes);
        ts.SetNotes(Ref, "p", a.Id, "again");
        Assert.Null(ts.SetNotes(Ref, "p", a.Id, "").Notes);
    }

    [Fact]
    public void SetBlockers_AcceptsJsonArray()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);
        var a = ts.AddTask(Ref, "p", "blocker", Priority.Low);
        var b = ts.AddTask(Ref, "p", "blocked", Priority.Low);

        var updated = ts.SetBlockers(Ref, "p", b.Id, new[] { a.Id });
        Assert.NotNull(updated.BlockedBy);
        Assert.Contains(a.Id, updated.BlockedBy!);
    }

    /// <summary>
    /// AddTaskWithExtensions now rides on <c>agentstategraph_taskstore_add_task_ex</c>
    /// (landed post-0.7.25-beta.1). Asserts the positive round-trip of
    /// every extension field — payload, parent_change, on_complete — and
    /// mirrors the Python suite's <c>test_task_extension_fields_roundtrip</c>.
    /// </summary>
    [Fact]
    public void AddTaskWithExtensions_RoundTripsAllExtensionFields()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);

        var payload = JsonSerializer.SerializeToElement(new { preferred_option = "spec-7" });
        var t = ts.AddTaskWithExtensions(
            Ref, "p", "with extensions", Priority.High,
            options: null,
            payload: payload,
            parentChange: "spec-7@42",
            onComplete: new OnCompleteHook.PromoteChange());

        Assert.Equal("with extensions", t.Title);
        Assert.Equal("spec-7@42", t.ParentChange);
        Assert.IsType<OnCompleteHook.PromoteChange>(t.OnComplete);
        Assert.NotNull(t.Payload);
        Assert.Equal(JsonValueKind.Object, t.Payload!.Value.ValueKind);
        Assert.Equal("spec-7",
            t.Payload.Value.GetProperty("preferred_option").GetString());
    }

    [Fact]
    public void AddTaskWithExtensions_NamedHook_RoundTrips()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);

        var t = ts.AddTaskWithExtensions(
            Ref, "p", "named hook", Priority.Medium,
            onComplete: new OnCompleteHook.Named("send_report"));

        var named = Assert.IsType<OnCompleteHook.Named>(t.OnComplete);
        Assert.Equal("send_report", named.Name);
    }

    [Fact]
    public void AddTaskWithExtensions_NoExtensions_MatchesAddTask()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ts = new TaskStore(repo, "/plans", "xunit");
        ts.CreatePlan(Ref, "p", null);

        var t = ts.AddTaskWithExtensions(Ref, "p", "plain", Priority.Low);
        Assert.Equal("plain", t.Title);
        Assert.Null(t.ParentChange);
        Assert.Null(t.OnComplete);
        Assert.True(t.Payload is null || t.Payload?.ValueKind == JsonValueKind.Null);
    }
}
