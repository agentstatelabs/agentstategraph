using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace AgentStateGraph;

/// <summary>
/// Short commit id (BLAKE3 prefix) returned by <see cref="Repository.Set"/>,
/// <see cref="Repository.Delete"/>, <see cref="Repository.Merge"/>, etc. A
/// simple value wrapper so the API doesn't leak raw strings everywhere.
/// </summary>
public readonly record struct CommitId(string Value)
{
    public override string ToString() => Value;

    public static implicit operator string(CommitId id) => id.Value;
}

/// <summary>
/// Entry in a commit log. Matches the JSON array shape emitted by
/// <c>agentstategraph_log</c>: short id + agent + intent category / description
/// + reasoning + confidence.
/// </summary>
public sealed record Commit(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("agent")] string Agent,
    [property: JsonPropertyName("intent_category")] string IntentCategory,
    [property: JsonPropertyName("intent_description")] string IntentDescription,
    [property: JsonPropertyName("reasoning")] string? Reasoning = null,
    [property: JsonPropertyName("confidence")] double? Confidence = null);

/// <summary>
/// One <c>allow</c> or <c>deny</c> rule on a <see cref="Policy"/>.
/// </summary>
public sealed record AuthorizedAction(
    [property: JsonPropertyName("action")] string Action,
    [property: JsonPropertyName("condition")] string? Condition = null,
    [property: JsonPropertyName("preconditions")] IReadOnlyList<string>? Preconditions = null);

/// <summary>
/// One <c>require_approval</c> rule on a <see cref="Policy"/>.
/// </summary>
public sealed record ApprovalRule(
    [property: JsonPropertyName("action")] string Action,
    [property: JsonPropertyName("approvers")] IReadOnlyList<string> Approvers,
    [property: JsonPropertyName("fallback")] FallbackAction Fallback,
    /// <summary>Timeout in milliseconds (matches Rust's duration_opt serde helper).</summary>
    [property: JsonPropertyName("timeout")] ulong? Timeout = null);

/// <summary>
/// One step in a <see cref="Policy"/>'s procedure.
/// </summary>
public sealed record ProcedureStep(
    [property: JsonPropertyName("action")] string Action,
    [property: JsonPropertyName("if_previous_failed")] string? IfPreviousFailed = null);

/// <summary>
/// The unit of authorization + procedure. Matches the Rust
/// <c>agentstategraph_policy::Policy</c> record one-to-one.
/// </summary>
public sealed record Policy(
    [property: JsonPropertyName("path")] string Path,
    [property: JsonPropertyName("version")] ulong Version,
    [property: JsonPropertyName("situation")] string Situation,
    [property: JsonPropertyName("situation_selector")] Selector SituationSelector,
    [property: JsonPropertyName("proposed_by")] string ProposedBy,
    [property: JsonPropertyName("proposed_at")] DateTimeOffset ProposedAt,
    [property: JsonPropertyName("active_from")] DateTimeOffset ActiveFrom,
    [property: JsonPropertyName("severity")] Severity Severity = Severity.Low,
    [property: JsonPropertyName("allow")] IReadOnlyList<AuthorizedAction>? Allow = null,
    [property: JsonPropertyName("deny")] IReadOnlyList<AuthorizedAction>? Deny = null,
    [property: JsonPropertyName("require_approval")] IReadOnlyList<ApprovalRule>? RequireApproval = null,
    [property: JsonPropertyName("procedure")] IReadOnlyList<ProcedureStep>? Procedure = null,
    [property: JsonPropertyName("triggers")] IReadOnlyList<string>? Triggers = null,
    [property: JsonPropertyName("required_fields")] IReadOnlyList<string>? RequiredFields = null,
    [property: JsonPropertyName("ratified_by")] string? RatifiedBy = null,
    [property: JsonPropertyName("ratified_at")] DateTimeOffset? RatifiedAt = null,
    [property: JsonPropertyName("ratification_reasoning")] string? RatificationReasoning = null,
    [property: JsonPropertyName("expires_at")] DateTimeOffset? ExpiresAt = null,
    [property: JsonPropertyName("supersedes")] string? Supersedes = null)
{
    /// <summary>Canonical <c>path@version</c> handle.</summary>
    public string Handle => $"{Path}@{Version}";

    /// <summary><c>true</c> once the policy has been ratified.</summary>
    public bool IsRatified => RatifiedBy is not null;
}

/// <summary>
/// A proposed change evaluated against change-cost policies
/// (POLICY_V1.md §22.2).
/// </summary>
public sealed record ChangeProposal(
    [property: JsonPropertyName("action")] string Action,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("intent")] string Intent,
    [property: JsonPropertyName("preferred_option")] string PreferredOption,
    [property: JsonPropertyName("alternatives")] IReadOnlyList<string>? Alternatives = null,
    [property: JsonPropertyName("tokens")] IReadOnlyList<string>? Tokens = null,
    [property: JsonPropertyName("attached_fields")] IReadOnlyDictionary<string, string>? AttachedFields = null);

/// <summary>
/// Evidence attached to a <c>Done</c> <see cref="Task"/>.
/// </summary>
public sealed record Proof(
    [property: JsonPropertyName("kind")] ProofKind Kind,
    [property: JsonPropertyName("value")] string Value,
    [property: JsonPropertyName("note")] string? Note = null)
{
    public static Proof Commit(string sha, string? note = null) => new(ProofKind.Commit, sha, note);
    public static Proof File(string path, string? note = null) => new(ProofKind.File, path, note);
    public static Proof Test(string name, string? note = null) => new(ProofKind.Test, name, note);
    public static Proof Text(string value, string? note = null) => new(ProofKind.Text, value, note);
}

/// <summary>
/// A named container of tasks.
/// </summary>
public sealed record Plan(
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("status")] PlanStatus Status,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("created_by")] string CreatedBy,
    [property: JsonPropertyName("description")] string? Description = null,
    [property: JsonPropertyName("archived_at")] DateTimeOffset? ArchivedAt = null);

/// <summary>
/// A unit of work inside a <see cref="Plan"/>. Includes the 0.6.0 task
/// extensions: <see cref="Payload"/>, <see cref="ParentChange"/>,
/// <see cref="OnComplete"/>.
/// </summary>
/// <remarks>
/// Named <c>Task</c> to match every other binding; callers using
/// <c>System.Threading.Tasks.Task</c> alongside this will need a
/// <c>using</c> alias (e.g. <c>using AsgTask = AgentStateGraph.Task;</c>)
/// or a fully-qualified reference.
/// </remarks>
public sealed record Task(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("title")] string Title,
    [property: JsonPropertyName("status")] TaskStatus Status,
    [property: JsonPropertyName("priority")] Priority Priority,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("created_by")] string CreatedBy,
    [property: JsonPropertyName("parent_id")] string? ParentId = null,
    [property: JsonPropertyName("blocked_by")] IReadOnlyList<string>? BlockedBy = null,
    [property: JsonPropertyName("started_at")] DateTimeOffset? StartedAt = null,
    [property: JsonPropertyName("started_by")] string? StartedBy = null,
    [property: JsonPropertyName("completed_at")] DateTimeOffset? CompletedAt = null,
    [property: JsonPropertyName("completed_by")] string? CompletedBy = null,
    [property: JsonPropertyName("proof")] Proof? Proof = null,
    [property: JsonPropertyName("abandoned_at")] DateTimeOffset? AbandonedAt = null,
    [property: JsonPropertyName("abandoned_reason")] string? AbandonedReason = null,
    [property: JsonPropertyName("assigned_to")] string? AssignedTo = null,
    /// <summary>
    /// Opaque JSON payload round-tripped by the task store (POLICY_V1.md
    /// §22.4). Serialized as <see cref="JsonElement"/> so consumers can
    /// inspect the shape without the binding interpreting it.
    /// </summary>
    [property: JsonPropertyName("payload")] JsonElement? Payload = null,
    [property: JsonPropertyName("parent_change")] string? ParentChange = null,
    [property: JsonPropertyName("on_complete")] OnCompleteHook? OnComplete = null);

/// <summary>
/// Options bundle for <see cref="TaskStore.AddTask"/>.
/// </summary>
public sealed record AddTaskOptions(
    string? ParentId = null,
    IReadOnlyList<string>? Blockers = null,
    string? AssignedTo = null);

/// <summary>
/// Lightweight index entry for an epoch (Rust:
/// <c>agentstategraph_core::epoch::EpochEntry</c>).
/// </summary>
public sealed record EpochEntry(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("description")] string Description,
    [property: JsonPropertyName("status")] EpochStatus Status,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("commit_count")] long CommitCount,
    [property: JsonPropertyName("root_intents")] IReadOnlyList<string>? RootIntents = null,
    [property: JsonPropertyName("agents")] IReadOnlyList<string>? Agents = null,
    [property: JsonPropertyName("sealed_at")] DateTimeOffset? SealedAt = null,
    [property: JsonPropertyName("seal_hash")] string? SealHash = null,
    [property: JsonPropertyName("tags")] IReadOnlyList<string>? Tags = null);

/// <summary>
/// Bounded, sealable segment of work (Rust:
/// <c>agentstategraph_core::epoch::Epoch</c>).
/// </summary>
public sealed record Epoch(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("description")] string Description,
    [property: JsonPropertyName("status")] EpochStatus Status,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("root_intents")] IReadOnlyList<string>? RootIntents = null,
    [property: JsonPropertyName("sealed_at")] DateTimeOffset? SealedAt = null,
    [property: JsonPropertyName("seal_summary")] string? SealSummary = null,
    [property: JsonPropertyName("seal_hash")] string? SealHash = null,
    [property: JsonPropertyName("commits")] IReadOnlyList<string>? Commits = null,
    [property: JsonPropertyName("agents")] IReadOnlyList<string>? Agents = null,
    [property: JsonPropertyName("branches")] IReadOnlyList<string>? Branches = null,
    [property: JsonPropertyName("tags")] IReadOnlyList<string>? Tags = null,
    [property: JsonPropertyName("sealed_commits")] IReadOnlyList<string>? SealedCommits = null);

/// <summary>
/// Durable sub-agent session record (Rust:
/// <c>agentstategraph_core::session::Session</c>).
/// </summary>
public sealed record Session(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("agent_id")] string AgentId,
    [property: JsonPropertyName("working_branch")] string WorkingBranch,
    [property: JsonPropertyName("head")] string Head,
    [property: JsonPropertyName("status")] SessionStatus Status,
    [property: JsonPropertyName("created_at")] DateTimeOffset CreatedAt,
    [property: JsonPropertyName("parent_session")] string? ParentSession = null,
    [property: JsonPropertyName("delegated_intent")] string? DelegatedIntent = null,
    [property: JsonPropertyName("report_to")] string? ReportTo = null,
    [property: JsonPropertyName("path_scope")] string? PathScope = null,
    [property: JsonPropertyName("ended_at")] DateTimeOffset? EndedAt = null);
