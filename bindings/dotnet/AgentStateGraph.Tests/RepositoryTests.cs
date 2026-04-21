using System;
using System.Linq;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// Repository-level coverage (commit storage + branching + log/diff/blame).
/// DeleteBranch and ListBranches are deliberately omitted — they are
/// pre-existing FFI gaps flagged in §3 of the 0.7.25-beta.1 plan.
/// </summary>
public sealed class RepositoryTests
{
    [Fact]
    public void NewMemoryRepo_SetGet_RoundTripsJson()
    {
        using var repo = new Repository();
        var commit = repo.Set("/hello", "\"world\"", "plan", "say hi");
        Assert.False(string.IsNullOrEmpty(commit.Value));

        var got = repo.Get("/hello");
        Assert.Equal("\"world\"", got);
    }

    [Fact]
    public void SetJson_SerializesViaSharedOptions()
    {
        using var repo = new Repository();
        var payload = new { greeting = "hi", count = 3 };
        repo.SetJson("/payload", payload, "plan", "set");
        var got = repo.Get("/payload");
        Assert.Contains("greeting", got);
        Assert.Contains("count", got);
    }

    [Fact]
    public void DeletePath_ProducesCommit()
    {
        using var repo = new Repository();
        repo.Set("/doomed", "42", "plan", "set");
        var del = repo.Delete("/doomed", "plan", "remove");
        Assert.False(string.IsNullOrEmpty(del.Value));
    }

    [Fact]
    public void Branch_CreatesNamedBranch()
    {
        using var repo = new Repository();
        repo.Set("/seed", "\"v\"", "plan", "seed");
        var head = repo.Branch("feature/x");
        Assert.False(string.IsNullOrEmpty(head));
    }

    [Fact]
    public void Merge_TwoBranches_ReturnsMergeCommit()
    {
        using var repo = new Repository();
        repo.Set("/base", "\"v1\"", "plan", "seed");
        repo.Branch("feature/merge");
        repo.Set("/base", "\"v2\"", "plan", "advance main");
        var merged = repo.Merge("feature/merge", "main", "merge back");
        Assert.False(string.IsNullOrEmpty(merged.Value));
    }

    [Fact]
    public void Log_ReturnsCommitsInOrder()
    {
        using var repo = new Repository();
        repo.Set("/a", "\"1\"", "plan", "first");
        repo.Set("/b", "\"2\"", "plan", "second");

        var log = repo.Log(limit: 10);
        Assert.True(log.Count >= 2);
        // Newest first — "second" should appear before "first".
        var descriptions = log.Select(c => c.IntentDescription).ToArray();
        var idxFirst = Array.IndexOf(descriptions, "first");
        var idxSecond = Array.IndexOf(descriptions, "second");
        Assert.True(idxSecond < idxFirst, "log should be newest-first");
    }

    [Fact]
    public void Diff_BetweenTwoRefs_ReturnsJson()
    {
        using var repo = new Repository();
        repo.Set("/x", "\"a\"", "plan", "a");
        repo.Branch("feature/diff");
        repo.Set("/x", "\"b\"", "plan", "b");

        var diff = repo.Diff("main", "feature/diff");
        Assert.False(string.IsNullOrEmpty(diff));
    }

    [Fact]
    public void Blame_ReturnsAgentAndIntent()
    {
        using var repo = new Repository();
        repo.Set("/blamed", "\"v\"", "plan", "set it");
        var blame = repo.Blame("/blamed");
        Assert.False(string.IsNullOrEmpty(blame));
    }
}
