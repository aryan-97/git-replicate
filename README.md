# git-replicate

Copy selected Git branch pushes from a primary remote to mirror remotes. See `GIT_REPLICATE.md` for the specification and `git-replicate --help` for the manual.

```text
git-replicate setup
git-replicate mirror add gitlab --url git@gitlab.com:team/project.git
git-replicate doctor --check-remote
git-replicate push            # primary first, then every mirror, same commit
git-replicate retry           # repair failed mirrors without force
git-replicate status --remote
```

**An ordinary `git push` is not mirrored** unless you opt in with `git-replicate hook install pre-push`. Git has no post-push hook, so the hook runs before the primary push and cannot know it will succeed. Hooks are per clone and skipped by `--no-verify`.

Build and verify: `cargo build --release`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (help snapshots: `UPDATE_SNAPSHOTS=1 cargo test`). Put the binary on `PATH` as `git-replicate` (the hook looks it up there).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | all selected writes succeeded (or local dry-run plan) |
| 2 | usage, configuration or repository error |
| 3 | primary push failed or outcome unknown |
| 4 | primary succeeded; a mirror failed, is unknown, or state could not be recorded |
| 5 | hook install/conflict error |
| 6 | read-only remote check failed |

## `--uiless` JSONL schema v1

One JSON object per stdout line. Every event has all fields (null when irrelevant):
`schema` (1), `event` (`plan`, `remote_result`, `check`, `warning`, `result`), `operation`, `remote`, `branch`, `oid`, `status`, `code`, `message`, `primary_status`, `mirror_results` (`[{remote,status,message}]`), `partial`.
The last line is always a `result` event; `code` there equals the exit status. `status` values: `ok`, `created`, `unchanged`, `planned`, `removed`, `installed`, `partial`, `skipped`, `failed`, `unknown`, `error`. A parent `git push` running the hook may still print non-JSON lines; call the wrapper directly for clean JSONL.

## Manual SSH/HTTPS checklist (disposable repositories)

1. Create two empty repositories on different providers; give your account write access.
2. SSH: `ssh-add -l`, `ssh -T git@HOST` (verify the host fingerprint yourself), `git ls-remote <url>`.
3. HTTPS: configure a credential helper with a token, then `git ls-remote <url>`.
4. `setup`, `mirror add`, `doctor --check-remote`, `push`; revoke access to one mirror, `push` (expect exit 4), restore, `retry`.
5. Repeat in a fresh clone: `setup` shows a plan; confirm it adds only the missing remotes.

CI uses local bare repositories behind a fake SSH command, never live providers.
