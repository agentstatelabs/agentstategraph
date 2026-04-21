using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
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
    /// <param name="refName">Branch / ref name.</param>
    /// <param name="prefix">Optional path prefix filter.</param>
    /// <param name="tenantFilter">
    /// Optional tenant scope (0.7.5-beta.1 §5c). When non-null the result is
    /// filtered client-side to policies whose <see cref="Policy.TenantId"/>
    /// is null (applies to all tenants) or equals <paramref name="tenantFilter"/>.
    /// The native FFI does not yet accept a tenant argument; this matches the
    /// Go §5c approach until a tenant-aware extern ships.
    /// </param>
    public IReadOnlyList<Policy> List(string refName, string? prefix = null, string? tenantFilter = null)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_list(H, refName, prefix);
        var all = Json.Deserialize<List<Policy>>(Strings.ConsumeUtf8(ptr), "list");
        return FilterByTenant(all, tenantFilter);
    }

    /// <summary>
    /// Currently-active policies: ratified AND <c>active_from &lt;= now</c>.
    /// </summary>
    /// <param name="refName">Branch / ref name.</param>
    /// <param name="prefix">Optional path prefix filter.</param>
    /// <param name="tenantFilter">
    /// Optional tenant scope — same client-side semantics as
    /// <see cref="List"/>. See that method's remarks for why the filter is
    /// client-side.
    /// </param>
    public IReadOnlyList<Policy> Active(string refName, string? prefix = null, string? tenantFilter = null)
    {
        ThrowIfDisposed();
        var ptr = NativeMethods.agentstategraph_policy_active(H, refName, prefix);
        var all = Json.Deserialize<List<Policy>>(Strings.ConsumeUtf8(ptr), "active");
        return FilterByTenant(all, tenantFilter);
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
        string agentId,
        string? tenantFilter = null)
    {
        ThrowIfDisposed();
        var situationJson = Json.Serialize(situation ?? new Dictionary<string, string>());
        var ptr = NativeMethods.agentstategraph_policy_evaluate(
            H, refName, situationJson, action, agentId);
        var decision = Json.Deserialize<Decision>(Strings.ConsumeUtf8(ptr), "evaluate");
        return ApplyTenantFilter(refName, decision, tenantFilter);
    }

    /// <summary>Runs the change-proposal evaluator (POLICY_V1.md §22.2).</summary>
    /// <param name="tenantFilter">
    /// Optional tenant scope (0.7.5-beta.1 §5c). When non-null and the decision's
    /// <c>matched_policy</c> carries a non-null <see cref="Policy.TenantId"/> that
    /// disagrees, the decision is rewritten to <see cref="Decision.NoPolicyMatch"/>.
    /// Matches the Go §5c client-side filter until a tenant-aware FFI ships.
    /// </param>
    public Decision EvaluateChange(string refName, ChangeProposal proposal, string? tenantFilter = null)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(proposal);
        var ptr = NativeMethods.agentstategraph_policy_evaluate_change(
            H, refName, Json.Serialize(proposal));
        var decision = Json.Deserialize<Decision>(Strings.ConsumeUtf8(ptr), "evaluate_change");
        return ApplyTenantFilter(refName, decision, tenantFilter);
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

    // -----------------------------------------------------------------------
    // Signing + external evaluator (0.7.5-beta.1 §5c)
    // -----------------------------------------------------------------------

    /// <summary>
    /// Invokes <c>agentstategraph_policy_sign</c>. The Rust implementation
    /// currently returns a stub envelope describing the signature that
    /// would be computed; callers parse the <see cref="JsonDocument"/>
    /// themselves. Pass <paramref name="signerKeyId"/> as <c>null</c> to
    /// let the FFI pick a default.
    /// </summary>
    public JsonDocument Sign(string refName, string path, string? signerKeyId = null)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(refName);
        ArgumentNullException.ThrowIfNull(path);
        var ptr = NativeMethods.agentstategraph_policy_sign(H, refName, path, signerKeyId);
        var raw = Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "policy_sign");
        return JsonDocument.Parse(raw);
    }

    /// <summary>
    /// Invokes <c>agentstategraph_policy_verify</c>. Returns the raw
    /// envelope as a <see cref="JsonDocument"/>.
    /// </summary>
    public JsonDocument Verify(string refName, string path)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(refName);
        ArgumentNullException.ThrowIfNull(path);
        var ptr = NativeMethods.agentstategraph_policy_verify(H, refName, path);
        var raw = Json.ThrowIfError(Strings.ConsumeUtf8(ptr), "policy_verify");
        return JsonDocument.Parse(raw);
    }

    /// <summary>
    /// Invokes <c>agentstategraph_policy_set_external_evaluator</c>. The
    /// Rust implementation is currently a stub that returns an error
    /// envelope — the raw response is wrapped in a
    /// <see cref="JsonDocument"/> and returned unchanged so callers can
    /// distinguish "stub" from a real registration once the implementation
    /// lands.
    /// </summary>
    public JsonDocument SetExternalEvaluator(string configJson)
    {
        ThrowIfDisposed();
        ArgumentNullException.ThrowIfNull(configJson);
        var ptr = NativeMethods.agentstategraph_policy_set_external_evaluator(H, configJson);
        var raw = Strings.ConsumeUtf8(ptr)
            ?? throw new AgentStateGraphException("policy_set_external_evaluator", "native FFI returned null");
        // NOTE: do NOT error-check — the current Rust stub intentionally
        // returns `{"error": "..."}` and callers need the envelope intact.
        return JsonDocument.Parse(raw);
    }

    // -----------------------------------------------------------------------
    // Tenant-scope helpers (0.7.5-beta.1 §5c, client-side filter)
    // -----------------------------------------------------------------------

    /// <summary>
    /// A policy is visible under <paramref name="filter"/> iff the filter is
    /// null, the policy's tenant is null (applies to all tenants), or the
    /// two match exactly.
    /// </summary>
    private static bool TenantMatches(Policy p, string? filter)
    {
        if (filter is null || p.TenantId is null)
        {
            return true;
        }
        return string.Equals(p.TenantId, filter, StringComparison.Ordinal);
    }

    private static IReadOnlyList<Policy> FilterByTenant(List<Policy> policies, string? filter)
    {
        if (filter is null)
        {
            return policies;
        }
        var filtered = new List<Policy>(policies.Count);
        foreach (var p in policies)
        {
            if (TenantMatches(p, filter))
            {
                filtered.Add(p);
            }
        }
        return filtered;
    }

    private Decision ApplyTenantFilter(string refName, Decision decision, string? filter)
    {
        if (filter is null)
        {
            return decision;
        }
        var matchedPath = decision switch
        {
            Decision.Allow a => a.MatchedPolicy,
            Decision.Deny d => d.MatchedPolicy,
            Decision.RequireApproval r => r.MatchedPolicy,
            _ => null,
        };
        if (string.IsNullOrEmpty(matchedPath))
        {
            return decision;
        }
        // matched_policy is `path@version` — strip the version to look up
        // the policy by path.
        var atIdx = matchedPath.IndexOf('@');
        var pathOnly = atIdx >= 0 ? matchedPath[..atIdx] : matchedPath;
        try
        {
            var matched = Get(refName, pathOnly);
            if (TenantMatches(matched, filter))
            {
                return decision;
            }
        }
        catch (AgentStateGraphException)
        {
            // If the lookup fails we err on the side of the original decision.
            return decision;
        }
        return new Decision.NoPolicyMatch();
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
