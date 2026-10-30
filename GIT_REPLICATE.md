# Git Replicate: implementation specification

## Goal and non-goals

Build `git-replicate`, a small cross-platform Rust CLI that copies selected Git branch pushes from one primary remote to configured mirror remotes (GitHub, GitLab, Bitbucket, Gitea, or any compatible Git server). No provider API, accounts, GUI, daemon, hosting provisioning, or automatic conflict resolution is required. A "mirror" here means matching selected Git refs, **not** a full Git `--mirror` push of every ref. Default scope: the current local branch only; no tags, notes, pull-request refs, deletions, or force pushes.

The product must work in ordinary repositories regardless of hosting provider, be useful in scripts via `--uiless`, never silently change a remote's history, and distinguish "primary succeeded, mirror failed" from total failure. Do not claim atomicity across remotes.

## Critical Git constraint and product decision

Git offers `pre-push` and `post-commit` hooks, but **no `post-push` hook**. `pre-push` runs before the primary push; `post-commit` runs before any later primary push. Neither can ensure "primary succeeds, then mirrors update." Do not use background jobs or sleeps to pretend otherwise.

1. **Recommended/default:** `git-replicate push` pushes the primary first and, only on success, pushes the exact same commit to each mirror. Use this when ordering matters.
2. **Opt-in hook:** `hook install pre-push` mirrors eligible refs before an ordinary `git push` to the primary. It cannot guarantee that the later primary push succeeds. Default hook policy is best-effort: report mirror failures but allow the primary push to continue. `--strict` may block the primary push on mirror failure, but a mirror may already have changed. Present this warning at installation.
3. **Post-commit hook:** defer remote-pushing support until a later, explicitly opt-in phase. A commit is local work, not consent to publish it. If implemented, never enable by default: it may publish unreviewed commits and can never undo a completed commit on error. A safe first post-commit feature is to queue a reminder/local state only, without network access.

An ordinary `git push` without the installed hook is **not** mirrored. Hooks are local to a clone, are not shared by cloning the repository, and may be bypassed by Git configuration or `--no-verify`. Document this prominently.

## User journeys

```text
git-replicate setup
git-replicate mirror add gitlab --url git@gitlab.com:team/project.git
git-replicate mirror add work --remote existing-work-remote
git-replicate setup                    # safe to rerun; reports already configured
git-replicate doctor --check-remote
git-replicate push
git-replicate status --remote
git-replicate hook install pre-push       # optional, with warning
```

On the first run, `setup` creates `.gitreplicate`; in a new clone, the tracked config exists but local mirror entries in `.git/config` may not, so the **same command** offers to add only the missing entries. When everything matches, print "Already set up; nothing to do" and return success without changing files or contacting a server. `setup` configures local remotes; it does **not** copy commits, branches, or tags. In CI, install the binary, configure credentials outside this tool, run `setup --yes` only after trusting the checked-out config, and use `git-replicate --uiless push`.

## CLI contract (v1)

Use `clap` with help and examples. Root flags `--uiless`, `--verbose` (redacted diagnostics), and `--no-color`; respect `NO_COLOR`, `CLICOLOR`, and non-TTY output. No command should ask for input unless it is explicitly interactive. `git-replicate --help` must show a detailed, man-page-style terminal guide with NAME, SYNOPSIS, DESCRIPTION, COMMANDS, OPTIONS, EXAMPLES, EXIT STATUS, CONFIGURATION FILE, and SECURITY sections. `git-replicate <command> --help` (including nested commands such as `mirror add --help`) shows command-specific usage, flags, examples, and safety notes. Help works outside a Git repository, never changes files or contacts a remote, exits `0`, and does not require an interactive pager.

| Command | Behavior |
| --- | --- |
| `setup [--primary NAME] [--yes]` | Idempotent entry point. If config is absent, select an existing primary remote (prefer `origin`, otherwise the sole remote); prompt for a choice if ambiguous in interactive mode, or require `--primary` in automation. Create `.gitreplicate` with the primary's validated push URL; offer to configure mirrors interactively, or use `mirror add` later. With existing config, compare expected remotes with `.git/config`, show a plan, and add only missing mirror remotes after approval (`--yes` in automation). If no changes are needed, return success with a friendly no-op message/event. Never overwrite an existing config or remote, install a hook, or contact the network. |
| `mirror add NAME (--url URL \| --remote REMOTE)` | Validate transport and uniqueness. `--url` creates local remote `gitreplicate-NAME`; `--remote` adopts an existing remote. Record its expected push URL in config. Refuse remote name collisions and primary-as-mirror. Roll back only newly created resources on a partial setup error. |
| `mirror list`, `mirror remove NAME` | List config; remove config entry only. Leave Git remote in place by default; a separate explicit `--remove-managed-remote` may remove only a remote created and still owned by this tool, after confirming its current URL. |
| `push [--branch BRANCH] [--dry-run]` | Default current branch; explicit branch must be a local branch. Primary-first sequential branch push; then each mirror. `--dry-run` is a **local plan only**, with no network or remote mutation. Never force or delete. |
| `retry [--branch BRANCH]` | Retry failed mirrors for the recorded partial push only after checking the primary's remote branch still equals the recorded commit. Otherwise ask for a fresh push. |
| `status [--remote]` | Local config, last attempts and hook state; optional read-only `ls-remote` checks of actual remote refs. Do not call a network service without `--remote`. |
| `doctor [--check-remote]` | Explain missing Git/config, invalid remotes, hooks, URL drift and SSH/HTTPS authentication remedies. Remote checks are opt-in, bounded by a timeout, and read-only. |
| `hook install pre-push [--strict]`, `hook uninstall pre-push`, `hook status` | Install/remove only an exact tool-owned hook. Refuse to overwrite an existing hook. Provide instructions for manual delegation from an existing hook. |
| `hook run pre-push "$@"` | Internal documented entry point for manual delegation; read Git's ref updates from stdin and honor the primary remote argument. |
| `--help`, `help [COMMAND]`, `--version` | Full manual-style help, command-specific help, and version metadata. Unknown commands/options and malformed values give actionable errors and point to relevant help. |

For `push`, do not offer arbitrary raw Git arguments in v1. Users with advanced refspecs can use Git directly; supporting them safely requires a separate explicit design. No implicit `git add`, `commit`, `fetch`, `pull`, `push --all`, `push --tags`, `--force`, `--mirror`, or remote creation.

## Configuration and state

`.gitreplicate` is a UTF-8 TOML file in the Git **worktree root**, not the invocation directory. It may be committed and reviewed. Example:

```toml
version = 1

[primary]
remote = "origin"
url = "git@github.com:team/project.git"

[[mirrors]]
name = "gitlab"
remote = "gitreplicate-gitlab"
url = "git@gitlab.com:team/project.git"

[[mirrors]]
name = "work"
remote = "existing-work-remote"
url = "ssh://git@example.org/team/project.git"
```

- `url` is the expected *push* destination for both primary and mirrors, not a credential store. Require exactly one push destination per remote. Compare configured URL to the locally resolved push URL before network writes; refuse mismatches (including `pushurl` overrides or URL rewrite changes), and explain how to fix them. Do not auto-"repair" mismatches. Validate `version`; fail on unknown versions and unknown security-relevant keys rather than ignoring a policy the user expected to apply.
- Allow SSH URLs (`git@host:path`, `ssh://...`) and HTTPS URLs. Reject embedded passwords/tokens, `http://`, `file://`, local paths, `ext::`, `git://`, URL fragments and unsupported helpers in v1. Never put credentials in the file, CLI arguments, logs, or state. Redact URL userinfo in every diagnostic, including failed Git stderr.
- `.gitreplicate` is user-controlled input, not executable code. Validate remote identifiers (`[A-Za-z][A-Za-z0-9_-]*`), length, uniqueness, and URL syntax; constrain file size and mirror count. Honor Git's own ref validation for branches.
- Store attempt records and a lock under the resolved Git **common directory** (via `git rev-parse --git-common-dir`), e.g. `gitreplicate/state.json` and `gitreplicate/lock`, not in tracked config. This keeps worktrees coordinated. Key last attempts by branch; distinguish wrapper attempts from hook attempts (a hook must not record primary success). Record branch, snapshot commit OID, primary outcome, per-mirror outcome, timestamp and a schema version; use atomic replacement and restrictive file permissions. Never store secrets. Handle absent/corrupt/stale state with a clear recovery path; a stale lock must require a safe liveness/age check or explicit user action, not blind deletion.
- Resolve paths through Git rather than assuming `.git` is a directory (worktrees and submodules use `.git` files). Refuse bare repositories and submodule worktrees in v1 with a specific explanation; allow commands from any subdirectory of an ordinary worktree. Reject symlinked or unexpected config/hook/state targets when writing, to avoid writing outside the repository.
- `setup` must report "not a Git repository" outside a worktree, explain how to add a Git primary remote if none exists, and refuse invalid or mismatched existing config/remotes instead of treating them as an empty setup. In `--uiless` mode, never prompt: require `--primary` for ambiguous first-run selection and `--yes` before creating local mirror remotes from existing config. A first-run `setup --primary origin --yes` may create a primary-only config; add mirrors via `mirror add`.

## Push algorithm and failure semantics

1. Find the worktree and load/validate config. Check local branch and commit exist (handle unborn HEAD and detached HEAD). Validate primary and mirror destinations before writes; a changed URL, duplicate destination, unsupported transport, or missing remote blocks the operation. Require all configured remotes to be present; tell new clones to run `setup`.
2. Acquire the repository lock. Snapshot the branch's full ref name and OID once. Use argument-array process execution, never a shell. Push `OID:refs/heads/BRANCH` to the primary, using `git push --porcelain` and no `--force`; capture Git's exit status and per-ref result. Set `GIT_REPLICATE_SKIP_HOOK=1` for child Git pushes so an installed pre-push hook cannot mirror before the wrapper's primary push or deadlock on its lock. Do not automatically set upstream or push tags.
3. On primary failure, stop: no mirror attempts. On primary success, persist partial state **before** attempting mirrors; if state persistence fails, stop and report an uncertain/partial operation rather than proceeding untracked. Push the *same OID and ref* to each mirror sequentially. Attempt remaining mirrors even after one fails. Non-fast-forward, policy, auth, rate-limit, and connection errors remain failures; do not "fix" by force-pushing or pulling.
4. Persist each outcome so interruption after primary success is recoverable. `retry` performs a read-only `ls-remote` check that primary still points at the saved OID, then retries only unsuccessful mirrors. If the primary moved or its state is uncertain, refuse retry and direct the user to inspect or run a new push. Repeated successful pushes must be harmless.
5. Recheck local branch OID after the sequence and warn if another process moved it. All remote writes still target the snapshotted OID. Network operations may partially succeed despite timeouts/disconnects: report **unknown** where Git cannot establish the outcome; use `status --remote` before retrying. Do not assert a distributed transaction.

Exit codes: `0` all selected writes succeeded (or a local dry-run plan succeeded); `2` usage/config/repository error; `3` primary push failed or its outcome is unknown (including auth/transport failures); `4` primary succeeded but a mirror failed, is unknown, or outcome could not be recorded; `5` hook installation/conflict error; `6` read-only remote check failed (`doctor`/`status`). Prefer stable documented codes over treating Git's stderr as an API. A best-effort hook reports failures but returns `0`; strict hook returns nonzero and explains that mirrors may already have changed.

## Hook mechanics

- `pre-push` receives remote name and URL as arguments and lines `local_ref local_oid remote_ref remote_oid` on stdin. Run only when invoked for the configured primary **name and verified URL**. Skip tag refs, delete requests (all-zero local OID), non-branch refs (both local and destination), and non-primary pushes with a concise notice. For eligible updates push `local_oid:remote_ref` to each mirror: an explicitly renamed destination branch must stay renamed. Use Git-supplied local OIDs rather than guessing HEAD; process multiple branch lines deterministically. Do not push when no eligible refs remain.
- Hooks are not a substitute for `git-replicate push`. If the primary later rejects a push, mirrors may be ahead. In best-effort mode, warn and allow Git to continue; strict mode fails the hook when a mirror fails, but cannot undo successful mirror pushes. No attempt to push to the primary inside the hook.
- Install in Git's effective hook directory only if it is safe and empty at that event. Respect `core.hooksPath` (including relative paths); never overwrite, prepend, or append to an existing hook, and never change `core.hooksPath` globally. Record a versioned ownership marker. On uninstall, remove only a byte-for-byte recognized tool-owned hook; otherwise refuse. If a custom hook exists, show a manual `git-replicate hook run pre-push "$@"` delegation example and how stdin forwarding works.
- The hook must locate a working binary or fail with a helpful setup message; avoid hardcoded paths that silently break after an upgrade. Honor `GIT_REPLICATE_SKIP_HOOK=1` before locking, and guard other recursion/re-entry; the wrapper sets that variable only on its child Git pushes. Hook output is short on human terminals and machine-readable when `GIT_REPLICATE_UILESS=1` or the delegated invocation uses `--uiless`. An ordinary parent `git push` may still print its own non-JSON output: automation requiring clean JSONL must invoke the wrapper directly. Never prompt in a hook.

## CLI presentation and automation

Human mode: use `anstyle`/`owo-colors` and `comfy-table` (or comparable maintained Rust crates) for restrained Rich-like headings, aligned remote/status tables, and success/warning/error colors. Include plain labels/icons that remain meaningful without color; avoid animated output in non-TTY mode. No color unless stdout is a TTY and color is enabled. Show progress per remote, not a spinner that hides failure details.

`--uiless` outputs one UTF-8 JSON object per line on stdout (JSONL), including failures; stderr is reserved for catastrophic serialization/startup errors. Schema v1 event fields: `schema`, `event`, `operation`, `remote`, `branch`, `oid`, `status`, `code`, `message` (nullable where irrelevant). Events include `plan`, `remote_result`, `warning`, and final `result`; final event contains `primary_status`, `mirror_results`, and `partial` (nullable for non-push commands). An already-complete `setup` emits a final success `result` with `status: "unchanged"`. Redact secrets from all fields. No prompts, ANSI codes, progress spinners, or tables. Exit codes remain authoritative. Document schema and add snapshot tests. In human mode, print errors to stderr. Explicit exception: `--help` and `--version` print conventional plain text even alongside `--uiless`, so help is readable and standard CLI tooling can consume it.

## Security and authentication

- The tool must never request, save, or display passwords, SSH private keys, PATs, or provider tokens. Delegate authentication to Git/SSH/credential helpers already installed by the user. For HTTPS, explain credential-manager/token setup; for SSH, explain key presence, host-key trust, agent and repository write permission. Do not auto-accept unknown host keys or call `ssh-keyscan` as a trust shortcut.
- For unattended pushes set `GIT_TERMINAL_PROMPT=0`; make SSH noninteractive using a documented BatchMode approach that does not silently replace the user's `GIT_SSH_COMMAND` or SSH configuration. If a user-supplied transport command prevents safe noninteractive operation, fail fast with instructions. Use bounded Git subprocess timeouts for diagnosis and network operations; kill/reap hung children while marking remote outcome unknown, not failed with certainty.
- Config from another repository or branch is untrusted even when versioned. `setup` requires review or `--yes` before creating local remotes from it; never execute scripts from config. Git's own global config, credential helpers, URL rewrites, and SSH configuration can run commands or redirect traffic; display the effective destinations and explain this trust boundary in `doctor`. Check actual destination before write. Avoid passing secret-bearing URLs to command lines; use validated remote names.
- Redact credentials and potentially sensitive query parameters from Git stdout/stderr, logs, JSON and panic paths. Do not emit environment variables or verbose process invocation dumps. Ensure lock/state/hook writes are scoped to the validated Git directory. Hooks are executable code; install only on explicit request.
- No force push or remote deletion in v1, including after divergence. Keep default behavior conservative if a server rejects branch updates, requires signing, blocks protected branches, or advertises unusual capabilities.

## Error/edge-case checklist (test these)

| Situation | Expected behavior |
| --- | --- |
| Outside a Git repo, in a bare repo, submodule, or on an unborn branch | Specific actionable error; no file or remote writes. |
| Invoked from nested directory, linked worktree, path with spaces/non-UTF-8 components | Resolve Git paths safely; never assume cwd is root or `.git` is a folder; report unsupported path encoding clearly if needed. |
| Detached HEAD, branch switched during push, branch with slash/unusual valid characters | Require an explicit existing branch for detached HEAD; use full ref and snapshotted OID; validate with Git. |
| Missing primary, mirror, or write permission; changed URL or unexpected pushurl; same URL twice | Refuse before writes where detectable; show exact remote and repair guidance without secrets. |
| No SSH key/agent, untrusted host key, expired HTTPS token, inaccessible repository | Fail promptly; suggest diagnostic `ssh -T`/`git ls-remote` and credential setup without attempting to provision access. |
| Primary rejects, mirror rejects, remote diverges, network times out after server accepts | Preserve partial/unknown state; no force or automatic rollback; offer `status --remote` and `retry`. |
| Multiple worktrees or simultaneous CLI/hook calls | Lock to prevent overlapping writes; no lost state; never wait indefinitely. |
| Existing hook, custom `core.hooksPath`, missing binary, hook bypass, manually changed hook | Do not overwrite; provide delegation/setup instructions; uninstall only owned content. |
| Unsupported ref, deletion push, tags, multiple refs in pre-push stdin | Skip unsupported refs explicitly; mirror only supported branch additions/updates at provided OIDs. |
| Malformed/oversized/unknown-version config, symlinks, duplicate entries, embedded credentials | Reject before subprocess writes; redact error output. |
| Repo with LFS, Git submodules, large objects, server quotas, signed/protected branches | Clarify scope: Git object pushes are mirrored; LFS content and external submodule repositories are **not** automatically mirrored. Explain server-side failures. |
| Offline mode, slow remote, user cancellation, disk full during state write | Bounded wait and clear partial/unknown status; never report full success when durable outcome recording fails. |

## Implementation plan and verification

1. Bootstrap Rust crate (2021/2024 edition supported by selected MSRV), `clap`, `serde`/`toml`/`serde_json`, `thiserror`, terminal styling, and an integration-test harness using temporary local Git repositories. Write modules for repo discovery, validated config, remote resolution, Git subprocesses, state/locking, CLI output, and hooks. Prefer Git CLI over `libgit2` for credential-helper and SSH parity.
2. Implement first-run and repeatable `setup`, mirror management, `doctor`, `status`, and human/JSONL output first. Test no config, existing config with missing remotes, and the unchanged no-op case. Add strict config/URL/path validation and redaction before enabling writes.
3. Implement primary-first `push`, durable per-remote state, dry-run, `retry`, lock handling, and exit codes. Test local bare remotes through a controlled fake SSH transport (production must still reject local-path URLs) and controlled Git failures without provider accounts; assert no mirror update if primary rejects, and partial status if a mirror rejects.
4. Implement opt-in pre-push hook, conflict-safe install/uninstall, stdin ref parsing, recursion guard, and manual delegation. Test hook ordering, bypass, multi-ref and failure-policy behavior.
5. Test macOS/Linux/Windows path and process behavior in CI; run `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test`. Snapshot-test root and nested `--help` outside a repository, including `--uiless --help`. Add a manual SSH/HTTPS checklist with disposable repositories; CI should not depend on live providers.

Acceptance: a fresh ordinary Git project can run `setup`, configure two mirrors, and rerun `setup` with no changes; push a branch through the wrapper, observe a partial failure, repair access and retry without force; a new clone can review and apply its local remote setup through the same command; an automation caller gets valid JSONL plus a reliable exit code; hooks do not overwrite user hooks; and all negative cases above produce safe, actionable results.
