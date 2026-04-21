using System;

namespace AgentStateGraph;

/// <summary>
/// Thrown when the native AgentStateGraph FFI reports an error. The FFI's
/// error channel is a JSON envelope of the form <c>{"error": "..."}</c> —
/// this exception carries that message verbatim so callers see the same
/// diagnostic the other five bindings surface.
/// </summary>
public sealed class AgentStateGraphException : Exception
{
    /// <summary>
    /// Optional operation name (e.g. <c>"propose"</c>, <c>"evaluate"</c>).
    /// Included in <see cref="Exception.Message"/> when present.
    /// </summary>
    public string? Operation { get; }

    public AgentStateGraphException(string message)
        : base(message)
    {
    }

    public AgentStateGraphException(string operation, string message)
        : base($"{operation}: {message}")
    {
        Operation = operation;
    }

    public AgentStateGraphException(string operation, string message, Exception inner)
        : base($"{operation}: {message}", inner)
    {
        Operation = operation;
    }
}
