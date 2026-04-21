using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>Advisory severity for a <see cref="Policy"/>.</summary>
public enum Severity
{
    Low,
    Medium,
    High,
    Critical,
}

/// <summary>Task urgency (ordered low &lt; medium &lt; high &lt; critical).</summary>
public enum Priority
{
    Low,
    Medium,
    High,
    Critical,
}

/// <summary>Lifecycle state of a task.</summary>
public enum TaskStatus
{
    Pending,
    InProgress,
    Done,
    Abandoned,
}

/// <summary>Lifecycle state of a plan.</summary>
public enum PlanStatus
{
    Active,
    Completed,
    Archived,
}

/// <summary>Category of evidence attached to a completed task.</summary>
public enum ProofKind
{
    Commit,
    File,
    Test,
    Text,
}

/// <summary>
/// Lifecycle state of a <see cref="Session"/>. Matches the Rust wire form
/// (PascalCase: <c>"Active"</c>, <c>"Completed"</c>, <c>"Abandoned"</c>) —
/// the core crate explicitly does not <c>rename_all</c> this enum.
/// </summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum SessionStatus
{
    Active,
    Completed,
    Abandoned,
}

/// <summary>
/// Status of an <see cref="Epoch"/>. Matches the Rust wire form
/// (PascalCase) — the core crate explicitly does not <c>rename_all</c>.
/// </summary>
[JsonConverter(typeof(JsonStringEnumConverter))]
public enum EpochStatus
{
    Active,
    Sealed,
    Archived,
}
