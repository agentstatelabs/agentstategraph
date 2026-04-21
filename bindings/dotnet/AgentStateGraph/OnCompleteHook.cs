using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>
/// Hook attached to a <see cref="Task"/>, run by the consumer when the
/// task completes. The <c>agentstategraph-tasks</c> crate round-trips the
/// hook but does not execute it — dispatch lives in the MCP server and
/// other consumers.
/// </summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(PromoteChange), typeDiscriminator: "promote_change")]
[JsonDerivedType(typeof(Named), typeDiscriminator: "named")]
public abstract record OnCompleteHook
{
    /// <summary>
    /// Promote a deferred <see cref="ChangeProposal"/> by re-running
    /// policy evaluation with <c>approval_granted</c> attached.
    /// </summary>
    public sealed record PromoteChange : OnCompleteHook;

    /// <summary>
    /// Call an arbitrary named hook registered by the consumer.
    /// </summary>
    public sealed record Named(
        [property: JsonPropertyName("name")] string Name)
        : OnCompleteHook;
}
