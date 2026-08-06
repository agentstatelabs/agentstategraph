# .asd/conclusions/

Compact JSONL home for ASD's six conclusion classes: decisions,
classifications, mappings, hazards, recipes, followups.

Files here travel with the git repo. They are written by
`asd conclusions export` (pre-commit) and read back by
`asd conclusions import` (post-merge / post-checkout).

Target size: kilobytes per project, not megabytes. The big derived
cache (call graph, effects-rev, symbol blobs) lives at `.asd/cache/`
which is gitignored.
