using System;
using System.Collections.Generic;
using System.Linq;
using AgentStateGraph.Interop;

namespace AgentStateGraph;

/// <summary>
/// Policy store bound to a <see cref="Repository"/>, path prefix, and
/// agent id. All policy writes commit as <c>IntentCategory::Plan</c>.
/// </summary>
/// <remarks>
/// The store holds a reference to its owning <see cref="Repository"/> so
/// the repo cannot be disposed out from under it.
/// </remarks>
public sealed class PolicyStore : IDisposable
{
    private readonly SafePolicyStoreHandle _handle;
    private readonly Repository _repo;
    private bool _disposed;

    /// <summary>Creates a new PolicyStore on top of <paramref name="repo"/>.</summary>
    public PolicyStore(Repository repo, string prefix, string agentId)
    {
        ArgumentNullException.ThrowIfNull(repo);
        ArgumentNullException.ThrowIfNull(prefix);
        ArgumentNullException.ThrowIfNull(agentId);

        var raw = NativeMethods.agentstategraph_policy_store_new(
            repo.Handle.DangerousGetHandle(), prefix, agentId);
        if (raw == IntPtr.Zero)
        {
            throw new AgentStateGraphException("policy_store_new", "failed to create policy store");
        }
        _handle = SafePolicyStoreHandle.Adopt(raw);
        _repo = repo;
    }

    private void ThrowIfDisposed()
    {
        if (_disposed)
        {
            throw new ObjectDisposedException(nameof(PolicyStore));
        }
    }

    private IntPtr H => _handle.DangerousGetHandle();

    // -----------------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------------

    /// <summary>
    /// Registers a new (unratified) policy and returns its
    /// <c>path@version</c> handle.
    /// </summary>
    public string Propose(string refName, Policy policy)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(policy);
        var ptr = NativeMethods.agentstategraph_policy_propose(H, refName, Json.Serialize(policy));
        return Json.Deserialize<string>(Strings.ConsumeUtf8(ptr), "propose");
    }

    /// <summary>
    /// Ratifies an unratified proposal. <paramref name="reasoning"/> must
    /// be non-empty (enforced by the Rust store).
    /// </summary>
    public void Ratify(string refName, string path, string ratifier, string reasoning)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_ratify(H, refName, path, ratifier, reasoning);
        Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "ratify");
    }

    /// <summary>
    /// Replaces the active policy at <paramref name="path"/> with
    /// <paramref name="newPolicy"/> and returns the new <c>path@version</c>
    /// handle.
    /// </summary>
    public string Supersede(string refName, string path, Policy newPolicy)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(newPolicy);
        var ptr = NativeMethods.agentstategraph_policy_supersede(
            H, refName, path, Json.Serialize(newPolicy));
        return Json.Deserialize<string>(Strings.ConsumeUtf8(ptr), "supersede");
    }

    // -----------------------------------------------------------------------
    // Read operations
    // -----------------------------------------------------------------------

    /// <summary>
    /// Every policy under <paramref name="prefix"/> (or all when <c>null</c>).
    /// Unratified proposals are included.
    /// </summary>
    public IReadOnlyList<Policy> List(string refName, string? prefix = null)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_list(H, refName, prefix);
        return Json.Deserialize<List<Policy>>(Strings.ConsumeUtf8(ptr), "list");
    }

    /// <summary>
    /// Currently-active policies: ratified AND <c>active_from &lt;= now</c>.
    /// </summary>
    public IReadOnlyList<Policy> Active(string refName, string? prefix = null)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_active(H, refName, prefix);
        return Json.Deserialize<List<Policy>>(Strings.ConsumeUtf8(ptr), "active");
    }

    /// <summary>The active (or latest proposed) policy at <paramref name="path"/>.</summary>
    public Policy Get(string refName, string path)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_get(H, refName, path);
        return Json.Deserialize<Policy>(Strings.ConsumeUtf8(ptr), "get");
    }

    /// <summary>
    /// Walks the supersedes chain at <paramref name="path"/>, oldest-first
    /// through the current version.
    /// </summary>
    public IReadOnlyList<Policy> History(string refName, string path)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_history(H, refName, path);
        return Json.Deserialize<List<Policy>>(Strings.ConsumeUtf8(ptr), "history");
    }

    // -----------------------------------------------------------------------
    // Evaluation
    // -----------------------------------------------------------------------

    /// <summary>
    /// Runs the authorization evaluator (POLICY_V1.md §5).
    /// <paramref name="situation"/> is a flat fact map.
    /// </summary>
    public Decision Evaluate(
        string refName,
        IReadOnlyDictionary<string, string>? situation,
        string action,
        string agentId)
    {
        ThrowIfDisposed();
        var situationJson = Json.Serialize(situation ?? new Dictionary<string, string>());
        var ptr = NativeMethods.agentstategraph_policy_evaluate(
            H, refName, situationJson, action, agentId);
        return Json.Deserialize<Decision>(Strings.ConsumeUtf8(ptr), "evaluate");
    }

    /// <summary>Runs the change-proposal evaluator (POLICY_V1.md §22.2).</summary>
    public Decision EvaluateChange(string refName, ChangeProposal proposal)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(proposal);
        var ptr = NativeMethods.agentstategraph_policy_evaluate_change(
            H, refName, Json.Serialize(proposal));
        return Json.Deserialize<Decision>(Strings.ConsumeUtf8(ptr), "evaluate_change");
    }

    /// <summary>
    /// Active policies whose <c>triggers</c> intersect <paramref name="tokens"/>.
    /// Binding-level: calls <see cref="Active"/> and filters locally, matching
    /// the Py / TS / Go / WASM / FFI pattern.
    /// </summary>
    /// <remarks>
    /// The native FFI exposes <c>agentstategraph_policy_check_tokens</c>;
    /// we still compute the intersection locally so that a binding / runtime
    /// mismatch on trigger semantics surfaces as a C# diff rather than
    /// silently drifting. If the FFI's native result is preferred, call
    /// through <see cref="Interop.NativeMethods"/> directly — kept internal
    /// for now.
    /// </remarks>
    public IReadOnlyList<Policy> CheckTokens(string refName, IReadOnlyList<string> tokens)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(tokens);
        if (tokens.Count == 0)
        {
            return Array.Empty<Policy>();
        }
        var tokenSet = new HashSet<string>(tokens, StringComparer.Ordinal);
        var active = Active(refName);
        var matched = new List<Policy>();
        foreach (var p in active)
        {
            if (p.Triggers is { Count: > 0 } && p.Triggers.Any(tokenSet.Contains))
            {
                matched.Add(p);
            }
        }
        return matched;
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        _handle.Dispose();
        GC.KeepAlive(_repo);
    }
}
