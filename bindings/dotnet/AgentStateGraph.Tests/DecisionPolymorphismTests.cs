using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// Exercises the <see cref="Decision"/> / <see cref="FallbackAction"/> /
/// <see cref="Selector"/> <c>[JsonPolymorphic]</c> tag routing. Does NOT
/// require the native library — pure System.Text.Json surface.
/// </summary>
public sealed class DecisionPolymorphismTests
{
    private static JsonSerializerOptions Opts()
    {
        var o = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            DictionaryKeyPolicy = JsonNamingPolicy.SnakeCaseLower,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        };
        o.Converters.Add(new JsonStringEnumConverter(JsonNamingPolicy.SnakeCaseLower));
        return o;
    }

    private static string Ser<T>(T v) => JsonSerializer.Serialize(v, Opts());
    private static T De<T>(string s) => JsonSerializer.Deserialize<T>(s, Opts())!;

    public static IEnumerable<object[]> DecisionVariants => new[]
    {
        new object[] { (Decision)new Decision.Allow("infra/x@1", new[] { "p1" }) },
        new object[] { (Decision)new Decision.Deny("infra/x@1", "disallowed") },
        new object[]
        {
            (Decision)new Decision.RequireApproval(
                "infra/x@1",
                new[] { "human" },
                new FallbackAction.LowestRiskAlternative()),
        },
        new object[] { (Decision)new Decision.NoPolicyMatch() },
    };

    [Theory]
    [MemberData(nameof(DecisionVariants))]
    public void Decision_RoundTripsViaTag(Decision original)
    {
        var json = Ser(original);
        var back = De<Decision>(json);
        // Reserialize — the two JSON representations must be identical.
        Assert.Equal(json, Ser(back));
        // Runtime type — the discriminator must route to the same variant.
        Assert.Equal(original.GetType(), back.GetType());
        Assert.Equal(original.KindTag, back.KindTag);
    }

    [Fact]
    public void Decision_NoPolicyMatch_HasNoExtraFields()
    {
        var json = Ser<Decision>(new Decision.NoPolicyMatch());
        Assert.Contains("\"kind\":\"no_policy_match\"", json);
        // No body keys on the wire beyond the discriminator.
        using var doc = JsonDocument.Parse(json);
        var names = new List<string>();
        foreach (var prop in doc.RootElement.EnumerateObject())
        {
            names.Add(prop.Name);
        }
        Assert.Equal(new[] { "kind" }, names);
    }

    [Fact]
    public void FallbackAction_PickAlternative_RoundTrip()
    {
        FallbackAction fb = new FallbackAction.PickAlternative("rollback");
        var back = De<FallbackAction>(Ser(fb));
        var picked = Assert.IsType<FallbackAction.PickAlternative>(back);
        Assert.Equal("rollback", picked.Action);
    }

    [Fact]
    public void Selector_All_Eq_Composition_RoundTrip()
    {
        Selector root = new Selector.All(new Selector[]
        {
            new Selector.Eq("env", "prod"),
            new Selector.Eq("tier", "edge"),
        });
        var back = De<Selector>(Ser(root));
        var all = Assert.IsType<Selector.All>(back);
        Assert.Equal(2, all.Children.Count);
        Assert.All(all.Children, c => Assert.IsType<Selector.Eq>(c));
    }

    [Fact]
    public void Selector_Ne_And_Matches_RoundTrip()
    {
        Selector ne = new Selector.Ne("region", "us-east-1");
        var neBack = De<Selector>(Ser(ne));
        var neCast = Assert.IsType<Selector.Ne>(neBack);
        Assert.Equal("region", neCast.Key);
        Assert.Equal("us-east-1", neCast.Value);

        Selector matches = new Selector.Matches("path", "^/policies/.*");
        var mBack = De<Selector>(Ser(matches));
        var mCast = Assert.IsType<Selector.Matches>(mBack);
        Assert.Equal("^/policies/.*", mCast.Pattern);
    }
}
