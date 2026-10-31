use crate::Ctx;
use crate::config::{self, Config, Mirror};
use crate::git;
use crate::out::{CliError, Final, R, RemoteRes};
use crate::repo::{self, Repo};
use crate::state::{self, Attempt, LOCK_WAIT, Lock, St, State};
use crate::util::short;

pub struct Pushed {
    pub st: St,
    pub msg: String,
    pub raw: String,
}

pub fn push_ref(repo: &Repo, remote: &str, oid: &str, refname: &str) -> Pushed {
    let spec = format!("{oid}:{refname}");
    let o = repo.git_net(&["push", "--porcelain", remote, &spec], true);
    let raw = o.stderr.clone();
    if o.timed_out {
        return Pushed {
            st: St::Unknown,
            msg: "timed out; the remote may or may not have accepted the update (check with `git-replicate status --remote`)".into(),
            raw,
        };
    }
    if !o.ok() {
        return Pushed {
            st: St::Failed,
            msg: git::summarize(&o),
            raw,
        };
    }
    let flag = |c: char| {
        o.stdout
            .lines()
            .any(|l| l.starts_with(c) && l[c.len_utf8()..].starts_with('\t'))
    };
    let msg = if flag('=') {
        "already up to date"
    } else if flag('*') {
        "new branch created"
    } else {
        "updated"
    };
    Pushed {
        st: St::Ok,
        msg: msg.into(),
        raw,
    }
}

pub fn ls_remote(repo: &Repo, remote: &str, refname: &str) -> Result<Option<String>, String> {
    let o = repo.git_net(&["ls-remote", remote, refname], false);
    if o.timed_out {
        return Err("timed out".into());
    }
    if !o.ok() {
        return Err(git::summarize(&o));
    }
    Ok(o.stdout.lines().find_map(|l| {
        let (oid, r) = l.split_once('\t')?;
        (r.trim() == refname).then(|| oid.to_string())
    }))
}

fn results(cfg: &Config, att: &Attempt) -> Vec<RemoteRes> {
    cfg.mirrors
        .iter()
        .map(|m| match att.mirrors.get(&m.remote) {
            Some(s) => RemoteRes {
                remote: m.remote.clone(),
                status: s.status.as_str().into(),
                message: s.message.clone(),
            },
            None => RemoteRes {
                remote: m.remote.clone(),
                status: "skipped".into(),
                message: None,
            },
        })
        .collect()
}

fn push_mirrors(
    ctx: &Ctx,
    repo: &Repo,
    mirrors: &[&Mirror],
    att: &mut Attempt,
    state: &mut State,
    refname: &str,
) -> bool {
    let mut recorded = true;
    let oid = att.oid.clone();
    for m in mirrors {
        ctx.out.info(&format!(
            "Pushing {} to mirror '{}'...",
            short(&oid),
            m.remote
        ));
        let p = push_ref(repo, &m.remote, &oid, refname);
        ctx.out.detail(&p.raw);
        att.set(&m.remote, p.st, Some(p.msg.clone()));
        if let Err(e) = state::record(repo, state, att, false) {
            recorded = false;
            ctx.out.warning(&format!("could not record state: {e}"));
        }
        ctx.out.remote_result(
            &m.remote,
            Some(&att.branch),
            Some(&oid),
            p.st.as_str(),
            &p.msg,
        );
    }
    recorded
}

pub fn push(ctx: &Ctx, branch: Option<String>, dry: bool) -> R<i32> {
    let out = &ctx.out;
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    repo::check_dest(&repo, &cfg)?;
    let (name, oid) = repo::resolve_branch(&repo, branch.as_deref())?;
    crate::allow::enforce(&cfg, &name)?;
    let refname = format!("refs/heads/{name}");
    if dry {
        out.plan(
            Some(&cfg.primary.remote),
            Some(&name),
            Some(&oid),
            &format!(
                "push {}:{refname} to primary '{}'",
                short(&oid),
                cfg.primary.remote
            ),
        );
        for m in &cfg.mirrors {
            out.plan(
                Some(&m.remote),
                Some(&name),
                Some(&oid),
                &format!(
                    "then push {}:{refname} to mirror '{}' if the primary succeeds",
                    short(&oid),
                    m.remote
                ),
            );
        }
        out.info("Dry run: local plan only; no network access, no remote changes.");
        out.result(Final {
            status: "planned".into(),
            message: "dry run complete".into(),
            branch: Some(name),
            oid: Some(oid),
            ..Default::default()
        });
        return Ok(0);
    }
    let _lock = Lock::acquire(&repo, LOCK_WAIT)?;
    let mut st = state::load(&repo)?;
    out.info(&format!(
        "Pushing {name} ({}) to primary '{}'...",
        short(&oid),
        cfg.primary.remote
    ));
    let p = push_ref(&repo, &cfg.primary.remote, &oid, &refname);
    out.detail(&p.raw);
    out.remote_result(
        &cfg.primary.remote,
        Some(&name),
        Some(&oid),
        p.st.as_str(),
        &p.msg,
    );
    let mut att = Attempt::new(&name, &oid, p.st);
    if p.st != St::Ok {
        for m in &cfg.mirrors {
            att.set(
                &m.remote,
                St::Skipped,
                Some("primary did not succeed".into()),
            );
        }
        if let Err(e) = state::record(&repo, &mut st, &att, false) {
            out.warning(&format!("could not record state: {e}"));
        }
        let unknown = p.st == St::Unknown;
        out.result(Final {
            status: "error".into(),
            code: 3,
            message: format!(
                "primary push {}; mirrors were not attempted",
                if unknown {
                    "outcome is unknown"
                } else {
                    "failed"
                }
            ),
            branch: Some(name),
            oid: Some(oid),
            primary_status: Some(p.st.as_str().into()),
            mirror_results: Some(results(&cfg, &att)),
            partial: Some(false),
        });
        return Ok(3);
    }
    for m in &cfg.mirrors {
        att.set(&m.remote, St::Pending, None);
    }
    if let Err(e) = state::record(&repo, &mut st, &att, false) {
        for m in &cfg.mirrors {
            att.set(
                &m.remote,
                St::Skipped,
                Some("state could not be recorded".into()),
            );
        }
        out.result(Final {
            status: "partial".into(),
            code: 4,
            message: format!(
                "primary push succeeded but state could not be recorded ({e}); mirrors were NOT attempted. Run `git-replicate push` again after fixing the state directory."
            ),
            branch: Some(name),
            oid: Some(oid),
            primary_status: Some("ok".into()),
            mirror_results: Some(results(&cfg, &att)),
            partial: Some(true),
        });
        return Ok(4);
    }
    let all: Vec<&Mirror> = cfg.mirrors.iter().collect();
    let recorded = push_mirrors(ctx, &repo, &all, &mut att, &mut st, &refname);
    if repo.branch_oid(&name).as_deref() != Some(oid.as_str()) {
        out.warning(&format!(
            "local branch '{name}' moved during the push; all remote writes used the snapshot {}",
            short(&oid)
        ));
    }
    finish(ctx, &cfg, att, recorded, "push")
}

fn finish(ctx: &Ctx, cfg: &Config, att: Attempt, recorded: bool, verb: &str) -> R<i32> {
    let bad: Vec<&str> = cfg
        .mirrors
        .iter()
        .filter(|m| {
            att.mirrors
                .get(&m.remote)
                .map(|s| s.status != St::Ok)
                .unwrap_or(true)
        })
        .map(|m| m.remote.as_str())
        .collect();
    let res = results(cfg, &att);
    let (status, code, msg) = if bad.is_empty() && recorded {
        let n = cfg.mirrors.len();
        let extra = if n == 0 {
            " (no mirrors configured)"
        } else {
            ""
        };
        (
            "ok",
            0,
            format!("{verb}: primary and {n} mirror(s) are up to date{extra}"),
        )
    } else if !recorded && bad.is_empty() {
        (
            "partial",
            4,
            format!("{verb}: remotes updated but the outcome could not be recorded durably"),
        )
    } else {
        (
            "partial",
            4,
            format!(
                "{verb}: primary succeeded but mirror(s) did not: {}. Fix access, check `git-replicate status --remote`, then run `git-replicate retry`",
                bad.join(", ")
            ),
        )
    };
    ctx.out.result(Final {
        status: status.into(),
        code,
        message: msg,
        branch: Some(att.branch.clone()),
        oid: Some(att.oid.clone()),
        primary_status: Some(att.primary.as_str().into()),
        mirror_results: Some(res),
        partial: Some(code == 4),
    });
    Ok(code)
}

pub fn retry(ctx: &Ctx, branch: Option<String>) -> R<i32> {
    let out = &ctx.out;
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    repo::check_dest(&repo, &cfg)?;
    let name = match branch {
        Some(b) => {
            if !repo.valid_branch_name(&b) {
                return Err(CliError::usage(format!("'{b}' is not a valid branch name")));
            }
            b
        }
        None => repo.current_branch().ok_or_else(|| {
            CliError::usage("HEAD is detached; no current branch").hint("Pass --branch BRANCH.")
        })?,
    };
    crate::allow::enforce(&cfg, &name)?;
    let refname = format!("refs/heads/{name}");
    let _lock = Lock::acquire(&repo, LOCK_WAIT)?;
    let mut st = state::load(&repo)?;
    let fresh = || CliError::usage("run a fresh `git-replicate push` instead");
    let mut att = st
        .branches
        .get(&name)
        .and_then(|b| b.wrapper.clone())
        .ok_or_else(|| {
            CliError::usage(format!(
                "no recorded git-replicate push for branch '{name}'"
            ))
            .hint("Run `git-replicate push`.")
        })?;
    if att.primary != St::Ok {
        return Err(CliError::usage(format!(
            "the last recorded push of '{name}' did not succeed on the primary (status {})",
            att.primary.as_str()
        ))
        .hint(fresh().msg));
    }
    let mut todo: Vec<&Mirror> = Vec::new();
    for m in &cfg.mirrors {
        match att.mirrors.get(&m.remote) {
            Some(s) if s.status == St::Ok => {}
            Some(_) => todo.push(m),
            None => out.warning(&format!(
                "mirror '{}' is not part of the recorded push; a fresh push is needed for it",
                m.remote
            )),
        }
    }
    if todo.is_empty() {
        let res = results(&cfg, &att);
        out.result(Final {
            status: "unchanged".into(),
            message: "nothing to retry; all recorded mirrors succeeded".into(),
            branch: Some(name),
            oid: Some(att.oid.clone()),
            primary_status: Some("ok".into()),
            mirror_results: Some(res),
            partial: Some(false),
            ..Default::default()
        });
        return Ok(0);
    }
    if !repo
        .git(&["cat-file", "-e", &format!("{}^{{commit}}", att.oid)])
        .ok()
    {
        return Err(CliError::usage(format!(
            "recorded commit {} no longer exists locally",
            short(&att.oid)
        ))
        .hint(fresh().msg));
    }
    match ls_remote(&repo, &cfg.primary.remote, &refname) {
        Err(e) => {
            return Err(CliError::new(6, format!("could not verify the primary's '{name}': {e}"))
                .hint("Nothing was pushed. Check connectivity/authentication (`git-replicate doctor --check-remote`)."));
        }
        Ok(Some(o)) if o == att.oid => {}
        Ok(other) => {
            return Err(CliError::usage(format!(
                "primary '{}' now has {} for {name}, not the recorded {}; refusing to retry",
                cfg.primary.remote,
                other
                    .map(|o| short(&o).to_string())
                    .unwrap_or_else(|| "no such branch".into()),
                short(&att.oid)
            ))
            .hint("Inspect the primary, then run a fresh `git-replicate push`."));
        }
    }
    let recorded = push_mirrors(ctx, &repo, &todo, &mut att, &mut st, &refname);
    finish(ctx, &cfg, att, recorded, "retry")
}
