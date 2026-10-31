use crate::Ctx;
use crate::config::{self, Config};
use crate::out::{CliError, Final, R};
use crate::repo;

pub fn parse_patterns(input: &str) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = Vec::new();
    for p in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
    {
        config::validate_pattern(p)?;
        if !v.iter().any(|x| x == p) {
            v.push(p.to_string());
        }
    }
    Ok(v)
}

pub fn denied(cfg: &Config, branches: &[String]) -> CliError {
    let list = branches
        .iter()
        .map(|b| format!("'{b}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let (noun, verb) = if branches.len() == 1 {
        ("branch", "is")
    } else {
        ("branches", "are")
    };
    let why = if cfg.branches.is_empty() {
        "the allowlist is empty".to_string()
    } else {
        format!("allowed: {}", cfg.branches.join(", "))
    };
    let cmds = branches
        .iter()
        .map(|b| format!("  git-replicate allowlist add '{b}'"))
        .collect::<Vec<_>>()
        .join("\n");
    CliError::usage(format!(
        "{noun} {list} {verb} not in the replication allowlist ({why}); nothing was pushed or mirrored"
    ))
    .hint(format!(
        "If this branch really should be published to every mirror, run this yourself:\n{cmds}\nSee `git-replicate allowlist list`."
    ))
}

pub fn enforce(cfg: &Config, branch: &str) -> R<()> {
    match config::allowed_by(cfg, branch) {
        Some(_) => Ok(()),
        None => Err(denied(cfg, &[branch.to_string()])),
    }
}

fn broad(p: &str) -> bool {
    p.trim_matches('*').is_empty()
}

pub fn add(ctx: &Ctx, patterns: Vec<String>) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let mut cfg = config::load(&repo.root)?;
    for p in &patterns {
        config::validate_pattern(p).map_err(CliError::usage)?;
    }
    let mut added: Vec<String> = Vec::new();
    for p in patterns {
        if cfg.branches.contains(&p) || added.contains(&p) {
            ctx.out
                .warning(&format!("'{p}' is already in the allowlist"));
        } else {
            added.push(p);
        }
    }
    if added.is_empty() {
        ctx.out.result(Final {
            status: "unchanged".into(),
            message: "allowlist unchanged".into(),
            ..Default::default()
        });
        return Ok(0);
    }
    cfg.branches.extend(added.iter().cloned());
    config::save(&repo.root, &cfg)?;
    for p in &added {
        if broad(p) {
            ctx.out.warning(&format!(
                "'{p}' matches (nearly) every branch; consider a narrower pattern"
            ));
        }
        ctx.out.check(None, "ok", &format!("added '{p}'"));
    }
    ctx.out.result(Final {
        status: "created".into(),
        message: format!(
            "added {} pattern(s); allowlist is now: {}",
            added.len(),
            cfg.branches.join(", ")
        ),
        ..Default::default()
    });
    Ok(0)
}

pub fn remove(ctx: &Ctx, patterns: Vec<String>) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let mut cfg = config::load(&repo.root)?;
    for p in &patterns {
        if !cfg.branches.contains(p) {
            return Err(CliError::usage(format!(
                "'{p}' is not in the allowlist; nothing was changed"
            ))
            .hint("See `git-replicate allowlist list` (patterns are matched literally here)."));
        }
    }
    cfg.branches.retain(|b| !patterns.contains(b));
    config::save(&repo.root, &cfg)?;
    for p in &patterns {
        ctx.out.check(None, "ok", &format!("removed '{p}'"));
    }
    if cfg.branches.is_empty() {
        ctx.out.warning(
            "the allowlist is now empty: every push will be denied until you add a branch",
        );
    }
    ctx.out.result(Final {
        status: "removed".into(),
        message: format!("removed {} pattern(s)", patterns.len()),
        ..Default::default()
    });
    Ok(0)
}

pub fn list(ctx: &Ctx) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    let cur = repo.current_branch();
    ctx.out.heading(&format!(
        "Branch allowlist ({} pattern(s)); `*` matches within one path segment, `**` across '/'",
        cfg.branches.len()
    ));
    for p in &cfg.branches {
        ctx.out.check(None, "ok", p);
    }
    if cfg.branches.is_empty() {
        ctx.out.warning("the allowlist is empty: every push is denied. Add a branch with `git-replicate allowlist add BRANCH`");
    }
    if let Some(b) = cur {
        let m = config::allowed_by(&cfg, &b);
        ctx.out.info(&format!(
            "Current branch '{b}': {}",
            match m {
                Some(p) => format!("allowed (pattern '{p}')"),
                None => "NOT allowed".to_string(),
            }
        ));
    }
    ctx.out.result(Final {
        status: "ok".into(),
        message: format!("{} pattern(s)", cfg.branches.len()),
        ..Default::default()
    });
    Ok(0)
}

pub fn check(ctx: &Ctx, branch: &str) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let cfg = config::load(&repo.root)?;
    if !repo.valid_branch_name(branch) {
        return Err(CliError::usage(format!(
            "'{branch}' is not a valid branch name"
        )));
    }
    let p = config::allowed_by(&cfg, branch).ok_or_else(|| denied(&cfg, &[branch.to_string()]))?;
    ctx.out.result(Final {
        status: "ok".into(),
        message: format!("branch '{branch}' is allowed (pattern '{p}')"),
        branch: Some(branch.to_string()),
        ..Default::default()
    });
    Ok(0)
}
