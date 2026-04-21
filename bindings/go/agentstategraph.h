/*
 * agentstategraph.h — shared cgo declarations for the Go binding.
 *
 * Every .go file in this package that `import "C"` must include this
 * header in its cgo preamble, because Go cgo scopes declarations
 * per-file.
 */
#ifndef AGENTSTATEGRAPH_H
#define AGENTSTATEGRAPH_H

#include <stdint.h>

typedef void* SgRepo;
typedef void* SgTaskStore;
typedef void* SgPolicyStore;

/* Repository */
extern SgRepo agentstategraph_new_memory();
extern SgRepo agentstategraph_new_sqlite(const char* path);
extern void agentstategraph_free(SgRepo repo);
extern void agentstategraph_free_string(char* s);

extern char* agentstategraph_get(SgRepo repo, const char* ref_name, const char* path);
extern char* agentstategraph_set(SgRepo repo, const char* ref_name, const char* path,
    const char* json_value, const char* intent_category, const char* intent_description);
extern char* agentstategraph_delete(SgRepo repo, const char* ref_name, const char* path,
    const char* intent_category, const char* intent_description);
extern char* agentstategraph_branch(SgRepo repo, const char* name, const char* from);
extern char* agentstategraph_diff(SgRepo repo, const char* ref_a, const char* ref_b);
extern char* agentstategraph_merge(SgRepo repo, const char* source, const char* target,
    const char* description);
extern char* agentstategraph_log(SgRepo repo, const char* ref_name, unsigned int limit);
extern char* agentstategraph_blame(SgRepo repo, const char* ref_name, const char* path);

/* TaskStore */
extern SgTaskStore agentstategraph_taskstore_new(SgRepo repo, const char* prefix, const char* agent_id);
extern void agentstategraph_taskstore_free(SgTaskStore store);
extern char* agentstategraph_taskstore_create_plan(SgTaskStore store, const char* ref_name,
    const char* name, const char* description);
extern char* agentstategraph_taskstore_list_plans(SgTaskStore store, const char* ref_name);
extern char* agentstategraph_taskstore_list_plans_by_status(SgTaskStore store, const char* ref_name,
    const char* status);
extern char* agentstategraph_taskstore_get_plan(SgTaskStore store, const char* ref_name, const char* name);
extern char* agentstategraph_taskstore_archive_plan(SgTaskStore store, const char* ref_name, const char* name);
extern char* agentstategraph_taskstore_delete_plan(SgTaskStore store, const char* ref_name, const char* name);
extern char* agentstategraph_taskstore_add_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* title, const char* priority, const char* parent_id, const char* blockers_json,
    const char* assigned_to);
extern char* agentstategraph_taskstore_list_tasks(SgTaskStore store, const char* ref_name, const char* plan);
extern char* agentstategraph_taskstore_task_ids(SgTaskStore store, const char* ref_name, const char* plan);
extern char* agentstategraph_taskstore_get_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id);
extern char* agentstategraph_taskstore_start_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id);
extern char* agentstategraph_taskstore_complete_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id, const char* proof_kind, const char* proof_value, const char* proof_note);
extern char* agentstategraph_taskstore_abandon_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id, const char* reason);
extern char* agentstategraph_taskstore_set_priority(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id, const char* priority);
extern char* agentstategraph_taskstore_set_blockers(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id, const char* blockers_json);
extern char* agentstategraph_taskstore_assign_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id, const char* agent);
extern char* agentstategraph_taskstore_unassign_task(SgTaskStore store, const char* ref_name, const char* plan,
    const char* task_id);
extern char* agentstategraph_taskstore_next_task(SgTaskStore store, const char* ref_name, const char* plan);
extern char* agentstategraph_taskstore_next_task_for(SgTaskStore store, const char* ref_name, const char* plan,
    const char* agent, uint8_t include_unassigned);
extern char* agentstategraph_taskstore_derived_status(SgTaskStore store, const char* ref_name, const char* plan,
    const char* parent_id);

/* PolicyStore */
extern SgPolicyStore agentstategraph_policy_store_new(SgRepo repo, const char* prefix, const char* agent_id);
extern void agentstategraph_policy_store_free(SgPolicyStore store);
extern char* agentstategraph_policy_propose(SgPolicyStore store, const char* ref_name, const char* policy_json);
extern char* agentstategraph_policy_ratify(SgPolicyStore store, const char* ref_name, const char* path,
    const char* ratifier, const char* reasoning);
extern char* agentstategraph_policy_supersede(SgPolicyStore store, const char* ref_name, const char* path,
    const char* new_policy_json);
extern char* agentstategraph_policy_list(SgPolicyStore store, const char* ref_name, const char* prefix_or_null);
extern char* agentstategraph_policy_active(SgPolicyStore store, const char* ref_name, const char* prefix_or_null);
extern char* agentstategraph_policy_get(SgPolicyStore store, const char* ref_name, const char* path);
extern char* agentstategraph_policy_history(SgPolicyStore store, const char* ref_name, const char* path);
extern char* agentstategraph_policy_evaluate(SgPolicyStore store, const char* ref_name, const char* situation_json,
    const char* action, const char* agent_id);
extern char* agentstategraph_policy_evaluate_change(SgPolicyStore store, const char* ref_name,
    const char* proposal_json);
extern char* agentstategraph_policy_check_tokens(SgPolicyStore store, const char* ref_name, const char* tokens_json);

/* Migrate */
extern char* agentstategraph_migrate_check(SgRepo repo, const char* ref_name, const char* target);
extern char* agentstategraph_migrate_run(SgRepo repo, const char* ref_name, const char* target, const char* mode);

#endif /* AGENTSTATEGRAPH_H */
