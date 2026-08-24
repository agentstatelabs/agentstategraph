# Cutting an AgentStateGraph release

Unlike CTXone and AgentStateDeveloper — whose releases build locally with
Docker, cross-rs and a sibling tap clone — AgentStateGraph's release is driven
entirely by CI. There are no local prerequisites beyond a clean tree.

## The one thing to know first

**Pushing `main` is the release.** There is no separate manual tag step.

`create-release-tag` fires on any commit to `main` whose title matches
`^release-prep: v`, reads the version out of `Cargo.toml`, and creates and
pushes the tag itself. That tag then runs `version-guard` and the publish
stages.

So review the diff *before* you push, not before you tag — by then it has
already gone out.

## Cutting a release

From a clean tree on `main`, level with origin:

```sh
scripts/release.sh 1.0.0          # bare version, no leading "v"
```

The script bumps only — it does not commit, tag or push. It updates the
workspace version and internal path deps in `Cargo.toml`, the TypeScript
binding's `package.json`, refreshes `Cargo.lock`, and stamps the changelog.

Review the diff, then:

```sh
git commit -am 'release-prep: v1.0.0'
git push origin main
```

CI then mirrors the commit to GitHub, dispatches the macOS build, stages the
xcframework, generates `Package.swift`, and pushes the tag.

## Two things that will bite you

**`release.sh` does not touch `bindings/capabilities.json`.** The
`binding-contract` job fails every release until `reviewed_core_version` there
matches the new workspace version. That gate is deliberate — it forces an
explicit review of all eight binding surfaces. See
[BINDING_RELEASE_POLICY.md](docs/BINDING_RELEASE_POLICY.md) for what the review
requires; bump the field only after doing it.

**Do not hand-edit `Package.swift` or `bindings/swift/release.json`.**
`prepare-swift-release` stages the xcframework and pushes a checksum-pinned
`Package.swift` commit on top of your prep commit; `create-release-tag` then
tags `FETCH_HEAD` rather than the prep commit so it picks that up. The Swift
checksum cannot be known before the artifact is built, which is why
`version-guard` deliberately excludes Swift metadata from its checks.

## What ships where

| Target | Where |
|---|---|
| GitHub release + Swift xcframework | `agentstatelabs/agentstategraph` (public) |
| npm, PyPI, generic swift package | GitLab package registry (internal) |

There is **no `cargo publish`** — nothing goes to crates.io, and nothing goes
to public npm or PyPI. A 404 on those public registries is expected, not a
failed release.

## After the release: bump the site strings

`AgentStateGraph-site` hardcodes the version in three places in
`site/src/pages/index.astro`, plus a SwiftPM pin in two:

| What | Where | When |
|---|---|---|
| Hero eyebrow | `index.astro` | Immediately — it is a version claim |
| Footer line | `index.astro` | Immediately |
| Terminal animation output | `index.astro` | Immediately |
| SwiftPM `from: "X.Y.Z"` | `index.astro` + `guides/swift.md` | **Only after the package publishes** |

The SwiftPM pin is a dependency coordinate, not a version claim. Bumping it
before the release exists hands visitors a snippet that fails, so it moves
*after* the tag pipeline completes — separately from the other three.

Leave alone: `bindings/capabilities.json` `reviewed_core_version` (that belongs
to the binding review), and the `0.9.x` mentions in the epochs, tasks and
capabilities docs, which are illustrative examples and factual records rather
than version claims.

The site deploys in two hops (GitLab CI mirrors to GitHub, GitHub Actions
builds Pages), so **confirm the live page, not the pipeline** — a green
pipeline only means the mirror landed. This is not hypothetical: the site
advertised `0.9.21` while the real release was `0.9.24`, stale by three
patches, because this step had no home in a checklist.

## Verifying a release actually landed

Check the published artifacts, never the job status:

```sh
curl -s https://api.github.com/repos/agentstatelabs/agentstategraph/releases/tags/v1.0.0 | jq '.tag_name, (.assets|length)'
curl -s https://raw.githubusercontent.com/agentstatelabs/agentstategraph/v1.0.0/Package.swift | grep nativeVersion
glab api "projects/agentstategroup%2Fagentstategraph/packages?order_by=created_at&sort=desc"
```

## See also

- [CONTRIBUTING.md](CONTRIBUTING.md) — the GitLab-canonical / GitHub-mirror
  model, and why release commits bypass merge requests
- [docs/BINDING_RELEASE_POLICY.md](docs/BINDING_RELEASE_POLICY.md) — the
  binding review gate and the current audit state
