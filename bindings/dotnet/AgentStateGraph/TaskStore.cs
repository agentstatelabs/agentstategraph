using System;
using System.Collections.Generic;
using AgentStateGraph.Interop;

namespace AgentStateGraph;

/// <summary>
/// Plan / task store bound to a <see cref="Repository"/>, path prefix, and
/// agent id. All writes commit as <c>IntentCategory::Plan</c>.
/// </summary>
/// <remarks>
/// The store holds a reference to its owning <see cref="Repository"/> so
/// the repo cannot be disposed out from under it. Disposing the store
/// frees only the task-store handle; the repository lives on.
/// </remarks>
public sealed class TaskStore : IDisposable
{
    private readonly SafeTaskStoreHandle _handle;
    // Hold a strong reference to keep the repo alive for our lifetime.
    private readonly Repository _repo;
    private bool _disposed;

    /// <summary>Creates a new TaskStore on top of <paramref name="repo"/>.</summary>
    public TaskStore(Repository repo, string prefix, string agentId)
    {
        ArgumentNullException.ThrowIfNull(repo);
        ArgumentNullException.ThrowIfNull(prefix);
        ArgumentNullException.ThrowIfNull(agentId);

        var raw = NativeMethods.agentstategraph_taskstore_new(
            repo.Handle.DangerousGetHandle(), prefix, agentId);
        if (raw == IntPtr.Zero)
        {
            throw new AgentStateGraphException("taskstore_new", "failed to create task store");
        }
        _handle = SafeTaskStoreHandle.Adopt(raw);
        _repo = repo;
    }

    private void ThrowIfDisposed()
    {
        if (_disposed)
        {
            throw new ObjectDisposedException(nameof(TaskStore));
        }
    }

    private IntPtr H => _handle.DangerousGetHandle();

    // -----------------------------------------------------------------------
    // Plan operations
    // -----------------------------------------------------------------------

    public Plan CreatePlan(string refName, string name, string? description = null)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_create_plan(H, refName, name, description ?? string.Empty);
        return Json.Deserialize<Plan>(Strings.ConsumeUtf8(ptr), "create_plan");
    }

    public IReadOnlyList<Plan> ListPlans(string refName)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_list_plans(H, refName);
        return Json.Deserialize<List<Plan>>(Strings.ConsumeUtf8(ptr), "list_plans");
    }

    public IReadOnlyList<Plan> ListPlansByStatus(string refName, PlanStatus? status)
    {
        ThrowIfDisposed();
        // Empty string means "all" — matches the Go binding's convention.
        var wire = status switch
        {
            null => string.Empty,
            PlanStatus.Active => "active",
            PlanStatus.Completed => "completed",
            PlanStatus.Archived => "archived",
            _ => status.ToString()!.ToLowerInvariant(),
        };
        var ptr = NativeMethods.agentstategraph_taskstore_list_plans_by_status(H, refName, wire);
        return Json.Deserialize<List<Plan>>(Strings.ConsumeUtf8(ptr), "list_plans_by_status");
    }

    public Plan GetPlan(string refName, string name)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_get_plan(H, refName, name);
        return Json.Deserialize<Plan>(Strings.ConsumeUtf8(ptr), "get_plan");
    }

    public Plan ArchivePlan(string refName, string name)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_archive_plan(H, refName, name);
        return Json.Deserialize<Plan>(Strings.ConsumeUtf8(ptr), "archive_plan");
    }

    public void DeletePlan(string refName, string name)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_delete_plan(H, refName, name);
        Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "delete_plan");
    }

    // -----------------------------------------------------------------------
    // Task operations
    // -----------------------------------------------------------------------

    public Task AddTask(
        string refName,
        string plan,
        string title,
        Priority priority,
        AddTaskOptions? options = null)
    {
        ThrowIfDisposed();
        string? blockersJson = null;
        if (options?.Blockers is { Count: > 0 })
        {
            blockersJson = Json.Serialize(options.Blockers);
        }
        var ptr = NativeMethods.agentstategraph_taskstore_add_task(
            H,
            refName,
            plan,
            title,
            PriorityWire(priority),
            options?.ParentId,
            blockersJson,
            options?.AssignedTo);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "add_task");
    }

    /// <summary>
    /// Convenience wrapper over <see cref="AddTask"/> that immediately
    /// attaches the 0.6.0 task extensions (payload / parent_change /
    /// on_complete). The core FFI does not expose a single-call form for
    /// these yet; this helper does the add + three writes under the hood
    /// once extension FFI lands.
    /// </summary>
    /// <remarks>
    /// FLAGGED: the §2 P/Invoke surface as shipped does not include
    /// <c>agentstategraph_taskstore_set_payload</c> /
    /// <c>_set_parent_change</c> / <c>_set_on_complete</c>. Until those
    /// exist, this method just forwards to <see cref="AddTask"/> and
    /// silently drops the extensions. Kept in the surface so callers have
    /// a stable name; §4 tests will flag any regression.
    /// </remarks>
    public Task AddTaskWithExtensions(
        string refName,
        string plan,
        string title,
        Priority priority,
        AddTaskOptions? options = null,
        System.Text.Json.JsonElement? payload = null,
        string? parentChange = null,
        OnCompleteHook? onComplete = null)
    {
        _ = payload;
        _ = parentChange;
        _ = onComplete;
        return AddTask(refName, plan, title, priority, options);
    }

    public IReadOnlyList<Task> ListTasks(string refName, string plan)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_list_tasks(H, refName, plan);
        return Json.Deserialize<List<Task>>(Strings.ConsumeUtf8(ptr), "list_tasks");
    }

    public IReadOnlyList<string> TaskIds(string refName, string plan)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_task_ids(H, refName, plan);
        return Json.Deserialize<List<string>>(Strings.ConsumeUtf8(ptr), "task_ids");
    }

    public Task GetTask(string refName, string plan, string taskId)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_get_task(H, refName, plan, taskId);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "get_task");
    }

    public Task StartTask(string refName, string plan, string taskId)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_start_task(H, refName, plan, taskId);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "start_task");
    }

    public Task CompleteTask(string refName, string plan, string taskId, Proof proof)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(proof);
        var ptr = NativeMethods.agentstategraph_taskstore_complete_task(
            H,
            refName,
            plan,
            taskId,
            ProofKindWire(proof.Kind),
            proof.Value,
            proof.Note);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "complete_task");
    }

    public Task AbandonTask(string refName, string plan, string taskId, string reason)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_abandon_task(H, refName, plan, taskId, reason);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "abandon_task");
    }

    public Task SetPriority(string refName, string plan, string taskId, Priority priority)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_set_priority(
            H, refName, plan, taskId, PriorityWire(priority));
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "set_priority");
    }

    public Task SetBlockers(string refName, string plan, string taskId, IReadOnlyList<string> blockers)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(blockers);
        var ptr = NativeMethods.agentstategraph_taskstore_set_blockers(
            H, refName, plan, taskId, Json.Serialize(blockers));
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "set_blockers");
    }

    public Task AssignTask(string refName, string plan, string taskId, string agent)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_assign_task(H, refName, plan, taskId, agent);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "assign_task");
    }

    public Task UnassignTask(string refName, string plan, string taskId)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_unassign_task(H, refName, plan, taskId);
        return Json.Deserialize<Task>(Strings.ConsumeUtf8(ptr), "unassign_task");
    }

    /// <summary>
    /// Returns the next unblocked pending task, or <c>null</c> if the plan
    /// has none.
    /// </summary>
    public Task? NextTask(string refName, string plan)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_next_task(H, refName, plan);
        return Json.DeserializeOptional<Task>(Strings.ConsumeUtf8(ptr), "next_task");
    }

    /// <summary>
    /// <see cref="NextTask"/> filtered by assignment. Pass <c>agent = null</c>
    /// for "any"; otherwise <paramref name="includeUnassigned"/> controls
    /// fallback to unassigned tasks.
    /// </summary>
    public Task? NextTaskFor(string refName, string plan, string? agent, bool includeUnassigned)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_next_task_for(
            H, refName, plan, agent ?? string.Empty, (byte)(includeUnassigned ? 1 : 0));
        return Json.DeserializeOptional<Task>(Strings.ConsumeUtf8(ptr), "next_task_for");
    }

    /// <summary>Derived rollup status for a parent task.</summary>
    public TaskStatus DerivedStatus(string refName, string plan, string parentId)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_taskstore_derived_status(H, refName, plan, parentId);
        return Json.Deserialize<TaskStatus>(Strings.ConsumeUtf8(ptr), "derived_status");
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        _handle.Dispose();
        GC.KeepAlive(_repo);
    }

    // -----------------------------------------------------------------------
    // Enum <-> wire helpers
    // -----------------------------------------------------------------------

    private static string PriorityWire(Priority p) => p switch
    {
        Priority.Low => "low",
        Priority.Medium => "medium",
        Priority.High => "high",
        Priority.Critical => "critical",
        _ => throw new ArgumentOutOfRangeException(nameof(p)),
    };

    private static string ProofKindWire(ProofKind k) => k switch
    {
        ProofKind.Commit => "commit",
        ProofKind.File => "file",
        ProofKind.Test => "test",
        ProofKind.Text => "text",
        _ => throw new ArgumentOutOfRangeException(nameof(k)),
    };
}
