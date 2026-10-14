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

Before committing, do the binding review and set `reviewed_core_version` in
`bindings/capabilities.json` by hand (see below). Review the diff, then:

```sh
python3 scripts/check-binding-capabilities.py
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
requires; bump the field only after doing it, and **in the `release-prep`
commit itself**. The release jobs run only when `main`'s HEAD title matches
`^release-prep: v`, so a review pushed as a follow-up commit passes CI and tags
nothing — which is how v1.2.0 and v1.2.2 stalled. To recover, see *If a
release tagged nothing* in [CONTRIBUTING.md](CONTRIBUTING.md).

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
| Rust crates | crates.io — the `publish-crates` job on the tag (below) |

Nothing goes to public npm or PyPI. A 404 on those public registries is
expected, not a failed release.

## Publishing the crates to crates.io

The 15 crates under `crates/` publish to crates.io. `bindings/python` and
`bindings/typescript` are `publish = false`: they ship as a wheel and an npm
package, not as crates.

The `publish-crates` job does this on every release tag, once
`CARGO_REGISTRY_TOKEN` is set. It needs `publish-github`, so it runs only after
the checks, the tests and the GitHub mirror's leak scan have passed, and it runs
the leak scan again before `scripts/publish-crates.sh --publish`. Crates already
on crates.io at the version are skipped, so retrying the job is safe. A tag
pipeline created before the token was set has no `publish-crates` job, so to
publish an existing release run a new pipeline for its tag (see *Re-running a
tag pipeline* below).

`CARGO_REGISTRY_TOKEN` is a masked, protected CI variable (`v*` tags are
protected, so tag pipelines can read it). Make it a crates.io token scoped to
`publish-update` and `agentstategraph*`, with an expiry. `publish-update` cannot
create a crate, so a crate new to the workspace gets its first version
published by hand. crates.io also rate-limits new crate names (a burst of five,
then about one every ten minutes), which a re-run of the script waits out.

By hand, from a detached checkout of the tag, never from a branch:

```sh
git worktree add --detach ../asg-v1.0.0 v1.0.0
cd ../asg-v1.0.0
scripts/publish-crates.sh             # dry run
scripts/publish-crates.sh --publish   # the real upload (uses `cargo login`)
```

Cargo 1.90 and later verifies every crate before uploading any, then uploads
in dependency order. A crates.io version can be yanked but never deleted or
re-uploaded, so anything wrong with a published crate is fixed by the next
patch release.

## Re-running a tag pipeline

A new pipeline on a tag that has already been released is safe: every publish
job skips what is already published, so nothing that is already out is
uploaded again. Use it to finish a release that a job left unpublished, for
example to run `publish-crates` once `CARGO_REGISTRY_TOKEN` exists. Retrying
jobs in the old pipeline is not enough for that, because job rules are
evaluated when a pipeline is created, so a job missing from the old pipeline
stays missing.

Start one from Build → Pipelines → *Run pipeline* with the tag selected, or:

```sh
glab api -X POST "projects/agentstategroup%2Fagentstategraph/pipeline?ref=v1.0.0"
```

| Job | On a re-run |
|---|---|
| `publish-github` | The tag is already on GitHub, so its push is a no-op and GitHub's release workflow does not run again; main is pushed only when that fast-forwards it. Once main has moved past the tag, the leak scan covers the tag's whole history and takes a few minutes. |
| `publish-crates` | Skips crates already on crates.io at the version. |
| `publish-wheel` | Skips a wheel already in the GitLab PyPI registry. |
| `publish-npm` | Skips a version already in the GitLab npm registry. |
| `publish-swift-release` | Skips assets already on the GitHub release with the staged bytes, then runs the SwiftPM verification again. |
| `site-version` | Leaves `releases.json` alone when it already shows this release or a newer one. |

A tag's pipeline runs the `.gitlab-ci.yml` and scripts at the tag's commit, not
main's, so the table holds only for a tag whose commit includes this section.
On v1.3.0 and earlier, `publish-wheel` and `publish-npm` fail on the version
the registry already has, and `publish-swift-release` deletes and re-uploads
its unchanged assets. The wheel and npm jobs are allowed to fail, so such a
pipeline still runs `publish-crates` and `site-version`, and passes with
warnings.

## After the release: the site's version strings

The tag pipeline's `site-version` job records the release in
`agentstatelabs.com/releases.json` (`scripts/publish-site-version.sh`, which
only ever moves a version forward). agentstategraph.dev reads that file at page
load for its hero badge, footer and terminal demo, so the site shows the new
version without a site change or deploy. The job runs 10 minutes after
`publish-github`, so the site never advertises a release GitHub has not built,
and it does nothing if `SITE_RELEASES_TOKEN` is unset.

Two things in `AgentStateGraph-site` are still edited by hand, through an MR
there:

| What | Where | When |
|---|---|---|
| Fallback version in the hero badge, footer and terminal demo | `site/src/pages/index.astro` | After the release. It is what crawlers, no-JS visitors and a failed fetch see. |
| SwiftPM `from: "X.Y.Z"` | `index.astro` + `site/src/content/docs/guides/swift.md` | **Only after the Swift package publishes** |

The SwiftPM pin is a dependency coordinate, not a version claim. Bumping it
before the release exists hands visitors a snippet that fails, so move it only
once `agentstategraph-swift-vX.Y.Z.xcframework.zip` downloads from the GitHub
release.

Leave alone: `bindings/capabilities.json` `reviewed_core_version` (that belongs
to the binding review), and the `0.9.x` mentions in the epochs, tasks and
capabilities docs, which are illustrative examples and factual records rather
than version claims.

**Confirm the live page, not the pipeline.** The site deploys in two hops
(GitLab CI mirrors to GitHub, GitHub Actions builds Pages), so a green pipeline
only means the mirror landed. This is not hypothetical: the site once
advertised `0.9.21` while the real release was `0.9.24`, stale by three
patches, because this step had no home in a checklist.

```sh
curl -s https://agentstatelabs.com/releases.json          # "agentstategraph": "vX.Y.Z"
curl -s https://agentstategraph.dev/ | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | sort | uniq -c  # fallback text
```

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
