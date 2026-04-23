using System;
using System.Linq;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// Mirrors <c>crates/agentstategraph-ffi/tests/taint_ffi.rs</c> scenarios via
/// the C# <see cref="Repository"/> surface added in 0.7.75 §9e. The binding
/// remains experimental — these tests run against the native library when it
/// is available (same opt-in pattern as the rest of the suite).
/// </summary>
public sealed class TaintTests
{
    [Fact]
    public void TaintApply_ThenListAndCheck()
    {
        using var repo = TestHelpers.FreshRepo();
        var id = repo.Taint(
            "/cluster",
            new TaintParams(
                Name: "t1",
                Effect: TaintEffect.Warn,
                Reason: "x",
                AgentId: "ops"));
        Assert.False(string.IsNullOrWhiteSpace(id));

        var list = repo.ListTaints();
        Assert.Contains(list, t => t.Id == id && t.Path == "/cluster" && t.Name == "t1");
    }

    [Fact]
    public void CheckTaint_BlocksWriteWhenEffectIsBlock()
    {
        using var repo = TestHelpers.FreshRepo();
        _ = repo.Taint(
            "/cluster",
            new TaintParams(
                Name: "blk",
                Effect: TaintEffect.Block,
                Reason: "hard-stop",
                AgentId: "ops"));

        var check = repo.CheckTaint("/cluster/inner", agentId: "agent-1", confidence: 1.0);
        Assert.True(check.Tainted);
        Assert.False(check.CanWrite);
    }

    [Fact]
    public void QuarantineAndRelease_RoundTrip()
    {
        using var repo = TestHelpers.FreshRepo();
        var id = repo.Quarantine(
            "/secret",
            new QuarantineParams(
                Name: "sec",
                Reason: "audit",
                AuthorizedAgents: new[] { "sec" },
                AgentId: "sec"));
        Assert.False(string.IsNullOrWhiteSpace(id));

        repo.Unquarantine(
            "/secret",
            "sec",
            new UntaintParams(Reason: "clear", AgentId: "sec", Proof: "c1"));

        // After release, the active list (include_resolved = false) should
        // no longer contain this quarantine.
        var remaining = repo.ListTaints(kind: TaintKind.Quarantine);
        Assert.DoesNotContain(remaining, t => t.Id == id);
    }

    [Fact]
    public void WatchApplyAndRemove()
    {
        using var repo = TestHelpers.FreshRepo();
        var id = repo.Watch(
            "/metric",
            new WatchParams(
                Name: "w1",
                Reason: "perf",
                AgentId: "ops",
                Metric: "pct",
                Threshold: 80.0));
        Assert.False(string.IsNullOrWhiteSpace(id));

        repo.Unwatch("/metric", "w1", new UnwatchParams(AgentId: "ops"));

        var remaining = repo.ListTaints(kind: TaintKind.Watch);
        Assert.DoesNotContain(remaining, t => t.Id == id);
    }

    [Fact]
    public void TaintApply_InvalidEffect_SurfacesAsError()
    {
        // The TaintEffect enum is round-tripped through snake_case, so we
        // cannot easily feed an invalid variant from C#. Instead, remove a
        // name that was never applied — same error channel.
        using var repo = TestHelpers.FreshRepo();
        var ex = Assert.Throws<AgentStateGraphException>(() =>
            repo.Untaint(
                "/ghost",
                "missing",
                new UntaintParams(Reason: "x", AgentId: "ops")));
        Assert.Equal("taint_remove", ex.Operation);
    }

    [Fact]
    public void ListTaints_FiltersByPrefixAndKind()
    {
        using var repo = TestHelpers.FreshRepo();
        _ = repo.Taint(
            "/a",
            new TaintParams(Name: "ta", Effect: TaintEffect.Warn, Reason: "r", AgentId: "ops"));
        _ = repo.Taint(
            "/b",
            new TaintParams(Name: "tb", Effect: TaintEffect.Warn, Reason: "r", AgentId: "ops"));
        _ = repo.Watch(
            "/a",
            new WatchParams(Name: "wa", Reason: "r", AgentId: "ops"));

        var aOnly = repo.ListTaints(pathPrefix: "/a");
        Assert.All(aOnly, t => Assert.StartsWith("/a", t.Path));
        Assert.True(aOnly.Count >= 2);

        var watches = repo.ListTaints(kind: TaintKind.Watch);
        Assert.All(watches, t => Assert.Equal(TaintKind.Watch, t.Kind));
        Assert.Contains(watches, t => t.Name == "wa");
    }

    [Fact]
    public void CheckTaint_CleanPath_AllowsWrite()
    {
        using var repo = TestHelpers.FreshRepo();
        var check = repo.CheckTaint("/nowhere", agentId: "anyone", confidence: 1.0);
        Assert.False(check.Tainted);
        Assert.False(check.Quarantined);
        Assert.True(check.CanWrite);
    }

    [Fact]
    public void Quarantine_RespectsAuthorizedAgents()
    {
        using var repo = TestHelpers.FreshRepo();
        _ = repo.Quarantine(
            "/vault",
            new QuarantineParams(
                Name: "sec",
                Reason: "audit",
                AuthorizedAgents: new[] { "agent/security" },
                AgentId: "sec"));

        var authorized = repo.CheckTaint("/vault/key", agentId: "agent/security", confidence: 1.0);
        var stranger = repo.CheckTaint("/vault/key", agentId: "agent/random", confidence: 1.0);

        Assert.True(authorized.Quarantined);
        Assert.True(stranger.Quarantined);
        // The authorized agent should be allowed to write; the stranger should not.
        Assert.True(authorized.CanWrite);
        Assert.False(stranger.CanWrite);
        Assert.Contains("agent/security", stranger.AuthorizedAgents ?? Array.Empty<string>());
    }
}
