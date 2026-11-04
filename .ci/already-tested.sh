#!/bin/sh
# Succeeds when a job can skip its work because the code was already tested.
#
# That is the case for a push to the default branch whose commit is a merge
# with exactly the tree of the branch it merged. The merged commit's own
# pipeline (its merge request pipeline, or its branch pipeline) already ran
# fmt, clippy and the tests on that tree, and nothing else changed. This holds
# whenever the branch was up to date with the default branch when it merged.
#
# Everything else runs in full: a merge that combined other changes, a direct
# push, a tag, and any scheduled or manually started ("Run pipeline")
# pipeline. If anything here fails, such as a missing parent in a shallow
# clone, the check fails and the job runs in full.
#
# Use it as the first line of a job's script:
#   - if sh .ci/already-tested.sh; then exit 0; fi

[ "${CI_PIPELINE_SOURCE:-}" = push ] || exit 1
[ -n "${CI_COMMIT_BRANCH:-}" ] || exit 1
[ "$CI_COMMIT_BRANCH" = "${CI_DEFAULT_BRANCH:-}" ] || exit 1

merged=$(git rev-parse -q --verify 'HEAD^2^{commit}' 2>/dev/null) || exit 1
tree=$(git rev-parse 'HEAD^{tree}' 2>/dev/null) || exit 1
merged_tree=$(git rev-parse "$merged^{tree}" 2>/dev/null) || exit 1
[ "$tree" = "$merged_tree" ] || exit 1

echo "Skipping: this merge's tree is identical to $merged, which its own pipeline already tested."
exit 0
