use crate::Ctx;
use crate::config::{self, Config};
use crate::git;
use crate::hook::{self, HookState};
use crate::out::{CliError, Final, R, RemoteRes};
use crate::push::ls_remote;
use crate::repo::{self, Repo};
use crate::state::{self, State};
use crate::util::{dest_key, redact, short};
use std::time::Duration;

fn entries(cfg: &Config) -> Vec<(&'static str, String, String)> {
    let mut v = vec![(
        "primary",
        cfg.primary.remote.clone(),
        cfg.primary.url.clone(),
    )];
    v.extend(
        cfg.mirrors
            .iter()
            .map(|m| ("mirror", m.remote.clone(), m.url.clone())),
    );
    v
}

pub fn status(ctx: &Ctx, remote: bool) -> R<i32> {
    let out = &ctx.out;
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    let remotes = repo.remotes()?;
    let st = state::load(&repo).unwrap_or_else(|e| {
        out.warning(&e.msg);
        State::default()
    });
    let branch = repo.current_branch();
    let local_oid = branch.as_deref().and_then(|b| repo.branch_oid(b));
    let refname = branch.as_ref().map(|b| format!("refs/heads/{b}"));
    out.heading(&format!(
        "Branch: {}   Config: {}",
        branch.as_deref().unwrap_or("(detached HEAD)"),
        config::FILE
    ));
    if remote && refname.is_none() {
        out.warning("detached HEAD: no branch to check remotely");
    }
    let attempt = branch
        .as_deref()
        .and_then(|b| st.branches.get(b))
        .and_then(|b| b.wrapper.as_ref());
    let mut rows = Vec::new();
    let mut results = Vec::new();
    let mut remote_failed = false;
    for (role, name, url) in entries(&cfg) {
        let local = repo::check_remote(&repo, &remotes, &name, &url);
        let (cfg_state, detail) = match &local {
            Ok(()) => ("ok", String::new()),
            Err(e) if !remotes.contains(&name) => ("missing", e.clone()),
            Err(e) => ("mismatch", e.clone()),
        };
        let last = match (role, attempt) {
            ("primary", Some(a)) => a.primary.as_str().to_string(),
            (_, Some(a)) => a
                .mirrors
                .get(&name)
                .map(|m| m.status.as_str().to_string())
                .unwrap_or_else(|| "-".into()),
            _ => "-".into(),
        };
        let mut rem = "-".to_string();
        let mut res_status = cfg_state;
        let mut msg = if detail.is_empty() {
            url.clone()
        } else {
            detail
        };
        if remote && local.is_ok() {
            if let Some(r) = &refname {
                match ls_remote(&repo, &name, r) {
                    Ok(Some(o)) if Some(&o) == local_oid.as_ref() => rem = "matches local".into(),
                    Ok(Some(o)) => rem = format!("differs ({})", short(&o)),
                    Ok(None) => rem = "absent".into(),
                    Err(e) => {
                        remote_failed = true;
                        rem = "check failed".into();
                        res_status = "failed";
                        msg = format!("remote check failed: {e}");
                    }
                }
                if res_status == "ok" {
                    msg = format!("{url}: {rem}");
                }
            }
        } else if remote && local.is_err() {
            rem = "skipped (config problem)".into();
        }
        out.remote_result(
            &name,
            branch.as_deref(),
            local_oid.as_deref(),
            res_status,
            &msg,
        );
        results.push(RemoteRes {
            remote: name.clone(),
            status: res_status.into(),
            message: Some(msg),
        });
        rows.push(vec![
            role.to_string(),
            name,
            url,
            cfg_state.to_string(),
            last,
            rem,
        ]);
    }
    out.table(
        &["Role", "Remote", "URL", "Config", "Last push", "Remote ref"],
        rows,
    );
    for (b, bs) in &st.branches {
        for (kind, a) in [("push", &bs.wrapper), ("hook", &bs.hook)] {
            if let Some(a) = a {
                let ms: Vec<String> = a
                    .mirrors
                    .iter()
                    .map(|(k, v)| format!("{k}={}", v.status.as_str()))
                    .collect();
                out.info(&format!(
                    "Last {kind} of {b} at {} ({}): primary={} {}",
                    a.timestamp,
                    short(&a.oid),
                    a.primary.as_str(),
                    ms.join(" ")
                ));
            }
        }
    }
    let h = hook::inspect(&repo);
    let hs = match h.state {
        HookState::Absent => "not installed (ordinary `git push` is not mirrored)".to_string(),
        HookState::Owned { strict } => format!(
            "installed ({})",
            if strict { "strict" } else { "best-effort" }
        ),
        HookState::Modified => "modified tool hook".into(),
        HookState::Foreign => "a different pre-push hook exists".into(),
    };
    out.info(&format!("pre-push hook: {hs}"));
    if !remote {
        out.info("No network used. Add --remote for read-only ls-remote checks.");
    }
    let code = if remote_failed { 6 } else { 0 };
    out.result(Final {
        status: if remote_failed { "error" } else { "ok" }.into(),
        code,
        message: if remote_failed {
            "one or more remote checks failed".into()
        } else {
            "status complete".into()
        },
        branch,
        oid: local_oid,
        mirror_results: Some(results),
        ..Default::default()
    });
    Ok(code)
}

fn remedy(url: &str, remote: &str) -> String {
    let host = dest_key(url).split('/').next().unwrap_or("").to_string();
    if url.starts_with("https://") {
        format!(
            "HTTPS: configure a credential helper/credential manager with a token that has write access, then test `git ls-remote {remote}`. Tokens are never stored by git-replicate."
        )
    } else {
        format!(
            "SSH: check that your key exists and is loaded (ssh-add -l), that the host key is already trusted (run `ssh -T git@{host}` yourself and verify the fingerprint; git-replicate never auto-accepts host keys), and that the account can write to the repository. Test with `git ls-remote {remote}`."
        )
    }
}

fn local_checks(ctx: &Ctx, repo: &Repo, cfg: &Config) -> bool {
    let out = &ctx.out;
    let mut bad = false;
    let remotes = repo.remotes().unwrap_or_default();
    for (role, name, url) in entries(cfg) {
        match repo::check_remote(repo, &remotes, &name, &url) {
            Ok(()) => out.check(
                Some(&name),
                "ok",
                &format!(
                    "{role} push destination {} matches .gitreplicate",
                    redact(&url)
                ),
            ),
            Err(e) => {
                bad = true;
                out.check(Some(&name), "error", &e);
            }
        }
    }
    bad
}

pub fn doctor(ctx: &Ctx, check_remote: bool) -> R<i32> {
    let out = &ctx.out;
    let mut local_bad = false;
    let g = git::run(&ctx.cwd, &["--version"], &[], git::LOCAL_TIMEOUT);
    if g.ok() {
        out.check(None, "ok", g.stdout.trim());
    } else {
        return Err(
            CliError::usage("git is not available").hint("Install Git and ensure it is on PATH.")
        );
    }
    let repo = repo::discover(&ctx.cwd)?;
    out.check(None, "ok", &format!("worktree: {}", repo.root.display()));
    let cfg = match config::load(&repo.root) {
        Ok(c) => {
            out.check(
                None,
                "ok",
                &format!("{} is valid ({} mirror(s))", config::FILE, c.mirrors.len()),
            );
            Some(c)
        }
        Err(e) => {
            local_bad = true;
            out.check(
                None,
                "error",
                &format!("{} {}", e.msg, e.hint.clone().unwrap_or_default()),
            );
            None
        }
    };
    let mut ok_remotes: Vec<(String, String)> = Vec::new();
    if let Some(cfg) = &cfg {
        local_bad |= local_checks(ctx, &repo, cfg);
        let remotes = repo.remotes().unwrap_or_default();
        for (_, name, url) in entries(cfg) {
            if repo::check_remote(&repo, &remotes, &name, &url).is_ok() {
                ok_remotes.push((name, url));
            }
        }
        out.check(None, "warn", "Trust boundary: Git's global config, credential helpers, URL rewrites and SSH configuration can run commands or redirect traffic. Effective push destinations are listed above.");
        if repo
            .git(&[
                "config",
                "--get-regexp",
                r"^url\..*\.(insteadof|pushinsteadof)$",
            ])
            .ok()
        {
            out.check(None, "warn", "URL rewrite rules (insteadOf/pushInsteadOf) are active; pushes are refused when they change a configured destination.");
        }
        let any_ssh = entries(cfg)
            .iter()
            .any(|(_, _, u)| !u.starts_with("https://"));
        if any_ssh {
            match repo.ssh_overridden() {
                Some(w) => out.check(None, "warn", &format!("SSH command is set via {w}; make sure it is non-interactive (e.g. ssh -o BatchMode=yes) so automation cannot hang on prompts.")),
                None => out.check(None, "ok", "SSH runs non-interactively (ssh -o BatchMode=yes); unknown host keys fail instead of being accepted."),
            }
        }
        if entries(cfg)
            .iter()
            .any(|(_, _, u)| u.starts_with("https://"))
            && !repo.git(&["config", "--get-all", "credential.helper"]).ok()
        {
            out.check(None, "warn", "No credential.helper is configured; HTTPS pushes cannot prompt (GIT_TERMINAL_PROMPT=0) and will fail without stored credentials.");
        }
    }
    let h = hook::inspect(&repo);
    match h.state {
        HookState::Absent => out.check(None, "ok", "pre-push hook not installed (optional)"),
        HookState::Owned { strict } => out.check(None, "ok", &format!("pre-push hook installed ({})", if strict { "strict" } else { "best-effort" })),
        HookState::Modified => out.check(None, "warn", "pre-push hook carries the tool marker but was modified"),
        HookState::Foreign => out.check(None, "warn", "a different pre-push hook exists; delegate with `git-replicate hook run pre-push \"$@\"`"),
    }
    if h.external {
        out.check(
            None,
            "warn",
            &format!(
                "hooks directory {} is outside the repository; hook install will refuse",
                h.dir.display()
            ),
        );
    }
    match state::load(&repo) {
        Ok(_) => out.check(None, "ok", "state file readable (or absent)"),
        Err(e) => {
            local_bad = true;
            out.check(
                None,
                "error",
                &format!("{} {}", e.msg, e.hint.unwrap_or_default()),
            );
        }
    }
    let lock = state::dir(&repo).join("lock");
    if lock.exists() {
        if state::lock_is_stale(&lock) {
            out.check(None, "warn", &format!("stale lock {}: its owner is gone; it is removed automatically on the next operation", lock.display()));
        } else {
            out.check(
                None,
                "warn",
                &format!("lock {} is held by a running operation", lock.display()),
            );
        }
    }
    let mut remote_bad = false;
    if check_remote {
        let t = git::net_timeout().min(Duration::from_secs(30));
        for (name, url) in &ok_remotes {
            let o = repo.git_net_timeout(&["ls-remote", name, "HEAD"], false, t);
            if o.ok() {
                out.check(Some(name), "ok", "read-only ls-remote succeeded");
            } else {
                remote_bad = true;
                let why = if o.timed_out {
                    "timed out".to_string()
                } else {
                    git::summarize(&o)
                };
                out.check(
                    Some(name),
                    "failed",
                    &format!("ls-remote failed: {why}. {}", remedy(url, name)),
                );
            }
        }
    } else {
        out.info("Remote checks skipped; use --check-remote for read-only connectivity tests.");
    }
    let code = if local_bad {
        2
    } else if remote_bad {
        6
    } else {
        0
    };
    out.result(Final {
        status: if code == 0 { "ok" } else { "error" }.into(),
        code,
        message: match code {
            0 => "no problems found".into(),
            2 => "configuration problems found (see above)".into(),
            _ => "remote check failed (see above)".into(),
        },
        ..Default::default()
    });
    Ok(code)
}
