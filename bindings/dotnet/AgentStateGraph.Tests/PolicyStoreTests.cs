using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using AgentStateGraph;
using Xunit;

namespace AgentStateGraph.Tests;

/// <summary>
/// Mirrors <c>bindings/python/tests/test_policy.py</c> scenario-for-scenario
/// through the C# <see cref="PolicyStore"/> surface. Plus C#-idiomatic
/// coverage for ratifier validation (the Go §5 fix) and prefix filtering.
/// </summary>
public sealed class PolicyStoreTests
{
    private const string Ref = Repository.DefaultRef;

    // -----------------------------------------------------------------------
    // Propose / ratify (Python: test_propose_creates_unratified_policy,
    // test_ratify_promotes_policy).
    // -----------------------------------------------------------------------

    [Fact]
    public void Propose_CreatesUnratifiedPolicy()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        var handle = ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/k8s/pod-failing"));

        Assert.Equal("infra/k8s/pod-failing@1", handle);
        var fetched = ps.Get(Ref, "infra/k8s/pod-failing");
        Assert.Equal(1u, fetched.Version);
        Assert.Null(fetched.RatifiedBy);
        Assert.Equal("xunit", fetched.ProposedBy);
        Assert.False(fetched.IsRatified);
    }

    [Fact]
    public void Ratify_PromotesPolicy()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/restart",
            allow: new[] { new AuthorizedAction("restart_pod") }));
        ps.Ratify(Ref, "infra/restart", "ops-lead", "approved after review");

        var p = ps.Get(Ref, "infra/restart");
        Assert.Equal("ops-lead", p.RatifiedBy);
        Assert.Equal("approved after review", p.RatificationReasoning);
        Assert.NotNull(p.RatifiedAt);
        Assert.True(p.IsRatified);
    }

    [Fact]
    public void Ratify_RejectsEmptyRatifier()
    {
        // Matches the Go §5 fix — empty ratifier must surface a clear error
        // rather than silently succeed.
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/empty-ratifier"));

        var ex = Assert.Throws<AgentStateGraphException>(() =>
            ps.Ratify(Ref, "infra/empty-ratifier", string.Empty, "whatever"));
        Assert.Equal("ratify", ex.Operation);
    }

    // -----------------------------------------------------------------------
    // Supersede chain (Python: test_supersede_chain_and_history).
    // -----------------------------------------------------------------------

    [Fact]
    public void Supersede_Chain_AndHistory()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/scale",
            allow: new[] { new AuthorizedAction("scale_up") }));
        ps.Ratify(Ref, "infra/scale", "ops", "v1");

        var v2 = TestHelpers.SkeletonPolicy(
            "infra/scale",
            allow: new[] { new AuthorizedAction("scale_up"), new AuthorizedAction("scale_down") })
            with { RatifiedBy = "ops", RatifiedAt = DateTimeOffset.UtcNow };
        var handle = ps.Supersede(Ref, "infra/scale", v2);
        Assert.Equal("infra/scale@2", handle);

        var history = ps.History(Ref, "infra/scale");
        Assert.Equal(new ulong[] { 1, 2 }, history.Select(p => p.Version).ToArray());
        Assert.Equal("infra/scale@1", history[^1].Supersedes);
    }

    // -----------------------------------------------------------------------
    // Evaluate — all four Decision kinds (Python: test_evaluate_allow / _deny
    // / _require_approval / _no_match).
    // -----------------------------------------------------------------------

    [Fact]
    public void Evaluate_Allow()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/restart",
            situationSelector: new Selector.Eq("namespace", "prod"),
            allow: new[] { new AuthorizedAction("restart_pod") }));
        ps.Ratify(Ref, "infra/restart", "ops", "ok");

        var d = ps.Evaluate(Ref,
            new Dictionary<string, string> { ["namespace"] = "prod" },
            "restart_pod", "agent-1");

        var allow = Assert.IsType<Decision.Allow>(d);
        Assert.Equal("infra/restart@1", allow.MatchedPolicy);
        Assert.Equal("allow", d.KindTag);
    }

    [Fact]
    public void Evaluate_Deny()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/no-delete",
            deny: new[] { new AuthorizedAction("delete_node", Condition: "always") }));
        ps.Ratify(Ref, "infra/no-delete", "ops", "ok");

        var d = ps.Evaluate(Ref, null, "delete_node", "agent-1");
        Assert.IsType<Decision.Deny>(d);
    }

    [Fact]
    public void Evaluate_RequireApproval_WithBlockFallback()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/risky",
            requireApproval: new[]
            {
                new ApprovalRule(
                    "truncate_index",
                    new[] { "human" },
                    new FallbackAction.Block()),
            }));
        ps.Ratify(Ref, "infra/risky", "ops", "ok");

        var d = ps.Evaluate(Ref, null, "truncate_index", "agent-1");
        var req = Assert.IsType<Decision.RequireApproval>(d);
        Assert.Equal(new[] { "human" }, req.Approvers);
        Assert.IsType<FallbackAction.Block>(req.Fallback);
    }

    [Fact]
    public void Evaluate_NoMatch_WhenNoPolicies()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        var d = ps.Evaluate(Ref, null, "anything", "agent-1");
        Assert.IsType<Decision.NoPolicyMatch>(d);
        Assert.Equal("no_policy_match", d.KindTag);
    }

    // -----------------------------------------------------------------------
    // EvaluateChange — triggers + required_fields + fallback
    // (Python: test_evaluate_change_with_triggers_and_fallback,
    //         test_evaluate_change_missing_required_fields).
    // -----------------------------------------------------------------------

    [Fact]
    public void EvaluateChange_WithTriggersAndFallback()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/high-cost",
            triggers: new[] { "reindex", "downtime" },
            requiredFields: new[] { "estimated_downtime" },
            requireApproval: new[]
            {
                new ApprovalRule(
                    "promote",
                    new[] { "human" },
                    new FallbackAction.LowestRiskAlternative()),
            },
            severity: Severity.High));
        ps.Ratify(Ref, "infra/high-cost", "ops", "big changes need approval");

        var proposal = new ChangeProposal(
            Action: "promote",
            AgentId: "agent-1",
            Intent: "merge option C",
            PreferredOption: "spec-7",
            Alternatives: new[] { "spec-1", "spec-3" },
            Tokens: new[] { "reindex" },
            AttachedFields: new Dictionary<string, string> { ["estimated_downtime"] = "5m" });

        var d = ps.EvaluateChange(Ref, proposal);
        var req = Assert.IsType<Decision.RequireApproval>(d);
        Assert.IsType<FallbackAction.LowestRiskAlternative>(req.Fallback);
    }

    [Fact]
    public void EvaluateChange_MissingRequiredFields_ShortCircuitsToApproval()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/needs-downtime",
            triggers: new[] { "reindex" },
            requiredFields: new[] { "estimated_downtime" },
            requireApproval: new[]
            {
                new ApprovalRule(
                    "promote",
                    new[] { "human" },
                    new FallbackAction.Block()),
            }));
        ps.Ratify(Ref, "infra/needs-downtime", "ops", "ok");

        var proposal = new ChangeProposal(
            Action: "promote",
            AgentId: "agent-1",
            Intent: string.Empty,
            PreferredOption: "x",
            Tokens: new[] { "reindex" },
            AttachedFields: new Dictionary<string, string>());

        var d = ps.EvaluateChange(Ref, proposal);
        Assert.IsType<Decision.RequireApproval>(d);
    }

    // -----------------------------------------------------------------------
    // active_from scheduled activation (§1 of the plan, mirrors Python's
    // test_evaluate_ignores_not_yet_active_policy).
    // -----------------------------------------------------------------------

    [Fact]
    public void Evaluate_IgnoresNotYetActivePolicy()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        var future = DateTimeOffset.UtcNow.AddHours(1);
        var pol = TestHelpers.SkeletonPolicy(
            "infra/future",
            allow: new[] { new AuthorizedAction("do_it") },
            activeFrom: future);
        ps.Propose(Ref, pol);
        ps.Ratify(Ref, "infra/future", "ops", "scheduled");

        var d = ps.Evaluate(Ref, null, "do_it", "agent-1");
        Assert.IsType<Decision.NoPolicyMatch>(d);

        var actives = ps.Active(Ref);
        Assert.DoesNotContain(actives, p => p.Path == "infra/future");
    }

    [Fact]
    public void Evaluate_PastActiveFrom_NormalEvaluation()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        var past = DateTimeOffset.UtcNow.AddHours(-1);
        var pol = TestHelpers.SkeletonPolicy(
            "infra/past",
            allow: new[] { new AuthorizedAction("do_it") },
            activeFrom: past);
        ps.Propose(Ref, pol);
        ps.Ratify(Ref, "infra/past", "ops", "backfilled");

        var d = ps.Evaluate(Ref, null, "do_it", "agent-1");
        Assert.IsType<Decision.Allow>(d);
    }

    // -----------------------------------------------------------------------
    // check_tokens trigger intersection (Python:
    // test_check_tokens_filters_by_trigger_intersection).
    // -----------------------------------------------------------------------

    [Fact]
    public void CheckTokens_TriggerIntersection()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/with-reindex", triggers: new[] { "reindex" }));
        ps.Ratify(Ref, "infra/with-reindex", "ops", "ok");
        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/with-network", triggers: new[] { "network" }));
        ps.Ratify(Ref, "infra/with-network", "ops", "ok");

        var matched = ps.CheckTokens(Ref, new[] { "reindex" });
        Assert.Equal(new[] { "infra/with-reindex" },
            matched.Select(p => p.Path).OrderBy(x => x).ToArray());

        var matchedAll = ps.CheckTokens(Ref, new[] { "reindex", "network" });
        Assert.Equal(
            new[] { "infra/with-network", "infra/with-reindex" },
            matchedAll.Select(p => p.Path).OrderBy(x => x).ToArray());
    }

    [Fact]
    public void CheckTokens_EmptyTokens_ReturnsEmpty()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");
        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/any", triggers: new[] { "any" }));
        ps.Ratify(Ref, "infra/any", "ops", "ok");

        var matched = ps.CheckTokens(Ref, Array.Empty<string>());
        Assert.Empty(matched);
    }

    // -----------------------------------------------------------------------
    // List + active + prefix (Python: test_list_and_active_filters).
    // -----------------------------------------------------------------------

    [Fact]
    public void List_IncludesUnratified_ActiveFiltersRatified()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/a"));
        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/b"));
        ps.Ratify(Ref, "infra/b", "ops", "ok");

        var listed = ps.List(Ref).Select(p => p.Path).OrderBy(x => x).ToArray();
        Assert.Equal(new[] { "infra/a", "infra/b" }, listed);

        var actives = ps.Active(Ref).Select(p => p.Path).ToArray();
        Assert.Equal(new[] { "infra/b" }, actives);
    }

    [Fact]
    public void List_PrefixFilter()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/a"));
        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/b"));

        var onlyA = ps.List(Ref, "infra/a").Select(p => p.Path).ToArray();
        Assert.Equal(new[] { "infra/a" }, onlyA);
    }

    // -----------------------------------------------------------------------
    // Get / history walking (supplements Python coverage).
    // -----------------------------------------------------------------------

    [Fact]
    public void Get_ByPath_ReturnsLatestVersion()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/getme"));
        ps.Ratify(Ref, "infra/getme", "ops", "ok");

        var p = ps.Get(Ref, "infra/getme");
        Assert.Equal("infra/getme", p.Path);
        Assert.Equal(1u, p.Version);
    }

    [Fact]
    public void History_SingleVersion_ReturnsOne()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/solo"));
        ps.Ratify(Ref, "infra/solo", "ops", "ok");

        var history = ps.History(Ref, "infra/solo");
        Assert.Single(history);
        Assert.Equal(1u, history[0].Version);
    }

    [Fact]
    public void PolicyStore_NullArguments_Throw()
    {
        using var repo = TestHelpers.FreshRepo();
        Assert.Throws<ArgumentNullException>(() =>
            new PolicyStore(null!, "/p", "a"));
        Assert.Throws<ArgumentNullException>(() =>
            new PolicyStore(repo, null!, "a"));
        Assert.Throws<ArgumentNullException>(() =>
            new PolicyStore(repo, "/p", null!));
    }

    [Fact]
    public void PolicyStore_DisposedAccess_Throws()
    {
        using var repo = TestHelpers.FreshRepo();
        var ps = new PolicyStore(repo, "/policies", "xunit");
        ps.Dispose();
        Assert.Throws<ObjectDisposedException>(() =>
            ps.List(Ref));
    }

    [Fact]
    public void Propose_NullPolicy_Throws()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");
        Assert.Throws<ArgumentNullException>(() => ps.Propose(Ref, null!));
    }

    // -----------------------------------------------------------------------
    // 0.7.5-beta.1 §5e — signing + multi-tenant + external evaluator.
    // -----------------------------------------------------------------------

    [Fact]
    public void Policy_SignatureField_RoundTrips()
    {
        var sig = new PolicySignature(
            Algorithm: "ed25519",
            SignerKeyId: "key-1",
            SignatureHex: "deadbeef");
        var policy = TestHelpers.SkeletonPolicy("infra/signed") with { Signature = sig };

        var back = TestHelpers.JsonRoundTrip(policy);
        Assert.NotNull(back.Signature);
        Assert.Equal("ed25519", back.Signature!.Algorithm);
        Assert.Equal("key-1", back.Signature.SignerKeyId);
        Assert.Equal("deadbeef", back.Signature.SignatureHex);
    }

    [Fact]
    public void Policy_TenantId_RoundTrips()
    {
        var policy = TestHelpers.SkeletonPolicy("infra/tenant-a") with { TenantId = "tenant-a" };
        var back = TestHelpers.JsonRoundTrip(policy);
        Assert.Equal("tenant-a", back.TenantId);

        var unscoped = TestHelpers.SkeletonPolicy("infra/all-tenants");
        var back2 = TestHelpers.JsonRoundTrip(unscoped);
        Assert.Null(back2.TenantId);
    }

    [Fact]
    public void Policy_ExternalEvaluator_RoundTrips()
    {
        // Exercise each variant of both the outer and inner tagged unions.
        var rego = TestHelpers.SkeletonPolicy("infra/rego") with
        {
            ExternalEvaluator = new ExternalEvaluatorRef.Rego(
                new EvaluatorSource.Inline("package p\nallow { true }")),
        };
        var backRego = TestHelpers.JsonRoundTrip(rego);
        var evalRego = Assert.IsType<ExternalEvaluatorRef.Rego>(backRego.ExternalEvaluator);
        Assert.Equal("rego", backRego.ExternalEvaluator!.KindTag);
        var inlineSrc = Assert.IsType<EvaluatorSource.Inline>(evalRego.Source);
        Assert.Equal("inline", evalRego.Source.KindTag);
        Assert.Contains("allow", inlineSrc.Body);

        var cedar = TestHelpers.SkeletonPolicy("infra/cedar") with
        {
            ExternalEvaluator = new ExternalEvaluatorRef.Cedar(
                new EvaluatorSource.FilePath("/etc/cedar/policy.cedar")),
        };
        var backCedar = TestHelpers.JsonRoundTrip(cedar);
        var evalCedar = Assert.IsType<ExternalEvaluatorRef.Cedar>(backCedar.ExternalEvaluator);
        var fileSrc = Assert.IsType<EvaluatorSource.FilePath>(evalCedar.Source);
        Assert.Equal("file_path", evalCedar.Source.KindTag);
        Assert.Equal("/etc/cedar/policy.cedar", fileSrc.Path);

        var wasm = TestHelpers.SkeletonPolicy("infra/wasm") with
        {
            ExternalEvaluator = new ExternalEvaluatorRef.Wasm(
                new EvaluatorSource.CommitRef("policies/eval.wasm")),
        };
        var backWasm = TestHelpers.JsonRoundTrip(wasm);
        var evalWasm = Assert.IsType<ExternalEvaluatorRef.Wasm>(backWasm.ExternalEvaluator);
        var commitSrc = Assert.IsType<EvaluatorSource.CommitRef>(evalWasm.Source);
        Assert.Equal("commit_ref", evalWasm.Source.KindTag);
        Assert.Equal("policies/eval.wasm", commitSrc.Path);
    }

    [Fact]
    public void Session_ScopeTenant_RoundTrips()
    {
        var session = new Session(
            Id: "sess-1",
            AgentId: "agent-1",
            WorkingBranch: "main",
            Head: "abc",
            Status: SessionStatus.Active,
            CreatedAt: DateTimeOffset.UtcNow,
            ScopeTenant: "tenant-xyz");
        var back = TestHelpers.JsonRoundTrip(session);
        Assert.Equal("tenant-xyz", back.ScopeTenant);

        var unscoped = session with { ScopeTenant = null };
        Assert.Null(TestHelpers.JsonRoundTrip(unscoped).ScopeTenant);
    }

    [Fact]
    public void PolicyStore_Sign_ReturnsStubOrSignature()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");
        ps.Propose(Ref, TestHelpers.SkeletonPolicy("infra/signable"));
        ps.Ratify(Ref, "infra/signable", "ops", "ok");

        using var doc = ps.Sign(Ref, "infra/signable", signerKeyId: null);
        Assert.Equal(JsonValueKind.Object, doc.RootElement.ValueKind);
        // With no signer registered, the FFI either returns a stub
        // envelope or an {"error": "..."} payload; either is acceptable
        // for the pass-through binding. We only assert the shape parses.
        var isStub = doc.RootElement.TryGetProperty("signature", out _)
            || doc.RootElement.TryGetProperty("stub", out _)
            || doc.RootElement.TryGetProperty("algorithm", out _);
        var isError = doc.RootElement.TryGetProperty("error", out _);
        Assert.True(isStub || isError, $"unexpected Sign envelope: {doc.RootElement.GetRawText()}");
    }

    [Fact]
    public void PolicyStore_SetExternalEvaluator_ReturnsStubEnvelope()
    {
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");

        // Minimal well-formed config — the Rust side is a stub so any
        // valid JSON is fine.
        var cfg = "{\"kind\":\"rego\",\"source\":{\"kind\":\"inline\",\"body\":\"package p\"}}";
        using var doc = ps.SetExternalEvaluator(cfg);
        Assert.Equal(JsonValueKind.Object, doc.RootElement.ValueKind);
        // Stub currently returns {"error": "..."} — binding must surface
        // it verbatim rather than throwing.
        var hasError = doc.RootElement.TryGetProperty("error", out _);
        var hasOk = doc.RootElement.TryGetProperty("ok", out _);
        Assert.True(hasError || hasOk,
            $"unexpected SetExternalEvaluator envelope: {doc.RootElement.GetRawText()}");
    }

    [Fact]
    public void SeveritySurfaces_OnRoundTrip()
    {
        // Severity round-trips as snake_case through the shared JSON options.
        using var repo = TestHelpers.FreshRepo();
        using var ps = new PolicyStore(repo, "/policies", "xunit");
        ps.Propose(Ref, TestHelpers.SkeletonPolicy(
            "infra/critical-one",
            severity: Severity.Critical));
        ps.Ratify(Ref, "infra/critical-one", "ops", "ok");
        var p = ps.Get(Ref, "infra/critical-one");
        Assert.Equal(Severity.Critical, p.Severity);
    }
}
