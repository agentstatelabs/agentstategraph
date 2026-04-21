using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>
/// Boolean expression over a situation map (Rust:
/// <c>agentstategraph_policy::Selector</c>). Tagged on <c>kind</c> with
/// snake_case discriminator; variants mirror the Rust enum exactly.
/// </summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(Always), typeDiscriminator: "always")]
[JsonDerivedType(typeof(Never), typeDiscriminator: "never")]
[JsonDerivedType(typeof(All), typeDiscriminator: "all")]
[JsonDerivedType(typeof(Any), typeDiscriminator: "any")]
[JsonDerivedType(typeof(Not), typeDiscriminator: "not")]
[JsonDerivedType(typeof(Eq), typeDiscriminator: "eq")]
[JsonDerivedType(typeof(Ne), typeDiscriminator: "ne")]
[JsonDerivedType(typeof(Matches), typeDiscriminator: "matches")]
[JsonDerivedType(typeof(Exists), typeDiscriminator: "exists")]
[JsonDerivedType(typeof(Gt), typeDiscriminator: "gt")]
[JsonDerivedType(typeof(Gte), typeDiscriminator: "gte")]
[JsonDerivedType(typeof(Lt), typeDiscriminator: "lt")]
[JsonDerivedType(typeof(Lte), typeDiscriminator: "lte")]
public abstract record Selector
{
    /// <summary>
    /// Wire-level discriminator tag, computed from runtime type. Mirrors
    /// the rename-free approach used on <see cref="Decision"/> /
    /// <see cref="FallbackAction"/> / <see cref="OnCompleteHook"/>:
    /// derived records intentionally do NOT expose a sibling <c>Kind</c>
    /// property because that would collide with the
    /// <c>[JsonPolymorphic]</c> discriminator.
    /// </summary>
    [JsonIgnore]
    public string KindTag => this switch
    {
        Always => "always",
        Never => "never",
        All => "all",
        Any => "any",
        Not => "not",
        Eq => "eq",
        Ne => "ne",
        Matches => "matches",
        Exists => "exists",
        Gt => "gt",
        Gte => "gte",
        Lt => "lt",
        Lte => "lte",
        _ => "unknown",
    };

    /// <summary>Matches everything.</summary>
    public sealed record Always : Selector;

    /// <summary>Matches nothing.</summary>
    public sealed record Never : Selector;

    /// <summary>All children match.</summary>
    public sealed record All(
        [property: JsonPropertyName("children")] IReadOnlyList<Selector> Children)
        : Selector;

    /// <summary>At least one child matches.</summary>
    public sealed record Any(
        [property: JsonPropertyName("children")] IReadOnlyList<Selector> Children)
        : Selector;

    /// <summary>The child does not match.</summary>
    public sealed record Not(
        [property: JsonPropertyName("child")] Selector Child)
        : Selector;

    /// <summary>situation[key] == value.</summary>
    public sealed record Eq(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] string Value)
        : Selector;

    /// <summary>situation[key] != value (false when key missing).</summary>
    public sealed record Ne(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] string Value)
        : Selector;

    /// <summary>situation[key] matches the given regex.</summary>
    public sealed record Matches(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("pattern")] string Pattern)
        : Selector;

    /// <summary>Key is present (regardless of value).</summary>
    public sealed record Exists(
        [property: JsonPropertyName("key")] string Key)
        : Selector;

    /// <summary>Numeric situation[key] &gt; value.</summary>
    public sealed record Gt(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] long Value)
        : Selector;

    /// <summary>Numeric situation[key] &gt;= value.</summary>
    public sealed record Gte(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] long Value)
        : Selector;

    /// <summary>Numeric situation[key] &lt; value.</summary>
    public sealed record Lt(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] long Value)
        : Selector;

    /// <summary>Numeric situation[key] &lt;= value.</summary>
    public sealed record Lte(
        [property: JsonPropertyName("key")] string Key,
        [property: JsonPropertyName("value")] long Value)
        : Selector;
}
