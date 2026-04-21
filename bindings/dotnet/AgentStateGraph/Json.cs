using System;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>
/// Shared <see cref="JsonSerializerOptions"/> for the entire binding. The
/// Rust serde surface uses <c>snake_case</c> on the wire; every C# record
/// / enum round-trips through these options so PascalCase callers get wire
/// parity with Python / TypeScript / Go / WASM / FFI.
/// </summary>
internal static class Json
{
    /// <summary>
    /// Lazily initialized so <see cref="JsonSerializerOptions"/> is frozen
    /// on first use (per .NET 8 / 10 guidance — mutating after the first
    /// (de)serialize throws).
    /// </summary>
    internal static JsonSerializerOptions Options { get; } = Build();

    private static JsonSerializerOptions Build()
    {
        var opts = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
            DictionaryKeyPolicy = JsonNamingPolicy.SnakeCaseLower,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            ReadCommentHandling = JsonCommentHandling.Skip,
        };
        // Enum variants (e.g. Severity.Critical) should also use
        // snake_case on the wire to match Rust `#[serde(rename_all =
        // "snake_case")]`.
        opts.Converters.Add(new JsonStringEnumConverter(JsonNamingPolicy.SnakeCaseLower));
        return opts;
    }

    /// <summary>
    /// Checks a JSON payload for an <c>{"error": "..."}</c> envelope. If
    /// present, throws <see cref="AgentStateGraphException"/>; otherwise
    /// returns <paramref name="raw"/> unchanged so the caller can proceed
    /// to strong-typed deserialization.
    /// </summary>
    internal static string ThrowIfError(string? raw, string operation)
    {
        if (raw is null)
        {
            throw new AgentStateGraphException(operation, "native FFI returned null");
        }
        // Fast-path: cheap substring scan before paying for a parse.
        if (raw.Contains("\"error\"", StringComparison.Ordinal))
        {
            try
            {
                using var doc = JsonDocument.Parse(raw);
                if (doc.RootElement.ValueKind == JsonValueKind.Object
                    && doc.RootElement.TryGetProperty("error", out var err)
                    && err.ValueKind == JsonValueKind.String)
                {
                    throw new AgentStateGraphException(operation, err.GetString() ?? "unknown error");
                }
            }
            catch (JsonException)
            {
                // Not JSON — let the caller decide what to do with it.
            }
        }
        return raw;
    }

    /// <summary>
    /// Convenience: error-check + deserialize in one call.
    /// </summary>
    internal static T Deserialize<T>(string? raw, string operation)
    {
        var json = ThrowIfError(raw, operation);
        try
        {
            var value = JsonSerializer.Deserialize<T>(json, Options);
            if (value is null)
            {
                throw new AgentStateGraphException(operation, $"deserialized null for {typeof(T).Name}");
            }
            return value;
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException(operation, $"failed to parse response: {ex.Message}", ex);
        }
    }

    /// <summary>
    /// Deserialize into a type that may legitimately be <c>null</c> on the
    /// wire (JSON literal <c>null</c>) — used for <c>next_task</c>-style
    /// optional returns.
    /// </summary>
    internal static T? DeserializeOptional<T>(string? raw, string operation) where T : class
    {
        var json = ThrowIfError(raw, operation);
        if (json == "null")
        {
            return null;
        }
        try
        {
            return JsonSerializer.Deserialize<T>(json, Options);
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException(operation, $"failed to parse response: {ex.Message}", ex);
        }
    }

    internal static string Serialize<T>(T value) =>
        JsonSerializer.Serialize(value, Options);
}
