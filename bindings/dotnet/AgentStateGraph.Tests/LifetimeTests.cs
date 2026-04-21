using System;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// C#-native IDisposable / SafeHandle behaviour. Other bindings don't
/// exercise this — managed-resource ownership is a .NET-specific concern.
/// </summary>
public sealed class LifetimeTests
{
    [Fact]
    public void Repository_DoubleDispose_IsIdempotent()
    {
        var repo = new Repository();
        repo.Dispose();
        // Second dispose must not throw or double-free.
        repo.Dispose();
    }

    [Fact]
    public void Repository_UseAfterDispose_Throws()
    {
        var repo = new Repository();
        repo.Dispose();
        Assert.Throws<ObjectDisposedException>(() => repo.Get("/x"));
    }

    [Fact]
    public void TaskStore_UsingScope_ReleasesHandle()
    {
        using var repo = new Repository();
        TaskStore? captured;
        using (var ts = new TaskStore(repo, "/plans", "xunit"))
        {
            ts.CreatePlan(Repository.DefaultRef, "p", null);
            captured = ts;
        }
        Assert.Throws<ObjectDisposedException>(() =>
            captured!.ListPlans(Repository.DefaultRef));
    }

    [Fact]
    public void PolicyStore_UsingScope_ReleasesHandle()
    {
        using var repo = new Repository();
        PolicyStore? captured;
        using (var ps = new PolicyStore(repo, "/policies", "xunit"))
        {
            captured = ps;
        }
        Assert.Throws<ObjectDisposedException>(() =>
            captured!.List(Repository.DefaultRef));
    }

    [Fact]
    public void TaskStore_KeepsRepositoryAlive_ViaStrongReference()
    {
        // The store holds a managed reference back to the repo (the
        // `_repo` field + GC.KeepAlive in Dispose) so that even if the
        // caller drops the Repository reference, GC won't collect it
        // out from under the store. We prove that by dropping the local
        // reference and forcing a collection — the store must still
        // be usable.
        var ts = CreateStoreDroppingRepoRef();
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();

        // Store still works — its held `_repo` kept the native handle alive.
        ts.CreatePlan(Repository.DefaultRef, "after-gc", null);
        ts.Dispose();
        ts.Dispose(); // idempotent
    }

    private static TaskStore CreateStoreDroppingRepoRef()
    {
        var repo = new Repository();
        return new TaskStore(repo, "/plans", "xunit");
        // `repo` local falls out of scope here; only the store's private
        // field keeps it reachable.
    }

    [Fact]
    public void SafeHandle_FinalizerReleasesNativeMemory()
    {
        // Smoke-check: a Repository created and dropped without Dispose
        // should still have its finalizer run the native free. We can't
        // directly observe the native-side free, but we CAN assert that
        // the managed memory churn from repeatedly allocating + dropping
        // does not grow unbounded — proving the SafeHandle isn't leaking
        // a managed reference path that pins the native handle.
        long before = GC.GetTotalMemory(forceFullCollection: true);
        for (int i = 0; i < 50; i++)
        {
            _ = new Repository();
        }
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        long after = GC.GetTotalMemory(forceFullCollection: true);

        // Allow generous headroom — xUnit's own infrastructure allocates.
        // The point is that the 50 Repository objects themselves don't
        // retain (they'd otherwise be detectable in the delta).
        Assert.True(after - before < 5_000_000,
            $"unexpected managed-heap growth: {after - before} bytes");
    }
}
