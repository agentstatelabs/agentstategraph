using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;
using AgentStateGraph.Interop;

namespace AgentStateGraph;

// =============================================================================
// Enums (0.7.75 §7 — taint / quarantine / watch)
// =============================================================================

/// <summary>
/// Discriminator between the three taint kinds.
/// Matches Rust <c>agentstategraph_taint::TaintKind</c>.
/// </summary>
public enum TaintKind
{
    Taint,
    Quarantine,
    Watch,
}

/// <summary>
/// Pre-commit hook behavior for a taint. Matches Rust
/// <c>agentstategraph_taint::TaintEffect</c>.
/// </summary>
public enum TaintEffect
{
    /// <summary>Attach a warning but allow the write.</summary>
    Warn,
    /// <summary>Reject the write.</summary>
    Block,
    /// <summary>Require confidence &gt;= 0.9; reject lower confidence writes.</summary>
    Review,
    /// <summary>Allow the write; flag the path for query / search filtering.</summary>
    Isolate,
    /// <summary>Watch-only — purely advisory.</summary>
    Advisory,
}

/// <summary>Advisory severity for a taint — does not change pre-commit semantics.</summary>
public enum TaintSeverity
{
    Low,
    Medium,
    High,
    Critical,
}

/// <summary>Direction a watch threshold fires in.</summary>
public enum WatchDirection
{
    Above,
    Below,
}

// =============================================================================
// Records — wire shapes from crates/agentstategraph-taint/src/types.rs
// =============================================================================

/// <summary>
/// A taint / quarantine / watch record as emitted by
/// <see cref="Repository.ListTaints"/> and the
/// <c>agentstategraph_check_taint</c> envelope.
/// </summary>
public sealed record Taint(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("path")] string Path,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("kind")] TaintKind Kind,
    [property: JsonPropertyName("effect")] TaintEffect Effect,
    [property: JsonPropertyName("severity")] TaintSeverity Severity,
    [property: JsonPropertyName("reason")] string Reason,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("commit_id")] string CommitId,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("propagate")] bool Propagate,
    [property: JsonPropertyName("expires_at")] DateTimeOffset? ExpiresAt = null,
    [property: JsonPropertyName("resolved_at")] DateTimeOffset? ResolvedAt = null,
    [property: JsonPropertyName("resolved_by")] string? ResolvedBy = null,
    [property: JsonPropertyName("resolved_reason")] string? ResolvedReason = null,
    [property: JsonPropertyName("resolved_proof")] string? ResolvedProof = null,
    /// <summary>
    /// Kind-specific metadata. Typed as <see cref="JsonElement"/> so callers
    /// can inspect the shape without the binding committing to a schema
    /// (matches the <see cref="Task.Payload"/> pattern).
    /// </summary>
    [property: JsonPropertyName("metadata")] IReadOnlyDictionary<string, JsonElement>? Metadata = null);

/// <summary>
/// Result of <see cref="Repository.CheckTaint"/>. Mirrors Rust
/// <c>agentstategraph_taint::TaintCheck</c>.
/// </summary>
public sealed record TaintCheck(
    [property: JsonPropertyName("tainted")] bool Tainted,
    [property: JsonPropertyName("quarantined")] bool Quarantined,
    [property: JsonPropertyName("watched")] bool Watched,
    [property: JsonPropertyName("can_write")] bool CanWrite,
    [property: JsonPropertyName("required_confidence")] double RequiredConfidence,
    [property: JsonPropertyName("isolated")] bool Isolated,
    [property: JsonPropertyName("taints")] IReadOnlyList<Taint>? Taints = null,
    [property: JsonPropertyName("quarantines")] IReadOnlyList<Taint>? Quarantines = null,
    [property: JsonPropertyName("watches")] IReadOnlyList<Taint>? Watches = null,
    [property: JsonPropertyName("authorized_agents")] IReadOnlyList<string>? AuthorizedAgents = null);

// =============================================================================
// Parameter records for the Repository surface
// =============================================================================

/// <summary>Parameters for <see cref="Repository.Taint"/>.</summary>
public sealed record TaintParams(
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("effect")] TaintEffect Effect,
    [property: JsonPropertyName("reason")] string Reason,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("severity")] TaintSeverity Severity = TaintSeverity.Medium,
    [property: JsonPropertyName("expires")] DateTimeOffset? ExpiresAt = null,
    [property: JsonPropertyName("propagate")] bool Propagate = true,
    [property: JsonPropertyName("metadata")] IReadOnlyDictionary<string, JsonElement>? Metadata = null);

/// <summary>Parameters for <see cref="Repository.Untaint"/> / <see cref="Repository.Unquarantine"/>.</summary>
public sealed record UntaintParams(
    [property: JsonPropertyName("reason")] string Reason,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("proof")] string? Proof = null);

/// <summary>Parameters for <see cref="Repository.Quarantine"/>.</summary>
public sealed record QuarantineParams(
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("reason")] string Reason,
    [property: JsonPropertyName("authorized_agents")] IReadOnlyList<string> AuthorizedAgents,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("severity")] TaintSeverity Severity = TaintSeverity.Medium,
    [property: JsonPropertyName("expires")] DateTimeOffset? ExpiresAt = null,
    [property: JsonPropertyName("propagate")] bool Propagate = true);

/// <summary>Parameters for <see cref="Repository.Watch"/>.</summary>
public sealed record WatchParams(
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("reason")] string Reason,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("metric")] string? Metric = null,
    [property: JsonPropertyName("threshold")] double? Threshold = null,
    [property: JsonPropertyName("direction")] WatchDirection Direction = WatchDirection.Above,
    [property: JsonPropertyName("check_interval_secs")] ulong? CheckIntervalSecs = null,
    [property: JsonPropertyName("expires")] DateTimeOffset? ExpiresAt = null,
    [property: JsonPropertyName("severity")] TaintSeverity Severity = TaintSeverity.Medium,
    [property: JsonPropertyName("propagate")] bool Propagate = true);

/// <summary>Parameters for <see cref="Repository.Unwatch"/>.</summary>
public sealed record UnwatchParams(
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("reason")] string? Reason = null);

// =============================================================================
// Repository instance API (§7 pass-through)
// =============================================================================

/// <summary>
/// Taint / quarantine / watch methods on <see cref="Repository"/>. Kept in a
/// partial class so the core repository file isn't buried in §7 surface.
/// </summary>
public sealed partial class Repository
{
    /// <summary>
    /// Applies a taint to <paramref name="path"/>. Returns the new taint id.
    /// </summary>
    public string Taint(string path, TaintParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        var ptr = NativeMethods.agentstategraph_taint_apply(
            _handle.DangerousGetHandle(), refName, path, Json.Serialize(p));
        return ParseIdEnvelope(Strings.ConsumeUtf8(ptr), "taint_apply");
    }

    /// <summary>Removes the taint named <paramref name="name"/> from <paramref name="path"/>.</summary>
    public void Untaint(string path, string name, UntaintParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        var body = BuildRemoveParams(name, p.Reason, p.AgentId, p.Proof);
        var ptr = NativeMethods.agentstategraph_taint_remove(
            _handle.DangerousGetHandle(), refName, path, body);
        Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "taint_remove");
    }

    /// <summary>Applies a quarantine. Returns the new taint id.</summary>
    public string Quarantine(string path, QuarantineParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        var ptr = NativeMethods.agentstategraph_quarantine_apply(
            _handle.DangerousGetHandle(), refName, path, Json.Serialize(p));
        return ParseIdEnvelope(Strings.ConsumeUtf8(ptr), "quarantine_apply");
    }

    /// <summary>Releases a quarantine.</summary>
    public void Unquarantine(string path, string name, UntaintParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        var body = BuildRemoveParams(name, p.Reason, p.AgentId, p.Proof);
        var ptr = NativeMethods.agentstategraph_quarantine_release(
            _handle.DangerousGetHandle(), refName, path, body);
        Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "quarantine_release");
    }

    /// <summary>Applies a watch. Returns the new taint id.</summary>
    public string Watch(string path, WatchParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        var ptr = NativeMethods.agentstategraph_watch_apply(
            _handle.DangerousGetHandle(), refName, path, Json.Serialize(p));
        return ParseIdEnvelope(Strings.ConsumeUtf8(ptr), "watch_apply");
    }

    /// <summary>Removes a watch.</summary>
    public void Unwatch(string path, string name, UnwatchParams p, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(p);
        // Build a body that includes `name` (required by the FFI) plus the
        // serialized unwatch params (reason + agent_id).
        using var baseDoc = JsonDocument.Parse(Json.Serialize(p));
        var body = MergeWithName(baseDoc.RootElement, name);
        var ptr = NativeMethods.agentstategraph_watch_remove(
            _handle.DangerousGetHandle(), refName, path, body);
        Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "watch_remove");
    }

    /// <summary>
    /// Lists taints / quarantines / watches. Filters by <paramref name="pathPrefix"/>
    /// and / or <paramref name="kind"/> when provided.
    /// </summary>
    public IReadOnlyList<Taint> ListTaints(
        string? pathPrefix = null,
        TaintKind? kind = null,
        bool includeResolved = false)
    {
        ThrowIfDisposed();
        var kindStr = kind switch
        {
            TaintKind.Taint => "taint",
            TaintKind.Quarantine => "quarantine",
            TaintKind.Watch => "watch",
            _ => null,
        };
        var ptr = NativeMethods.agentstategraph_list_taints(
            _handle.DangerousGetHandle(), pathPrefix, kindStr, includeResolved);
        var raw = Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "list_taints");
        try
        {
            using var doc = JsonDocument.Parse(raw);
            if (doc.RootElement.TryGetProperty("taints", out var arr)
                && arr.ValueKind == JsonValueKind.Array)
            {
                var list = JsonSerializer.Deserialize<List<Taint>>(arr.GetRawText(), Json.Options)
                    ?? new List<Taint>();
                return list;
            }
            throw new AgentStateGraphException("list_taints", $"unexpected response: {raw}");
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException("list_taints", $"failed to parse response: {ex.Message}", ex);
        }
    }

    /// <summary>
    /// Checks the taint status for <paramref name="path"/> against
    /// <paramref name="agentId"/> at <paramref name="confidence"/>.
    /// </summary>
    public TaintCheck CheckTaint(string path, string agentId = "", double confidence = 1.0)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(path);
        var ptr = NativeMethods.agentstategraph_check_taint(
            _handle.DangerousGetHandle(), path, agentId ?? string.Empty, confidence);
        var raw = Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "check_taint");
        try
        {
            using var doc = JsonDocument.Parse(raw);
            if (doc.RootElement.TryGetProperty("check", out var check))
            {
                return JsonSerializer.Deserialize<TaintCheck>(check.GetRawText(), Json.Options)
                    ?? throw new AgentStateGraphException("check_taint", "deserialized null TaintCheck");
            }
            throw new AgentStateGraphException("check_taint", $"unexpected response: {raw}");
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException("check_taint", $"failed to parse response: {ex.Message}", ex);
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    private static string ParseIdEnvelope(string? raw, string operation)
    {
        var json = Json.ThrowIfError(raw, operation);
        try
        {
            using var doc = JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("id", out var id)
                && id.ValueKind == JsonValueKind.String)
            {
                return id.GetString() ?? string.Empty;
            }
            throw new AgentStateGraphException(operation, $"unexpected response: {json}");
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException(operation, $"failed to parse response: {ex.Message}", ex);
        }
    }

    private static string BuildRemoveParams(string name, string reason, string agentId, string? proof)
    {
        // The FFI expects {"name": ..., "reason": ..., "agent_id": ..., "proof"?}
        var obj = new Dictionary<string, object?>
        {
            ["name"] = name,
            ["reason"] = reason,
            ["agent_id"] = agentId,
        };
        if (proof is not null)
        {
            obj["proof"] = proof;
        }
        return JsonSerializer.Serialize(obj, Json.Options);
    }

    private static string MergeWithName(JsonElement baseObj, string name)
    {
        var dict = new Dictionary<string, JsonElement>(StringComparer.Ordinal);
        if (baseObj.ValueKind == JsonValueKind.Object)
        {
            foreach (var prop in baseObj.EnumerateObject())
            {
                dict[prop.Name] = prop.Value.Clone();
            }
        }
        // Serialize manually to avoid re-parsing. The FFI tolerates string
        // `name` at the top level alongside agent_id / reason fields.
        using var ms = new System.IO.MemoryStream();
        using (var writer = new Utf8JsonWriter(ms))
        {
            writer.WriteStartObject();
            writer.WriteString("name", name);
            foreach (var (k, v) in dict)
            {
                if (k == "name") continue;
                writer.WritePropertyName(k);
                v.WriteTo(writer);
            }
            writer.WriteEndObject();
        }
        return System.Text.Encoding.UTF8.GetString(ms.ToArray());
    }
}
