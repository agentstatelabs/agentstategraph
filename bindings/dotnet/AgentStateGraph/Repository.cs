using System;
using System.Collections.Generic;
using System.Text.Json;
using AgentStateGraph.Interop;

namespace AgentStateGraph;

/// <summary>
/// An AgentStateGraph repository — an AI-native, commit-versioned state
/// store. The handle is <see cref="IDisposable"/>; always dispose (ideally
/// via <c>using</c>) to release the underlying native resources promptly.
/// </summary>
/// <remarks>
/// Construct with <see cref="Repository()"/> for an ephemeral in-memory
/// store or <see cref="OpenSqlite(string)"/> for a SQLite-backed durable
/// store. Derived handles (<see cref="TaskStore"/>, <see cref="PolicyStore"/>)
/// hold a reference back to this instance to keep the repository alive
/// for the lifetime of any derived store.
/// </remarks>
public sealed class Repository : IDisposable
{
    private readonly SafeRepoHandle _handle;
    private bool _disposed;

    /// <summary>Default ref for all operations when a caller omits one.</summary>
    public const string DefaultRef = "main";

    /// <summary>Creates an ephemeral, in-memory repository.</summary>
    public Repository()
    {
        var raw = NativeMethods.agentstategraph_new_memory();
        if (raw == IntPtr.Zero)
        {
            throw new AgentStateGraphException("new", "failed to create memory repository");
        }
        _handle = SafeRepoHandle.Adopt(raw);
    }

    private Repository(SafeRepoHandle handle)
    {
        _handle = handle;
    }

    /// <summary>Opens (or creates) a SQLite-backed repository at <paramref name="path"/>.</summary>
    public static Repository OpenSqlite(string path)
    {
        ArgumentNullException.ThrowIfNull(path);
        var raw = NativeMethods.agentstategraph_new_sqlite(path);
        if (raw == IntPtr.Zero)
        {
            throw new AgentStateGraphException("open_sqlite", $"failed to open sqlite repository at '{path}'");
        }
        return new Repository(SafeRepoHandle.Adopt(raw));
    }

    /// <summary>Internal accessor used by <see cref="TaskStore"/> / <see cref="PolicyStore"/>.</summary>
    internal SafeRepoHandle Handle => _handle;

    private void ThrowIfDisposed()
    {
        if (_disposed)
        {
            throw new ObjectDisposedException(nameof(Repository));
        }
    }

    // -----------------------------------------------------------------------
    // Read / write operations
    // -----------------------------------------------------------------------

    /// <summary>Returns the JSON value at <paramref name="path"/> as a string.</summary>
    public string Get(string path, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_get(_handle.DangerousGetHandle(), refName, path);
        var raw = Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("get", "native FFI returned null");
        return raw;
    }

    /// <summary>
    /// Writes <paramref name="jsonValue"/> at <paramref name="path"/> and
    /// creates a new commit. Returns the short commit id.
    /// </summary>
    public CommitId Set(
        string path,
        string jsonValue,
        string intentCategory,
        string intentDescription,
        string refName = DefaultRef)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_set(
            _handle.DangerousGetHandle(), refName, path, jsonValue, intentCategory, intentDescription);
        var raw = Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("set", "native FFI returned null");
        return new CommitId(raw);
    }

    /// <summary>
    /// Convenience: serializes <paramref name="value"/> to JSON via the
    /// shared <see cref="JsonSerializer"/> options, then delegates to
    /// <see cref="Set(string, string, string, string, string)"/>.
    /// </summary>
    public CommitId SetJson<T>(
        string path,
        T value,
        string intentCategory,
        string intentDescription,
        string refName = DefaultRef)
    {
        var json = Json.Serialize(value);
        return Set(path, json, intentCategory, intentDescription, refName);
    }

    /// <summary>Removes the value at <paramref name="path"/> and creates a new commit.</summary>
    public CommitId Delete(
        string path,
        string intentCategory,
        string intentDescription,
        string refName = DefaultRef)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_delete(
            _handle.DangerousGetHandle(), refName, path, intentCategory, intentDescription);
        var raw = Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("delete", "native FFI returned null");
        return new CommitId(raw);
    }

    /// <summary>
    /// Creates a new branch <paramref name="name"/> starting at
    /// <paramref name="from"/> (or the default ref if null).
    /// </summary>
    public string Branch(string name, string? from = null)
    {
        ThrowIfDisposed();
        // FFI expects a non-null `from`; substitute the default ref when
        // the caller omits it. (The Go binding passes "main" explicitly;
        // the C# API models the default as an optional parameter.)
        var fromRef = from ?? "main";
        var ptr = NativeMethods.agentstategraph_branch(_handle.DangerousGetHandle(), name, fromRef);
        return Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("branch", "native FFI returned null");
    }

    /// <summary>
    /// Returns every branch whose name starts with <paramref name="prefix"/>.
    /// Pass <c>null</c> (or an empty string) for no filter.
    /// </summary>
    public IReadOnlyList<BranchEntry> ListBranches(string? prefix = null)
    {
        ThrowIfDisposed();
        var filter = string.IsNullOrEmpty(prefix) ? null : prefix;
        var ptr = NativeMethods.agentstategraph_list_branches(_handle.DangerousGetHandle(), filter);
        var raw = Strings.ConsumeUtf8(ptr);
        return Json.Deserialize<List<BranchEntry>>(raw, "list_branches");
    }

    /// <summary>
    /// Removes the branch <paramref name="name"/>. Returns <c>true</c> if
    /// the branch existed, <c>false</c> if it did not (non-error).
    /// </summary>
    public bool DeleteBranch(string name)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(name);
        var ptr = NativeMethods.agentstategraph_delete_branch(_handle.DangerousGetHandle(), name);
        var raw = Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "delete_branch");
        try
        {
            using var doc = JsonDocument.Parse(raw);
            if (doc.RootElement.ValueKind == JsonValueKind.Object
                && doc.RootElement.TryGetProperty("deleted", out var deleted)
                && deleted.ValueKind is JsonValueKind.True or JsonValueKind.False)
            {
                return deleted.GetBoolean();
            }
            throw new AgentStateGraphException("delete_branch", $"unexpected response: {raw}");
        }
        catch (JsonException ex)
        {
            throw new AgentStateGraphException("delete_branch", $"failed to parse response: {ex.Message}", ex);
        }
    }

    /// <summary>
    /// Computes a structured diff between two refs. Returns raw JSON; the
    /// shape mirrors the Rust <c>Diff</c> type and is left undecoded here
    /// to match the Go binding's deliberate pass-through.
    /// </summary>
    public string Diff(string refA, string refB)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_diff(_handle.DangerousGetHandle(), refA, refB);
        return Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("diff", "native FFI returned null");
    }

    /// <summary>
    /// Merges <paramref name="source"/> into <paramref name="target"/>.
    /// Returns the short commit id of the merge commit.
    /// </summary>
    public CommitId Merge(string source, string target, string description)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_merge(
            _handle.DangerousGetHandle(), source, target, description);
        var raw = Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("merge", "native FFI returned null");
        // The FFI surfaces merge errors as "error:<msg>" prefixed strings
        // (agentstategraph.go does the same defensive check).
        if (raw.StartsWith("error:", StringComparison.Ordinal))
        {
            throw new AgentStateGraphException("merge", raw[6..]);
        }
        return new CommitId(raw);
    }

    /// <summary>
    /// Returns up to <paramref name="limit"/> commits on
    /// <paramref name="refName"/>, newest first.
    /// </summary>
    public IReadOnlyList<Commit> Log(uint limit = 100, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_log(_handle.DangerousGetHandle(), refName, limit);
        var raw = Strings.ConsumeUtf8(ptr);
        return Json.Deserialize<List<Commit>>(raw, "log");
    }

    /// <summary>
    /// Returns who last modified <paramref name="path"/> and why. Raw JSON
    /// blame entry — shape is defined by <c>agentstategraph_core::Blame</c>.
    /// </summary>
    public string Blame(string path, string refName = DefaultRef)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_blame(_handle.DangerousGetHandle(), refName, path);
        return Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("blame", "native FFI returned null");
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        _handle.Dispose();
    }
}
