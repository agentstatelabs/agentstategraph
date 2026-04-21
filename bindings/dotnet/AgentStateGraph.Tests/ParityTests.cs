using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// C# runner for <c>spec/policy_parity_fixture.json</c>. Seventh runner
/// to join the shared fixture (Rust reference + Python + TypeScript + Go
/// + WASM + C FFI). Must produce identical Decision kind and matched
/// policy handle identity for every change_proposal + evaluate entry.
/// </summary>
/// <remarks>
/// Mirrors <c>crates/agentstategraph-policy/tests/parity_reference.rs</c>
/// and <c>bindings/{python,typescript,go}</c> runners bit-for-bit on
/// Decision kind. Fixture is located by walking up from
/// <see cref="AppContext.BaseDirectory"/> until a directory containing
/// <c>spec/policy_parity_fixture.json</c> is found (capped at 16 parents).
/// </remarks>
public sealed class ParityTests
{
    private const int MaxParentWalk = 16;
    private const string FixtureRelativePath = "spec/policy_parity_fixture.json";

    [Fact]
    public void PolicyFixture_MatchesRustReference()
    {
        var fixturePath = LocateFixture();
        Assert.True(File.Exists(fixturePath), $"fixture not found: {fixturePath}");

        var bytes = File.ReadAllBytes(fixturePath);
        var fixture = JsonSerializer.Deserialize<FixtureRecord>(bytes, FixtureOptions)
            ?? throw new InvalidOperationException("fixture deserialized as null");

        Assert.NotNull(fixture.Policies);
        Assert.NotNull(fixture.ChangeProposals);
        Assert.NotNull(fixture.Evaluate);

        using var repo = new Repository();
        using var store = new PolicyStore(repo, fixture.Prefix, fixture.AgentId);

        // 1. Propose every policy.
        foreach (var policy in fixture.Policies)
        {
            store.Propose(fixture.Ref, policy);
        }

        // 2. Ratify entries.
        foreach (var r in fixture.Ratify ?? new List<RatifyEntry>())
        {
            store.Ratify(fixture.Ref, r.Path, r.Ratifier, r.Reasoning);
        }

        // 3. evaluate_change assertions.
        foreach (var entry in fixture.ChangeProposals)
        {
            var decision = store.EvaluateChange(fixture.Ref, entry.Proposal);
            var actualKind = DecisionKindTag(decision);
            Assert.True(
                string.Equals(actualKind, entry.ExpectedDecisionKind, StringComparison.Ordinal),
                $"proposal {entry.Label}: decision.kind mismatch (expected {entry.ExpectedDecisionKind}, got {actualKind})");

            if (!string.IsNullOrEmpty(entry.ExpectedMatchedPolicyPrefix))
            {
                var matched = MatchedPolicy(decision) ?? "";
                Assert.True(
                    matched.StartsWith(entry.ExpectedMatchedPolicyPrefix, StringComparison.Ordinal),
                    $"proposal {entry.Label}: matched_policy {matched} should start with {entry.ExpectedMatchedPolicyPrefix}");
            }
        }

        // 4. evaluate assertions.
        foreach (var entry in fixture.Evaluate)
        {
            var situation = entry.Situation ?? new Dictionary<string, string>();
            var decision = store.Evaluate(fixture.Ref, situation, entry.Action, entry.AgentId);
            var actualKind = DecisionKindTag(decision);
            Assert.True(
                string.Equals(actualKind, entry.ExpectedDecisionKind, StringComparison.Ordinal),
                $"evaluate {entry.Label}: decision.kind mismatch (expected {entry.ExpectedDecisionKind}, got {actualKind})");
        }
    }

    // -----------------------------------------------------------------------
    // Fixture locator: walk up from AppContext.BaseDirectory until a
    // directory containing spec/policy_parity_fixture.json is found. Mirrors
    // the Rust reference runner's manifest-dir climb, but in managed code so
    // the test works both when run under `dotnet test` (bin/Debug/...) and
    // from an installed NuGet layout.
    // -----------------------------------------------------------------------
    private static string LocateFixture()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        for (var i = 0; i < MaxParentWalk && dir is not null; i++, dir = dir.Parent)
        {
            var candidate = Path.Combine(dir.FullName, FixtureRelativePath);
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }
        throw new FileNotFoundException(
            $"could not find {FixtureRelativePath} within {MaxParentWalk} parents of {AppContext.BaseDirectory}");
    }

    private static string DecisionKindTag(Decision decision) => decision.KindTag;

    private static string? MatchedPolicy(Decision decision) => decision switch
    {
        Decision.Allow a => a.MatchedPolicy,
        Decision.Deny d => d.MatchedPolicy,
        Decision.RequireApproval r => r.MatchedPolicy,
        Decision.NoPolicyMatch => null,
        _ => null,
    };

    // -----------------------------------------------------------------------
    // Local JSON options mirroring AgentStateGraph.Json.Options. Kept
    // local rather than using the internal shared instance so this file has
    // no dependency on InternalsVisibleTo surface beyond what xUnit already
    // exercises.
    // -----------------------------------------------------------------------
    private static readonly JsonSerializerOptions FixtureOptions = BuildFixtureOptions();

    private static JsonSerializerOptions BuildFixtureOptions()
    {
        var opts = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            DictionaryKeyPolicy = JsonNamingPolicy.SnakeCaseLower,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            ReadCommentHandling = JsonCommentHandling.Skip,
            PropertyNameCaseInsensitive = false,
        };
        opts.Converters.Add(new JsonStringEnumConverter(JsonNamingPolicy.SnakeCaseLower));
        return opts;
    }

    // -----------------------------------------------------------------------
    // Fixture shape. Private nested records — don't leak into the public
    // surface. JSON keys resolved via SnakeCaseLower naming policy; the
    // ExpectedDecisionKind string stays a raw wire tag for comparison with
    // the polymorphic Decision discriminator.
    // -----------------------------------------------------------------------
    private sealed record FixtureRecord(
        string Description,
        string Prefix,
        [property: JsonPropertyName("agent_id")] string AgentId,
        [property: JsonPropertyName("ref")] string Ref,
        List<Policy> Policies,
        List<RatifyEntry>? Ratify,
        [property: JsonPropertyName("change_proposals")] List<ChangeProposalEntry> ChangeProposals,
        List<EvaluateEntry> Evaluate);

    private sealed record RatifyEntry(
        string Path,
        string Ratifier,
        string Reasoning);

    private sealed record ChangeProposalEntry(
        string Label,
        ChangeProposal Proposal,
        [property: JsonPropertyName("expected_decision_kind")] string ExpectedDecisionKind,
        [property: JsonPropertyName("expected_matched_policy_prefix")] string? ExpectedMatchedPolicyPrefix);

    private sealed record EvaluateEntry(
        string Label,
        Dictionary<string, string>? Situation,
        string Action,
        [property: JsonPropertyName("agent_id")] string AgentId,
        [property: JsonPropertyName("expected_decision_kind")] string ExpectedDecisionKind);
}
