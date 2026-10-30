use crate::config::Config;
use crate::git::{self, GitOut, LOCAL_TIMEOUT};
use crate::out::{CliError, R};
use crate::util::{redact, validate_url};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Repo {
    pub root: PathBuf,
    pub common_dir: PathBuf,
}

fn utf8_path(s: &str) -> R<PathBuf> {
    if s.contains('\u{FFFD}') {
        return Err(CliError::usage(
            "the repository path is not valid UTF-8, which is not supported",
        ));
    }
    Ok(PathBuf::from(s.trim_end_matches(['\n', '\r'])))
}

pub fn discover(cwd: &Path) -> R<Repo> {
    let o = git::run(
        cwd,
        &["rev-parse", "--is-bare-repository"],
        &[],
        LOCAL_TIMEOUT,
    );
    if let Some(e) = &o.spawn_err {
        return Err(CliError::usage(e.clone()).hint("Install Git and make sure `git` is on PATH."));
    }
    if !o.ok() {
        return Err(CliError::usage("not a Git repository (or any parent directory)")
            .hint("Run this command inside a Git worktree. Create one with `git init`, then add a primary remote with `git remote add origin <url>`."));
    }
    if o.stdout.trim() == "true" {
        return Err(CliError::usage("bare repositories are not supported in v1")
            .hint("Run git-replicate from an ordinary worktree clone."));
    }
    let top = git::run(cwd, &["rev-parse", "--show-toplevel"], &[], LOCAL_TIMEOUT);
    if !top.ok() {
        return Err(CliError::usage(
            "not inside a Git worktree (are you inside the .git directory?)",
        )
        .hint("Change to the working tree of the repository."));
    }
    let root = utf8_path(&top.stdout)?;
    let dirs = git::run(
        &root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        &[],
        LOCAL_TIMEOUT,
    );
    if !dirs.ok() {
        return Err(CliError::usage(format!(
            "could not resolve the Git directory: {}",
            git::summarize(&dirs)
        )));
    }
    let common_dir = utf8_path(&dirs.stdout)?;
    let sup = git::run(
        &root,
        &["rev-parse", "--show-superproject-working-tree"],
        &[],
        LOCAL_TIMEOUT,
    );
    if sup.ok() && !sup.stdout.trim().is_empty() {
        return Err(CliError::usage("submodule worktrees are not supported in v1")
            .hint("Run git-replicate in the top-level repository or in a standalone clone of the submodule."));
    }
    Ok(Repo { root, common_dir })
}

impl Repo {
    pub fn git(&self, args: &[&str]) -> GitOut {
        git::run(&self.root, args, &[], LOCAL_TIMEOUT)
    }

    pub fn text(&self, args: &[&str]) -> Result<String, String> {
        let o = self.git(args);
        if o.ok() {
            Ok(o.stdout.trim_end().to_string())
        } else {
            Err(git::summarize(&o))
        }
    }

    pub fn net_env(&self, skip_hook: bool) -> Vec<(String, String)> {
        let mut env = vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())];
        if skip_hook {
            env.push(("GIT_REPLICATE_SKIP_HOOK".into(), "1".into()));
        }
        if self.ssh_overridden().is_none() {
            env.push(("GIT_SSH_COMMAND".into(), "ssh -o BatchMode=yes".into()));
        }
        env
    }

    pub fn ssh_overridden(&self) -> Option<&'static str> {
        if std::env::var_os("GIT_SSH_COMMAND").is_some() {
            return Some("GIT_SSH_COMMAND");
        }
        if std::env::var_os("GIT_SSH").is_some() {
            return Some("GIT_SSH");
        }
        if self.git(&["config", "--get", "core.sshCommand"]).ok() {
            return Some("core.sshCommand");
        }
        None
    }

    pub fn git_net(&self, args: &[&str], skip_hook: bool) -> GitOut {
        self.git_net_timeout(args, skip_hook, git::net_timeout())
    }

    pub fn git_net_timeout(&self, args: &[&str], skip_hook: bool, t: Duration) -> GitOut {
        git::run(&self.root, args, &self.net_env(skip_hook), t)
    }

    pub fn remotes(&self) -> R<Vec<String>> {
        match self.text(&["remote"]) {
            Ok(s) => Ok(s
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()),
            Err(e) => Err(CliError::usage(format!("could not list remotes: {e}"))),
        }
    }

    pub fn push_urls(&self, remote: &str) -> Result<Vec<String>, String> {
        self.text(&["remote", "get-url", "--push", "--all", remote])
            .map(|s| {
                s.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
    }

    pub fn single_push_url(&self, remote: &str) -> Result<String, String> {
        let urls = self.push_urls(remote)?;
        match urls.len() {
            1 => Ok(urls.into_iter().next().unwrap()),
            n => Err(format!(
                "remote '{remote}' has {n} push URLs; exactly one is required"
            )),
        }
    }

    pub fn create_remote(&self, name: &str, url: &str) -> Result<(), String> {
        self.text(&["remote", "add", "--", name, url])?;
        self.text(&[
            "config",
            &format!("remote.{name}.gitreplicate-managed"),
            "true",
        ])
        .map(|_| ())
        .inspect_err(|_| {
            let _ = self.remove_remote(name);
        })
    }

    pub fn remove_remote(&self, name: &str) -> Result<(), String> {
        self.text(&["remote", "remove", name]).map(|_| ())
    }

    pub fn is_managed(&self, name: &str) -> bool {
        self.text(&[
            "config",
            "--get",
            &format!("remote.{name}.gitreplicate-managed"),
        ])
        .map(|v| v == "true")
        .unwrap_or(false)
    }

    pub fn current_branch(&self) -> Option<String> {
        self.text(&["symbolic-ref", "-q", "HEAD"])
            .ok()
            .and_then(|r| r.strip_prefix("refs/heads/").map(String::from))
    }

    pub fn branch_oid(&self, name: &str) -> Option<String> {
        self.text(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}^{{commit}}"),
        ])
        .ok()
    }

    pub fn valid_branch_name(&self, name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with('-')
            && self
                .git(&["check-ref-format", &format!("refs/heads/{name}")])
                .ok()
    }

    pub fn valid_ref(&self, r: &str) -> bool {
        r.starts_with("refs/heads/") && self.git(&["check-ref-format", r]).ok()
    }
}

pub fn resolve_branch(repo: &Repo, explicit: Option<&str>) -> R<(String, String)> {
    let name = match explicit {
        Some(b) => {
            if !repo.valid_branch_name(b) {
                return Err(CliError::usage(format!("'{b}' is not a valid branch name")));
            }
            b.to_string()
        }
        None => repo.current_branch().ok_or_else(|| {
            CliError::usage("HEAD is detached; no current branch to replicate")
                .hint("Pass --branch BRANCH with an existing local branch.")
        })?,
    };
    match repo.branch_oid(&name) {
        Some(oid) => Ok((name, oid)),
        None => Err(CliError::usage(if explicit.is_some() {
            format!("'{name}' is not an existing local branch with commits")
        } else {
            format!("branch '{name}' has no commits yet (unborn branch)")
        })
        .hint("Create a commit first; git-replicate never creates commits.")),
    }
}

pub fn check_remote(
    repo: &Repo,
    remotes: &[String],
    remote: &str,
    expected: &str,
) -> Result<(), String> {
    if !remotes.iter().any(|r| r == remote) {
        return Err(format!(
            "git remote '{remote}' is missing. Run `git-replicate setup` to review and add it, or `git remote add {remote} <url>`"
        ));
    }
    let resolved = repo.single_push_url(remote)?;
    if let Err(e) = validate_url(&resolved) {
        return Err(format!(
            "resolved push URL {} is not allowed: {e}",
            redact(&resolved)
        ));
    }
    if resolved != expected {
        return Err(format!(
            "push URL changed: git resolves '{}' but .gitreplicate expects '{}'. Check remote.{remote}.pushurl and url.*.insteadOf/pushInsteadOf, then fix the remote (git remote set-url) or edit .gitreplicate after review",
            redact(&resolved),
            redact(expected)
        ));
    }
    Ok(())
}

pub fn dest_problems(repo: &Repo, cfg: &Config) -> R<Vec<(String, String)>> {
    let remotes = repo.remotes()?;
    let mut v = Vec::new();
    if let Err(e) = check_remote(repo, &remotes, &cfg.primary.remote, &cfg.primary.url) {
        v.push((cfg.primary.remote.clone(), e));
    }
    for m in &cfg.mirrors {
        if let Err(e) = check_remote(repo, &remotes, &m.remote, &m.url) {
            v.push((m.remote.clone(), e));
        }
    }
    Ok(v)
}

pub fn check_dest(repo: &Repo, cfg: &Config) -> R<()> {
    let p = dest_problems(repo, cfg)?;
    if p.is_empty() {
        return Ok(());
    }
    let msg = p
        .iter()
        .map(|(r, e)| format!("{r}: {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    Err(CliError::usage(format!("remote configuration does not match .gitreplicate; nothing was pushed\n{msg}"))
        .hint("In a new clone, run `git-replicate setup` to review and add missing remotes. Mismatches are never repaired automatically."))
}
