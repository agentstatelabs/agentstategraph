using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>
/// Result of a policy evaluation. The four variants map 1:1 to the Rust
/// <c>Decision</c> enum in <c>agentstategraph-policy</c>. Wire form is
/// serde-tagged on <c>kind</c> with snake_case discriminator values.
/// </summary>
/// <remarks>
/// System.Text.Json handles the tagged union via
/// <see cref="JsonPolymorphicAttribute"/>; the <c>"kind"</c> discriminator
/// matches Rust's <c>#[serde(tag = "kind", rename_all = "snake_case")]</c>.
/// </remarks>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(Allow), typeDiscriminator: "allow")]
[JsonDerivedType(typeof(Deny), typeDiscriminator: "deny")]
[JsonDerivedType(typeof(RequireApproval), typeDiscriminator: "require_approval")]
[JsonDerivedType(typeof(NoPolicyMatch), typeDiscriminator: "no_policy_match")]
public abstract record Decision
{
    /// <summary>Convenience accessor for the variant tag.</summary>
    [JsonIgnore]
    public abstract DecisionKind Kind { get; }

    /// <summary>The policy authorizes the action; preconditions are advisory.</summary>
    public sealed record Allow(
        [property: JsonPropertyName("matched_policy")] string MatchedPolicy,
        [property: JsonPropertyName("preconditions")] IReadOnlyList<string>? Preconditions = null)
        : Decision
    {
        public override DecisionKind Kind => DecisionKind.Allow;
    }

    /// <summary>The policy forbids the action; <see cref="Reason"/> is human-readable.</summary>
    public sealed record Deny(
        [property: JsonPropertyName("matched_policy")] string MatchedPolicy,
        [property: JsonPropertyName("reason")] string Reason)
        : Decision
    {
        public override DecisionKind Kind => DecisionKind.Deny;
    }

    /// <summary>The policy requires human approval before the action can proceed.</summary>
    public sealed record RequireApproval(
        [property: JsonPropertyName("matched_policy")] string MatchedPolicy,
        [property: JsonPropertyName("approvers")] IReadOnlyList<string> Approvers,
        [property: JsonPropertyName("fallback")] FallbackAction Fallback,
        [property: JsonPropertyName("timeout")] ulong? Timeout = null,
        [property: JsonPropertyName("approval_task_path")] string? ApprovalTaskPath = null)
        : Decision
    {
        public override DecisionKind Kind => DecisionKind.RequireApproval;
    }

    /// <summary>No active policy matched; default-deny is the caller's responsibility.</summary>
    public sealed record NoPolicyMatch : Decision
    {
        public override DecisionKind Kind => DecisionKind.NoPolicyMatch;
    }
}

/// <summary>
/// What to do while a change is awaiting approval (POLICY_V1.md §22.3).
/// Five variants; tag is <c>kind</c> with snake_case discriminator.
/// </summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(Block), typeDiscriminator: "block")]
[JsonDerivedType(typeof(PickAlternative), typeDiscriminator: "pick_alternative")]
[JsonDerivedType(typeof(LowestRiskAlternative), typeDiscriminator: "lowest_risk_alternative")]
[JsonDerivedType(typeof(KeepCurrentState), typeDiscriminator: "keep_current_state")]
[JsonDerivedType(typeof(DelegateTo), typeDiscriminator: "delegate_to")]
public abstract record FallbackAction
{
    /// <summary>Do nothing; wait for approval.</summary>
    public sealed record Block : FallbackAction;

    /// <summary>Run the named alternative action.</summary>
    public sealed record PickAlternative(
        [property: JsonPropertyName("action")] string Action)
        : FallbackAction;

    /// <summary>Pick the lowest-risk option from <c>ChangeProposal.Alternatives</c>.</summary>
    public sealed record LowestRiskAlternative : FallbackAction;

    /// <summary>
    /// Leave current state unchanged; record the preferred option as a
    /// pending upgrade.
    /// </summary>
    public sealed record KeepCurrentState : FallbackAction;

    /// <summary>Delegate to another policy by path.</summary>
    public sealed record DelegateTo(
        [property: JsonPropertyName("policy_path")] string PolicyPath)
        : FallbackAction;
}
