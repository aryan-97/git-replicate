# git-replicate

[![Version](https://img.shields.io/badge/version-1.2.0-blue.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-Apache--2.0-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/tests-37%20passing-brightgreen.svg)](tests/cli.rs)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](Cargo.toml)

A lightweight, cross-platform Rust CLI that replicates selected Git branch pushes from a **primary** remote to one or more **mirror** remotes (GitHub, GitLab, Bitbucket, Gitea, or any SSH/HTTPS Git server).

Unlike `git push --mirror`, `git-replicate` replicates **only explicitly allowlisted branches**—never tags, notes, pull-request refs, deletions, or force pushes.

---

## Features

- **Primary-First Ordering** — Pushes to your primary remote first and, only if that succeeds, pushes the exact same commit (`OID:refs/heads/BRANCH`) to every configured mirror.
- **Branch Allowlist** — Only branches matching patterns in `.gitreplicate` (e.g. `main`, `release/*`, `hotfix/**`) can be replicated; everything else is denied before any network write.
- **Safe Recovery (`retry`)** — Records per-remote attempt state under `.git/gitreplicate/state.json`. If a mirror fails after the primary succeeds, `git-replicate retry` verifies the primary hasn't moved and retries only the failed mirrors without `--force`.
- **Zero-Trust URL & Credential Safety** — Delegates authentication to your existing Git/SSH/credential helpers. Rejects insecure transports (`http://`, `file://`, `git://`, `ext::`) and embedded credentials, blocks unexpected `pushurl` / `insteadOf` drift, and redacts secrets from all output.
- **Automation-Ready (`--uiless`)** — Emits structured JSON Lines (JSONL) on `stdout` with deterministic exit codes and zero interactive prompts.

---

## Installation

Install directly from GitHub using Cargo:

```sh
cargo install --git https://github.com/aryan-97/git-replicate
```

Or build from a local checkout:

```sh
cargo install --path .
```

Ensure Cargo's bin directory (`~/.cargo/bin`) is on your `PATH` as `git-replicate`.

---

## Quick Start

```sh
# 1. Initialize .gitreplicate (or sync missing mirror remotes in a fresh clone)
git-replicate setup --allow-branch main

# 2. Add one or more mirror remotes
git-replicate mirror add gitlab --url git@gitlab.com:team/project.git
git-replicate mirror add work --remote existing-work-remote

# 3. Verify local configuration and remote connectivity
git-replicate doctor --check-remote

# 4. Push current branch (primary first, then mirrors)
git-replicate push

# 5. Repair any failed mirrors after fixing access/network issues
git-replicate retry

# 6. Inspect local state and remote refs
git-replicate status --remote
```

---

## Commands

Run `git-replicate --help` or `git-replicate <COMMAND> --help` for full manual pages.

| Command | Description |
| :--- | :--- |
| `setup [--primary NAME] [--allow-branch PATTERN] [--yes]` | Idempotent setup. Creates `.gitreplicate` on first run; in a new clone, plans and creates any missing local mirror remotes. |
| `mirror add NAME (--url URL \| --remote REMOTE)` | Add a mirror by URL (creates `gitreplicate-NAME`) or adopt an existing Git remote. |
| `mirror list` / `mirror remove NAME` | List configured mirrors or remove a mirror entry (`--remove-managed-remote` also deletes the tool-created Git remote). |
| `allowlist add\|remove\|list\|check` | Manage branch patterns allowed to be pushed or mirrored (`*` matches within a path segment, `**` matches across `/`). |
| `push [--branch BRANCH] [--dry-run]` | Push a branch to the primary remote first, then each mirror. `--dry-run` prints the local plan without network access. |
| `retry [--branch BRANCH]` | Retry failed mirrors from the last partial push after confirming the primary still points to the recorded commit. |
| `status [--remote]` | Show remote config, last push/hook attempts, and optional read-only `git ls-remote` comparisons. |
| `doctor [--check-remote]` | Diagnose Git setup, URL drift, hooks, trust boundaries, and SSH/HTTPS authentication. |
| `hook install\|uninstall\|status pre-push` | Manage the optional tool-owned `pre-push` hook (`--strict` blocks the primary push if a mirror fails). |

---

## Wrapper vs. `pre-push` Hook

Git provides a `pre-push` hook but **no `post-push` hook**:

1. **`git-replicate push` (Recommended):** Pushes the primary first and updates mirrors only after the primary succeeds.
2. **`git-replicate hook install pre-push` (Opt-in):** Mirrors eligible branch updates during an ordinary `git push` *before* the primary push happens. Because Git runs `pre-push` prior to updating the primary, a rejected primary push can leave mirrors ahead. By default the hook is best-effort (warns on mirror failure and lets the primary push continue); pass `--strict` to block the primary push on mirror failure.

> **Note:** An ordinary `git push` without the hook installed is **not** mirrored. Hooks are local to a clone and bypassed by `git push --no-verify`.

---

## Configuration (`.gitreplicate`)

Stored at the worktree root and safe to commit (contains no credentials):

```toml
version = 1
branches = ["main", "release/*"]

[primary]
remote = "origin"
url = "git@github.com:team/project.git"

[[mirrors]]
name = "gitlab"
remote = "gitreplicate-gitlab"
url = "git@gitlab.com:team/project.git"
```

- Only `ssh://`, `git@host:path`, and `https://` URLs without embedded credentials are accepted.
- If a local Git remote resolves to a different push URL than `.gitreplicate` (via `remote.<name>.pushurl` or `url.*.insteadOf`), writes are refused until resolved.

---

## Exit Codes & `--uiless` JSONL

| Code | Meaning |
| :---: | :--- |
| `0` | All selected writes succeeded (or local `--dry-run` / no-op succeeded) |
| `2` | Usage, configuration, allowlist, or repository error (nothing pushed) |
| `3` | Primary push failed or outcome unknown (mirrors were not attempted) |
| `4` | Primary succeeded, but one or more mirrors failed/timed out or state could not be saved |
| `5` | Hook installation or conflict error |
| `6` | Read-only remote check failed (`doctor --check-remote`, `status --remote`, `retry` pre-check) |

When `--uiless` (or `GIT_REPLICATE_UILESS=1`) is passed, every line on `stdout` is a JSON object (`schema: 1`) with keys:
`schema`, `event` (`plan` | `remote_result` | `check` | `warning` | `result`), `operation`, `remote`, `branch`, `oid`, `status`, `code`, `message`, `primary_status`, `mirror_results`, and `partial`. The final line is always a `result` event whose `code` matches the process exit code.

---

## Test Suite & Verification

Run the full verification suite locally:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

| Suite | Tests | Status | Coverage Area |
| :--- | :---: | :---: | :--- |
| Unit (`src/config.rs`, `src/util.rs`) | 6 | Passing | Branch glob matching (`*`, `**`), pattern validation, URL validation, secret redaction, destination keys, ISO timestamps |
| CLI & Help Snapshots (`tests/cli.rs`) | 3 | Passing | Man-page `--help` snapshots, nested subcommand help, `--uiless` usage errors, out-of-repo & bare/submodule handling |
| Setup & Mirror Management (`tests/cli.rs`) | 5 | Passing | Idempotent `setup`, fresh-clone remote creation, ambiguous primary handling, URL & duplicate-destination rejection |
| Replication & Recovery (`tests/cli.rs`) | 9 | Passing | Primary-first push, JSONL schema v1, `--dry-run`, primary failure isolation, partial failure + `retry`, non-fast-forward refusal, URL drift, worktrees, stale/live locks |
| Branch Allowlist (`tests/cli.rs`) | 4 | Passing | First-run allowlist seeding, `allowlist add/remove/list/check`, wrapper & hook enforcement |
| Pre-Push Hook & Security (`tests/cli.rs`) | 10 | Passing | Hook install/uninstall safety, `core.hooksPath`, best-effort vs `--strict`, tag/deletion skipping, recursion guard, credential redaction |
| **Total** | **37** | **All Passing** | Offline integration harness using local bare repositories and mock SSH transport |

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Unless required by applicable law or agreed to in writing, software distributed under the License is distributed on an `"AS IS"` basis, without warranties or conditions of any kind.

