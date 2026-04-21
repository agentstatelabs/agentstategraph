using System;
using System.Collections.Generic;
using System.Text.Json;
using AgentStateGraph;

namespace AgentStateGraph.Tests;

/// <summary>
/// Shared helpers for the xUnit suite. Mirrors the <c>_policy</c> /
/// <c>store</c> fixtures used by <c>bindings/python/tests/test_policy.py</c>
/// so scenarios line up one-to-one.
/// </summary>
internal static class TestHelpers
{
    /// <summary>A fresh, ephemeral in-memory repository.</summary>
    public static Repository FreshRepo() => new Repository();

    /// <summary>
    /// Fluent <see cref="Policy"/> skeleton matching the Python
    /// <c>_policy(path, ...)</c> helper. Callers can override severity,
    /// <c>active_from</c>, triggers, etc. Leaves <c>ratified_by</c> unset
    /// so the policy is a fresh proposal.
    /// </summary>
    public static Policy SkeletonPolicy(
        string path,
        Selector? situationSelector = null,
        IReadOnlyList<AuthorizedAction>? allow = null,
        IReadOnlyList<AuthorizedAction>? deny = null,
        IReadOnlyList<ApprovalRule>? requireApproval = null,
        IReadOnlyList<string>? triggers = null,
        IReadOnlyList<string>? requiredFields = null,
        Severity severity = Severity.Low,
        DateTimeOffset? activeFrom = null)
    {
        var now = DateTimeOffset.UtcNow;
        return new Policy(
            Path: path,
            Version: 1,
            Situation: $"situation for {path}",
            SituationSelector: situationSelector ?? new Selector.Always(),
            ProposedBy: "xunit",
            ProposedAt: now,
            ActiveFrom: activeFrom ?? now,
            Severity: severity,
            Allow: allow,
            Deny: deny,
            RequireApproval: requireApproval,
            Triggers: triggers,
            RequiredFields: requiredFields);
    }

    /// <summary>
    /// Proposes <paramref name="policy"/> and immediately ratifies it.
    /// Returns the freshly-fetched policy record so the caller can assert
    /// round-trip state without another <c>Get</c>.
    /// </summary>
    public static Policy RatifiedPolicy(
        Repository repo,
        PolicyStore store,
        Policy policy,
        string ratifier = "alice",
        string reasoning = "approved for tests",
        string refName = Repository.DefaultRef)
    {
        _ = repo; // Reserved: future helpers may want the repo directly.
        store.Propose(refName, policy);
        store.Ratify(refName, policy.Path, ratifier, reasoning);
        return store.Get(refName, policy.Path);
    }

    /// <summary>
    /// Round-trips <paramref name="value"/> through <see cref="JsonSerializer"/>
    /// using the binding's shared options — asserts deserialization does not
    /// throw and the result is non-null. Returns the roundtripped instance.
    /// </summary>
    public static T JsonRoundTrip<T>(T value) where T : class
    {
        var opts = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            DictionaryKeyPolicy = JsonNamingPolicy.SnakeCaseLower,
            DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull,
        };
        opts.Converters.Add(new System.Text.Json.Serialization.JsonStringEnumConverter(
            JsonNamingPolicy.SnakeCaseLower));
        var json = JsonSerializer.Serialize(value, opts);
        var back = JsonSerializer.Deserialize<T>(json, opts)
            ?? throw new InvalidOperationException("round-trip produced null");
        return back;
    }

}
