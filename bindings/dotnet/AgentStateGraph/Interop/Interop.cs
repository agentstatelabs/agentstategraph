using System;
using System.Runtime.InteropServices;

namespace AgentStateGraph.Interop;

/// <summary>
/// 1:1 P/Invoke surface for the <c>agentstategraph_ffi</c> C ABI. Every
/// extern declared in <c>bindings/go/agentstategraph.h</c> has a matching
/// <c>[DllImport]</c> here. Idiomatic wrappers live in §3; this file is
/// intentionally mechanical.
/// </summary>
internal static partial class NativeMethods
{
    private const string Lib = "agentstategraph_ffi";

    /* Repository */

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_new_memory();

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_new_sqlite(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    [DllImport(Lib)]
    internal static extern void agentstategraph_free(IntPtr repo);

    [DllImport(Lib)]
    internal static extern void agentstategraph_free_string(IntPtr s);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_get(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_set(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string jsonValue,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string intentCategory,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string intentDescription);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_delete(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string intentCategory,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string intentDescription);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_branch(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? from);

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_list_branches(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? prefix);

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_delete_branch(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_diff(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refA,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refB);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_merge(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string source,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string target,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string description);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_log(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        uint limit);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_blame(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    /* TaskStore */

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_new(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string prefix,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string agentId);

    [DllImport(Lib)]
    internal static extern void agentstategraph_taskstore_free(IntPtr store);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_create_plan(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string description);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_list_plans(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_list_plans_by_status(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string status);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_get_plan(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_archive_plan(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_delete_plan(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_add_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string title,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string priority,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? parentId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? blockersJson,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? assignedTo);

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_taskstore_add_task_ex(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string title,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string priority,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? parentId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? blockersJson,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? assignedTo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? payloadJson,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? parentChange,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? onCompleteJson);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_list_tasks(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_task_ids(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_get_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_start_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_complete_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string proofKind,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string proofValue,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? proofNote);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_abandon_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string reason);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_set_priority(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string priority);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_set_blockers(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string blockersJson);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_assign_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string agent);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_unassign_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string taskId);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_next_task(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_next_task_for(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string agent,
        byte includeUnassigned);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_taskstore_derived_status(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string plan,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string parentId);

    /* PolicyStore */

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_store_new(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string prefix,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string agentId);

    [DllImport(Lib)]
    internal static extern void agentstategraph_policy_store_free(IntPtr store);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_propose(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string policyJson);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_ratify(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string ratifier,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string reasoning);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_supersede(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string newPolicyJson);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_list(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? prefixOrNull);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_active(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? prefixOrNull);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_get(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_history(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_evaluate(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string situationJson,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string action,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string agentId);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_evaluate_change(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string proposalJson);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_policy_check_tokens(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string tokensJson);

    /* PolicyStore — 0.7.5-beta.1 §5c signing + external evaluator */

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_policy_sign(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? signerKeyId);

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_policy_verify(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path);

    [DllImport(Lib, CharSet = CharSet.Ansi)]
    internal static extern IntPtr agentstategraph_policy_set_external_evaluator(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string configJson);

    /* Migrate */

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_migrate_check(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string target);

    [DllImport(Lib)]
    internal static extern IntPtr agentstategraph_migrate_run(
        IntPtr repo,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string refName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string target,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string mode);
}
