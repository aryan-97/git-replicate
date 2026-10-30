use clap::{Args, Parser, Subcommand, ValueEnum};

const ROOT_TEMPLATE: &str = "\
NAME
    git-replicate - copy selected Git branch pushes from a primary remote to mirror remotes

SYNOPSIS
    git-replicate [--uiless] [--verbose] [--no-color] <COMMAND> [ARGS]
    git-replicate help [COMMAND]

DESCRIPTION
    git-replicate pushes a branch to your primary remote first and, only if that
    succeeds, pushes the exact same commit to each configured mirror remote
    (GitHub, GitLab, Bitbucket, Gitea or any Git server reachable over SSH or
    HTTPS). It matches selected branch refs; it is NOT `git push --mirror`.
    By default only the current local branch is pushed: no tags, notes,
    pull-request refs, deletions or force pushes, ever.

    An ordinary `git push` is NOT mirrored unless you opt in to the pre-push hook
    (`git-replicate hook install pre-push`). Hooks are local to one clone, are
    not shared by cloning, and can be bypassed (`git push --no-verify`). Git has
    no post-push hook, so only `git-replicate push` can guarantee that mirrors
    are updated after the primary succeeded. Updates across remotes are not
    atomic: a primary success followed by a mirror failure is reported as a
    partial result (exit status 4) and can be repaired with `retry`.

COMMANDS
{subcommands}

OPTIONS
{options}

EXAMPLES
    git-replicate setup
    git-replicate mirror add gitlab --url git@gitlab.com:team/project.git
    git-replicate mirror add work --remote existing-work-remote
    git-replicate doctor --check-remote
    git-replicate push
    git-replicate push --dry-run
    git-replicate retry
    git-replicate status --remote
    git-replicate --uiless push          # JSON Lines on stdout, for CI

EXIT STATUS
    0   all selected writes succeeded (or a local dry-run plan succeeded)
    2   usage, configuration or repository error (nothing was pushed)
    3   primary push failed or its outcome is unknown
    4   primary succeeded, but a mirror failed, is unknown, or could not be recorded
    5   hook installation or conflict error
    6   read-only remote check failed (doctor, status, retry pre-check)

CONFIGURATION FILE
    `.gitreplicate` is a UTF-8 TOML file in the worktree root. It may be
    committed and reviewed. It holds expected push URLs, never credentials.

        version = 1

        [primary]
        remote = \"origin\"
        url = \"git@github.com:team/project.git\"

        [[mirrors]]
        name = \"gitlab\"
        remote = \"gitreplicate-gitlab\"
        url = \"git@gitlab.com:team/project.git\"

    Only SSH (git@host:path, ssh://) and https:// URLs without embedded
    credentials are accepted. If the push URL Git resolves for a remote differs
    from the file (pushurl, url.*.insteadOf), the operation is refused.
    Attempt records and a lock live in the Git common directory under
    gitreplicate/ (never in tracked files).

SECURITY
    git-replicate never asks for, stores or prints passwords, tokens or SSH keys;
    authentication is delegated to Git, SSH and your credential helpers. Network
    commands run non-interactively (GIT_TERMINAL_PROMPT=0 and, unless you set
    GIT_SSH_COMMAND, GIT_SSH or core.sshCommand, `ssh -o BatchMode=yes`). Unknown
    SSH host keys are never auto-accepted. URL credentials are redacted from all
    output. A cloned .gitreplicate is untrusted input: `setup` shows a plan and
    needs approval (or --yes) before creating local remotes. LFS content and
    external submodule repositories are not mirrored automatically.

Use `git-replicate <COMMAND> --help` for command-specific help.
";

#[derive(Parser, Debug)]
#[command(
    name = "git-replicate",
    version,
    about = "Copy selected Git branch pushes from a primary remote to mirror remotes",
    help_template = ROOT_TEMPLATE,
    arg_required_else_help = true,
    disable_help_subcommand = false
)]
pub struct Cli {
    /// Emit one JSON object per line (JSONL) on stdout; never prompt
    #[arg(long, global = true, env = "GIT_REPLICATE_UILESS")]
    pub uiless: bool,
    /// Print redacted diagnostics to stderr
    #[arg(long, global = true)]
    pub verbose: bool,
    /// Disable colored output (NO_COLOR and CLICOLOR=0 are also honored)
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create or verify .gitreplicate and local mirror remotes (idempotent)
    #[command(after_help = SETUP_HELP)]
    Setup(SetupArgs),
    /// Manage mirror entries (add, list, remove)
    #[command(subcommand)]
    Mirror(MirrorCmd),
    /// Push a branch to the primary, then to every mirror (primary first)
    #[command(after_help = PUSH_HELP)]
    Push(PushArgs),
    /// Retry failed mirrors of the last partial push
    #[command(after_help = RETRY_HELP)]
    Retry(RetryArgs),
    /// Show configuration, recorded attempts and hook state
    #[command(after_help = STATUS_HELP)]
    Status(StatusArgs),
    /// Diagnose configuration, remotes, hooks and authentication
    #[command(after_help = DOCTOR_HELP)]
    Doctor(DoctorArgs),
    /// Manage the optional pre-push hook
    #[command(subcommand)]
    Hook(HookCmd),
}

#[derive(Args, Debug)]
pub struct SetupArgs {
    /// Existing remote to use as the primary (required in automation if ambiguous)
    #[arg(long, value_name = "NAME")]
    pub primary: Option<String>,
    /// Approve creating missing local mirror remotes without prompting
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    /// Local branch to push (default: the current branch)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Print the local plan only; no network access or remote changes
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Branch whose recorded partial push should be retried (default: current)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Also run read-only `git ls-remote` checks against every remote
    #[arg(long)]
    pub remote: bool,
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Also run read-only, time-bounded `git ls-remote` checks
    #[arg(long)]
    pub check_remote: bool,
}

#[derive(Subcommand, Debug)]
pub enum MirrorCmd {
    /// Add a mirror from a new URL or an existing Git remote
    #[command(after_help = MIRROR_ADD_HELP)]
    Add {
        /// Short mirror name (letters, digits, '-' and '_')
        name: String,
        /// SSH or HTTPS URL; creates local remote gitreplicate-NAME
        #[arg(long, conflicts_with = "remote", required_unless_present = "remote")]
        url: Option<String>,
        /// Adopt an existing Git remote
        #[arg(long, value_name = "REMOTE")]
        remote: Option<String>,
    },
    /// List configured mirrors and their local state
    #[command(
        after_help = "EXAMPLES\n    git-replicate mirror list\n    git-replicate --uiless mirror list\n\nSAFETY\n    Read-only; never contacts a remote."
    )]
    List,
    /// Remove a mirror entry from .gitreplicate
    #[command(after_help = MIRROR_REMOVE_HELP)]
    Remove {
        /// Mirror name
        name: String,
        /// Also remove the Git remote, if this tool created it and its URL still matches
        #[arg(long)]
        remove_managed_remote: bool,
        /// Approve remote removal without prompting
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum HookEvent {
    #[value(name = "pre-push")]
    PrePush,
}

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// Install the tool-owned pre-push hook (never overwrites existing hooks)
    #[command(after_help = HOOK_INSTALL_HELP)]
    Install {
        /// Hook event
        event: HookEvent,
        /// Block the primary push when a mirror fails (mirrors may already have changed)
        #[arg(long)]
        strict: bool,
    },
    /// Remove the hook only if it is exactly the tool-owned hook
    #[command(
        after_help = "EXAMPLES\n    git-replicate hook uninstall pre-push\n\nSAFETY\n    Refuses to remove hooks that are missing the ownership marker or were edited."
    )]
    Uninstall {
        /// Hook event
        event: HookEvent,
    },
    /// Show the effective hook directory and hook ownership
    #[command(
        after_help = "EXAMPLES\n    git-replicate hook status\n\nSAFETY\n    Read-only; never contacts a remote."
    )]
    Status,
    /// Internal entry point used by hooks (also for manual delegation)
    #[command(after_help = HOOK_RUN_HELP)]
    Run {
        /// Hook event
        event: HookEvent,
        /// Fail (exit 4) when a mirror fails
        #[arg(long)]
        strict: bool,
        /// Arguments Git passes to the hook: REMOTE_NAME REMOTE_URL
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
}

const SETUP_HELP: &str = "\
DESCRIPTION
    Idempotent entry point. First run: choose the primary remote (origin if
    present, else the only remote) and create .gitreplicate with its validated
    push URL. In an existing clone: compare .gitreplicate with .git/config and
    add only the missing mirror remotes after approval. When nothing is missing it
    prints \"Already set up; nothing to do\" and changes nothing.

EXAMPLES
    git-replicate setup
    git-replicate setup --primary origin --yes     # automation, primary-only config
    git-replicate --uiless setup --yes             # CI: apply remotes from reviewed config

SAFETY
    Never contacts the network, never copies commits, never overwrites an
    existing config or remote, never installs a hook. In --uiless mode it never
    prompts: pass --primary when ambiguous and --yes before creating remotes.
    Only run `setup --yes` after you trust the checked-out .gitreplicate.";

const PUSH_HELP: &str = "\
DESCRIPTION
    Pushes OID:refs/heads/BRANCH to the primary, then, only on success, the same
    snapshotted commit to each mirror in order. A failing mirror does not stop the
    others. Never forces, deletes, pushes tags, or pulls/commits/fetches.

EXAMPLES
    git-replicate push
    git-replicate push --branch release/1.x
    git-replicate push --dry-run

SAFETY
    --dry-run is a local plan only. Exit 3: primary failed (mirrors untouched).
    Exit 4: primary succeeded but a mirror failed; fix access, then `retry`.
    Remote updates are not atomic across remotes.";

const RETRY_HELP: &str = "\
DESCRIPTION
    Retries only the unsuccessful mirrors of the last recorded wrapper push of a
    branch, after a read-only ls-remote confirms the primary still points to the
    recorded commit. Otherwise run a fresh `git-replicate push`.

EXAMPLES
    git-replicate retry
    git-replicate retry --branch main

SAFETY
    Never forces. Use `status --remote` first if an outcome is `unknown`.";

const STATUS_HELP: &str = "\
EXAMPLES
    git-replicate status
    git-replicate status --remote

SAFETY
    Without --remote no network is used. --remote only runs read-only
    `git ls-remote` against remotes whose push URL matches .gitreplicate.";

const DOCTOR_HELP: &str = "\
DESCRIPTION
    Explains missing Git/config, invalid remotes, hooks, URL drift (pushurl,
    insteadOf rewrites) and how to fix SSH/HTTPS authentication.

EXAMPLES
    git-replicate doctor
    git-replicate doctor --check-remote

SAFETY
    Remote checks are opt-in, bounded by a timeout and read-only. Git's own
    config, credential helpers, URL rewrites and SSH configuration can run
    commands or redirect traffic; doctor shows the effective destinations.";

const MIRROR_ADD_HELP: &str = "\
DESCRIPTION
    --url creates the local remote gitreplicate-NAME; --remote adopts an existing
    remote. The expected push URL is recorded in .gitreplicate.

EXAMPLES
    git-replicate mirror add gitlab --url git@gitlab.com:team/project.git
    git-replicate mirror add work --remote existing-work-remote

SAFETY
    URLs with embedded credentials, http://, file://, git://, ext:: or local
    paths are rejected. Name collisions and using the primary as a mirror are
    refused. Only resources created by this command are rolled back on error.
    Never contacts the network.";

const MIRROR_REMOVE_HELP: &str = "\
EXAMPLES
    git-replicate mirror remove gitlab
    git-replicate mirror remove gitlab --remove-managed-remote

SAFETY
    By default only the .gitreplicate entry is removed and the Git remote is left
    in place. --remove-managed-remote removes a remote only if this tool created
    it and its current URL still matches the config.";

const HOOK_INSTALL_HELP: &str = "\
DESCRIPTION
    Installs a pre-push hook that mirrors eligible branch updates before an
    ordinary `git push` to the primary.

EXAMPLES
    git-replicate hook install pre-push
    git-replicate hook install pre-push --strict

SAFETY
    WARNING: Git has no post-push hook. The hook runs BEFORE the primary push and
    cannot know whether it will succeed; mirrors may end up ahead of the primary.
    Default is best-effort (report mirror failures, let the push continue).
    --strict blocks the primary push on mirror failure, but a mirror may already
    have changed. Hooks are local to this clone and bypassed by --no-verify.
    Existing hooks are never overwritten; core.hooksPath is respected.";

const HOOK_RUN_HELP: &str = "\
DESCRIPTION
    Reads Git's pre-push ref updates (local_ref local_oid remote_ref remote_oid)
    from stdin and mirrors eligible branch updates when invoked for the
    configured primary remote. Use it to delegate from an existing hook:

        #!/bin/sh
        git-replicate hook run pre-push \"$@\" || exit $?
        # (stdin is forwarded; if your hook also reads stdin, save it first:
        #  input=$(cat); printf '%s\\n' \"$input\" | git-replicate hook run pre-push \"$@\")

SAFETY
    Skips tags, deletions, non-branch refs and pushes to other remotes. Honors
    GIT_REPLICATE_SKIP_HOOK=1. Never prompts. Never pushes to the primary.";
