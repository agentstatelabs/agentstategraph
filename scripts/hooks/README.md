# Leak-scan publish gate

Keeps internal/private content out of the **public GitHub mirror**. Because
GitLab → GitHub push-mirroring is automatic, the only safe enforcement point
is **in front of GitLab**: content that never enters GitLab can never be
mirrored.

## Components

| File | Role |
|------|------|
| `../leak-scan.sh` | The scanner. Two tiers: **BLOCK** (fails) and **WARN** (reports). Scans working tree, a commit range, or full history. |
| `pre-push` | Client-side hook — blocks `git push` if outgoing commits contain BLOCK content. First line of defence. |
| `pre-receive` | GitLab server-side hook — the authoritative gate; GitLab rejects the push before storing/mirroring. |
| `install.sh` | Installs the client `pre-push` hook into the current clone. |

## What it flags

**BLOCK** (push refused): generic — local home paths (`/Users/…`, `/home/…`)
and secret material (private keys, AWS/GitHub/Slack tokens) — plus your
org-specific terms (internal hostnames, IP ranges, private email domains,
internal repo/org names). The org-specific terms are **not** listed in this
public file; they live in the gitignored `.leakscan.local`, distributed via
the private enterprise repo, so the public copy never enumerates your secrets.

**WARN** (reported, allowed — these may appear in public marketing): internal
project/codenames, also configured in `.leakscan.local`.

**Never flagged:** `asd`/`ctx`-generated files, `.md` files, `.vscode/`, and
this tool's own files.

## Install

```sh
# client-side, per clone (dev machines)
scripts/hooks/install.sh

# server-side, on the self-managed GitLab (authoritative). As GitLab admin,
# place pre-receive in the repo's custom_hooks/ (or the global
# hooks/pre-receive.d/) and vendor leak-scan.sh next to it or set LEAKSCAN_BIN.
```

## Manual runs

```sh
scripts/leak-scan.sh --tree           # scan the working tree
scripts/leak-scan.sh --range A..B      # scan a commit range
scripts/leak-scan.sh --history         # audit all history
```

Exit `0` clean, `1` blocked. Findings are logged to `.git/leak-scan/last-scan.log`.
Email notification is stubbed: set `LEAKSCAN_NOTIFY` to an executable that
receives `<logfile> <block-count> <warn-count>` (wire up when email is ready).

## Reusing in another repo

1. Copy `scripts/leak-scan.sh` and `scripts/hooks/` into the repo.
2. Run `scripts/hooks/install.sh` (client) and install `pre-receive` on GitLab.
3. Tune per-repo patterns/allowlist in a repo-root `.leakscan.local`:

   ```bash
   # .leakscan.local  (sourced by leak-scan.sh)
   BLOCK+=('mysecretproject\.internal')
   WARN+=('\bAnotherCodename\b')
   ALLOW_PATHS+=(':!vendor/*')
   ```

The org-wide BLOCK/WARN defaults live in `leak-scan.sh`; `.leakscan.local`
only appends.
