use crate::Ctx;
use crate::cli::SetupArgs;
use crate::config::{self, Config, MAX_NAME, Mirror};
use crate::out::{CliError, Final, R, RemoteRes};
use crate::repo::{self, Repo};
use crate::util::{redact, valid_ident, validate_url};
use std::io::{BufRead, IsTerminal, Write};

fn interactive(ctx: &Ctx) -> bool {
    !ctx.out.json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn ask(q: &str) -> Option<String> {
    print!("{q} ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(n) if n > 0 => Some(s.trim().to_string()),
        _ => None,
    }
}

fn confirm(q: &str) -> bool {
    matches!(
        ask(&format!("{q} [y/N]")).as_deref(),
        Some("y" | "Y" | "yes" | "YES" | "Yes")
    )
}

pub fn setup(ctx: &Ctx, a: SetupArgs) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    if !config::exists(&repo.root) {
        return first_run(ctx, &repo, a);
    }
    let out = &ctx.out;
    let cfg = config::load(&repo.root)?;
    let remotes = repo.remotes()?;
    repo::check_remote(&repo, &remotes, &cfg.primary.remote, &cfg.primary.url)
        .map_err(|e| CliError::usage(format!("primary remote: {e}")))?;
    let mut missing = Vec::new();
    let mut bad = Vec::new();
    for m in &cfg.mirrors {
        if !remotes.contains(&m.remote) {
            missing.push(m);
        } else if let Err(e) = repo::check_remote(&repo, &remotes, &m.remote, &m.url) {
            bad.push(format!("{}: {e}", m.remote));
        }
    }
    if !bad.is_empty() {
        return Err(CliError::usage(format!("existing remotes do not match .gitreplicate:\n{}", bad.join("\n")))
            .hint("Nothing was changed. Fix the remote (git remote set-url) or review and edit .gitreplicate."));
    }
    if missing.is_empty() {
        out.result(Final {
            status: "unchanged".into(),
            message: "Already set up; nothing to do".into(),
            ..Default::default()
        });
        return Ok(0);
    }
    out.heading("Plan: add missing local mirror remotes from .gitreplicate");
    for m in &missing {
        out.plan(
            Some(&m.remote),
            None,
            None,
            &format!("git remote add {} {}", m.remote, m.url),
        );
    }
    if !a.yes {
        if !interactive(ctx) {
            return Err(CliError::usage(format!(
                "{} local remote(s) must be created from .gitreplicate, which needs approval",
                missing.len()
            ))
            .hint("Review .gitreplicate, then re-run with --yes."));
        }
        if !confirm("Create these remotes?") {
            out.result(Final {
                status: "unchanged".into(),
                message: "Aborted; nothing changed".into(),
                ..Default::default()
            });
            return Ok(0);
        }
    }
    let mut created: Vec<&str> = Vec::new();
    for m in &missing {
        if let Err(e) = repo.create_remote(&m.remote, &m.url) {
            for c in &created {
                let _ = repo.remove_remote(c);
            }
            return Err(
                CliError::usage(format!("could not create remote '{}': {e}", m.remote))
                    .hint("Remotes created earlier in this run were rolled back."),
            );
        }
        created.push(&m.remote);
        out.remote_result(&m.remote, None, None, "created", "remote added");
    }
    out.result(Final {
        status: "created".into(),
        message: format!("Added {} missing mirror remote(s)", created.len()),
        mirror_results: Some(
            created
                .iter()
                .map(|r| RemoteRes {
                    remote: r.to_string(),
                    status: "created".into(),
                    message: None,
                })
                .collect(),
        ),
        ..Default::default()
    });
    Ok(0)
}

fn first_run(ctx: &Ctx, repo: &Repo, a: SetupArgs) -> R<i32> {
    let out = &ctx.out;
    let remotes = repo.remotes()?;
    if remotes.is_empty() {
        return Err(CliError::usage(
            "this repository has no remotes, so there is no primary to record",
        )
        .hint("Add one first: git remote add origin <ssh-or-https-url>"));
    }
    let primary = match a.primary {
        Some(p) => {
            if !remotes.contains(&p) {
                return Err(CliError::usage(format!("remote '{p}' does not exist"))
                    .hint(format!("Existing remotes: {}", remotes.join(", "))));
            }
            p
        }
        None if remotes.iter().any(|r| r == "origin") => "origin".into(),
        None if remotes.len() == 1 => remotes[0].clone(),
        None if interactive(ctx) => {
            println!("Multiple remotes found:");
            for (i, r) in remotes.iter().enumerate() {
                println!("  {}) {r}", i + 1);
            }
            let pick =
                ask("Choose the primary remote (number):").and_then(|s| s.parse::<usize>().ok());
            match pick.and_then(|n| remotes.get(n.wrapping_sub(1))) {
                Some(r) => r.clone(),
                None => return Err(CliError::usage("no valid primary remote chosen")),
            }
        }
        None => {
            return Err(CliError::usage(format!(
                "several remotes exist and none is named 'origin' ({}); the primary is ambiguous",
                remotes.join(", ")
            ))
            .hint("Re-run with --primary NAME."));
        }
    };
    let url = repo
        .single_push_url(&primary)
        .map_err(|e| CliError::usage(format!("primary remote '{primary}': {e}")))?;
    validate_url(&url).map_err(|e| {
        CliError::usage(format!(
            "primary push URL {} is not supported: {e}",
            redact(&url)
        ))
        .hint("Use an SSH or HTTPS URL without embedded credentials (git remote set-url).")
    })?;
    let mut cfg = Config::new(&primary, &url);
    config::validate(&cfg).map_err(CliError::usage)?;
    config::create(&repo.root, &cfg)?;
    out.remote_result(
        &primary,
        None,
        None,
        "created",
        &format!("primary recorded in {} ({})", config::FILE, redact(&url)),
    );
    if interactive(ctx) && !a.yes {
        while confirm("Add a mirror now?") {
            let (Some(name), Some(url)) = (ask("Mirror name:"), ask("Mirror URL:")) else {
                break;
            };
            match add_mirror(repo, &mut cfg, &name, Src::Url(url)) {
                Ok(m) => out.remote_result(
                    &m.name,
                    None,
                    None,
                    "created",
                    &format!("mirror added as remote {}", m.remote),
                ),
                Err(e) => out.warning(&e.msg),
            }
        }
    }
    out.result(Final {
        status: "created".into(),
        message: format!(
            "Created {} with primary '{primary}' and {} mirror(s). Add mirrors with `git-replicate mirror add`.",
            config::FILE,
            cfg.mirrors.len()
        ),
        ..Default::default()
    });
    Ok(0)
}

pub enum Src {
    Url(String),
    Remote(String),
}

pub fn add_mirror(repo: &Repo, cfg: &mut Config, name: &str, src: Src) -> R<Mirror> {
    if !valid_ident(name, MAX_NAME) {
        return Err(
            CliError::usage(format!("invalid mirror name '{name}'")).hint(format!(
                "Use a letter followed by letters, digits, '-' or '_' (max {MAX_NAME} chars)."
            )),
        );
    }
    if cfg
        .mirrors
        .iter()
        .any(|m| m.name.eq_ignore_ascii_case(name))
    {
        return Err(CliError::usage(format!(
            "a mirror named '{name}' already exists"
        )));
    }
    let remotes = repo.remotes()?;
    repo::check_remote(repo, &remotes, &cfg.primary.remote, &cfg.primary.url)
        .map_err(|e| CliError::usage(format!("primary remote: {e}")))?;
    let (remote, url, created) = match src {
        Src::Url(u) => {
            validate_url(&u).map_err(|e| CliError::usage(format!("mirror URL rejected: {e}")))?;
            let r = format!("gitreplicate-{name}");
            if remotes.contains(&r) {
                return Err(
                    CliError::usage(format!("a Git remote named '{r}' already exists")).hint(
                        format!("Adopt it with: git-replicate mirror add {name} --remote {r}"),
                    ),
                );
            }
            (r, u, true)
        }
        Src::Remote(r) => {
            if !remotes.contains(&r) {
                return Err(CliError::usage(format!("Git remote '{r}' does not exist")));
            }
            let url = repo
                .single_push_url(&r)
                .map_err(|e| CliError::usage(format!("remote '{r}': {e}")))?;
            validate_url(&url).map_err(|e| {
                CliError::usage(format!(
                    "remote '{r}' push URL {} rejected: {e}",
                    redact(&url)
                ))
            })?;
            (r, url, false)
        }
    };
    let m = Mirror {
        name: name.into(),
        remote: remote.clone(),
        url: url.clone(),
    };
    let mut new = cfg.clone();
    new.mirrors.push(m.clone());
    config::validate(&new).map_err(CliError::usage)?;
    if created {
        repo.create_remote(&remote, &url)
            .map_err(|e| CliError::usage(format!("could not create remote '{remote}': {e}")))?;
    }
    if let Err(e) = config::save(&repo.root, &new) {
        if created {
            let _ = repo.remove_remote(&remote);
        }
        return Err(e);
    }
    *cfg = new;
    Ok(m)
}

pub fn mirror_add(ctx: &Ctx, name: &str, url: Option<String>, remote: Option<String>) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let mut cfg = config::load(&repo.root)?;
    let src = match (url, remote) {
        (Some(u), None) => Src::Url(u),
        (None, Some(r)) => Src::Remote(r),
        _ => {
            return Err(CliError::usage(
                "exactly one of --url or --remote is required",
            ));
        }
    };
    let m = add_mirror(&repo, &mut cfg, name, src)?;
    ctx.out.result(Final {
        status: "created".into(),
        message: format!(
            "Mirror '{}' added (remote {}, {})",
            m.name,
            m.remote,
            redact(&m.url)
        ),
        ..Default::default()
    });
    Ok(0)
}

pub fn mirror_list(ctx: &Ctx) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    let remotes = repo.remotes()?;
    let mut rows = Vec::new();
    let mut results = Vec::new();
    for m in &cfg.mirrors {
        let st = match repo::check_remote(&repo, &remotes, &m.remote, &m.url) {
            Ok(()) => ("ok", "present".to_string()),
            Err(e) if !remotes.contains(&m.remote) => ("missing", e),
            Err(e) => ("mismatch", e),
        };
        rows.push(vec![
            m.name.clone(),
            m.remote.clone(),
            m.url.clone(),
            st.0.to_string(),
        ]);
        ctx.out.remote_result(
            &m.name,
            None,
            None,
            st.0,
            &format!("{} {}", m.url, if st.0 == "ok" { "" } else { &st.1 }),
        );
        results.push(RemoteRes {
            remote: m.name.clone(),
            status: st.0.into(),
            message: Some(m.url.clone()),
        });
    }
    if cfg.mirrors.is_empty() {
        ctx.out
            .info("No mirrors configured. Add one with `git-replicate mirror add NAME --url URL`.");
    } else {
        ctx.out
            .table(&["Name", "Remote", "URL", "Local state"], rows);
    }
    ctx.out.result(Final {
        status: "ok".into(),
        message: format!("{} mirror(s) configured", cfg.mirrors.len()),
        mirror_results: Some(results),
        ..Default::default()
    });
    Ok(0)
}

pub fn mirror_remove(ctx: &Ctx, name: &str, remove_remote: bool, yes: bool) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let mut cfg = config::load(&repo.root)?;
    let idx = cfg
        .mirrors
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            CliError::usage(format!("no mirror named '{name}'"))
                .hint("See `git-replicate mirror list`.")
        })?;
    let m = cfg.mirrors[idx].clone();
    let mut drop_remote = false;
    if remove_remote {
        if !repo.remotes()?.contains(&m.remote) {
            ctx.out.warning(&format!(
                "Git remote '{}' is not present; only the config entry is removed",
                m.remote
            ));
        } else {
            if !repo.is_managed(&m.remote) {
                return Err(CliError::usage(format!(
                    "remote '{}' was not created by git-replicate; refusing to remove it",
                    m.remote
                ))
                .hint(format!(
                    "Remove it yourself if desired: git remote remove {}",
                    m.remote
                )));
            }
            let cur = repo
                .single_push_url(&m.remote)
                .map_err(|e| CliError::usage(format!("remote '{}': {e}", m.remote)))?;
            if cur != m.url {
                return Err(CliError::usage(format!(
                    "remote '{}' now points to {}, not {}; refusing to remove it",
                    m.remote,
                    redact(&cur),
                    redact(&m.url)
                )));
            }
            if !yes
                && !(interactive(ctx)
                    && confirm(&format!(
                        "Remove Git remote '{}' ({})?",
                        m.remote,
                        redact(&cur)
                    )))
            {
                return Err(CliError::usage("removing a Git remote needs confirmation")
                    .hint("Re-run with --yes after checking the URL."));
            }
            drop_remote = true;
        }
    }
    cfg.mirrors.remove(idx);
    config::save(&repo.root, &cfg)?;
    let mut msg = format!("Mirror '{}' removed from {}", m.name, config::FILE);
    if drop_remote {
        match repo.remove_remote(&m.remote) {
            Ok(()) => msg.push_str(&format!("; Git remote '{}' removed", m.remote)),
            Err(e) => ctx.out.warning(&format!(
                "config updated, but removing remote '{}' failed: {e}",
                m.remote
            )),
        }
    } else {
        msg.push_str(&format!("; Git remote '{}' left in place", m.remote));
    }
    ctx.out.result(Final {
        status: "removed".into(),
        message: msg,
        ..Default::default()
    });
    Ok(0)
}
