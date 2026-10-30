use crate::Ctx;
use crate::config;
use crate::out::{CliError, Final, R, RemoteRes};
use crate::push::push_ref;
use crate::repo::{self, Repo};
use crate::state::{self, Attempt, LOCK_WAIT, Lock, St};
use crate::util::{is_hex_oid, is_zero_oid, refuse_symlink, short};
use std::collections::BTreeMap;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

const MARKER: &str = "# git-replicate-hook: v1";
const EVENT: &str = "pre-push";

pub fn script(strict: bool) -> String {
    let (mode, flag, miss) = if strict {
        ("strict", " --strict", 1)
    } else {
        ("best-effort", "", 0)
    };
    format!(
        "#!/bin/sh\n\
{MARKER} {EVENT} {mode}\n\
# Managed by git-replicate. Remove with: git-replicate hook uninstall {EVENT}\n\
if [ -n \"$GIT_REPLICATE_SKIP_HOOK\" ]; then\n  exit 0\nfi\n\
if ! command -v git-replicate >/dev/null 2>&1; then\n  \
echo \"git-replicate: binary not found in PATH; mirrors were NOT updated.\" >&2\n  \
echo \"git-replicate: install it again, or delete this hook file to stop seeing this message.\" >&2\n  \
exit {miss}\nfi\n\
exec git-replicate hook run {EVENT}{flag} \"$@\"\n"
    )
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HookState {
    Absent,
    Owned { strict: bool },
    Modified,
    Foreign,
}

pub struct HookInfo {
    pub dir: PathBuf,
    pub path: PathBuf,
    pub state: HookState,
    pub custom_path: bool,
    pub external: bool,
}

fn canon_lenient(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = cur.canonicalize() {
            return rest.iter().rev().fold(c, |a, r| a.join(r));
        }
        match (
            cur.file_name().map(|n| n.to_owned()),
            cur.parent().map(|p| p.to_path_buf()),
        ) {
            (Some(n), Some(par)) => {
                rest.push(n);
                cur = par;
            }
            _ => return p.to_path_buf(),
        }
    }
}

pub fn inspect(repo: &Repo) -> HookInfo {
    let custom = repo
        .text(&["config", "--path", "--get", "core.hooksPath"])
        .ok()
        .filter(|s| !s.is_empty());
    let dir = match &custom {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                repo.root.join(p)
            }
        }
        None => repo.common_dir.join("hooks"),
    };
    let path = dir.join(EVENT);
    let d = canon_lenient(&dir);
    let inside = [&repo.root, &repo.common_dir]
        .iter()
        .any(|b| d.starts_with(canon_lenient(b)));
    let state = match fs::symlink_metadata(&path) {
        Err(_) => HookState::Absent,
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => HookState::Foreign,
        Ok(_) => match fs::read(&path) {
            Ok(b) if b == script(false).as_bytes() => HookState::Owned { strict: false },
            Ok(b) if b == script(true).as_bytes() => HookState::Owned { strict: true },
            Ok(b) if String::from_utf8_lossy(&b).contains(MARKER) => HookState::Modified,
            _ => HookState::Foreign,
        },
    };
    HookInfo {
        dir,
        path,
        state,
        custom_path: custom.is_some(),
        external: !inside,
    }
}

const DELEGATE: &str = "To delegate manually, add this line to your existing hook (Git passes the remote name and URL as arguments and the ref updates on stdin, which `hook run` reads):\n    git-replicate hook run pre-push \"$@\"\nIf your hook also reads stdin, save it first: input=$(cat); printf '%s\\n' \"$input\" | git-replicate hook run pre-push \"$@\"";

fn conflict(msg: String, hint: &str) -> CliError {
    CliError::new(5, msg).hint(hint.to_string())
}

pub fn install(ctx: &Ctx, strict: bool) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    config::load(&repo.root)?;
    let out = &ctx.out;
    let info = inspect(&repo);
    out.warning("Git has no post-push hook: this pre-push hook runs BEFORE the primary push and cannot know whether it will succeed, so mirrors may end up ahead of the primary. Hooks are local to this clone and are skipped by `git push --no-verify`. `git-replicate push` is the recommended way to keep ordering.");
    if strict {
        out.warning("--strict blocks the primary push if a mirror fails, but mirrors may already have changed.");
    }
    if info.external {
        return Err(conflict(
            format!(
                "core.hooksPath ({}) is outside this repository; refusing to write there",
                info.dir.display()
            ),
            DELEGATE,
        ));
    }
    if let Ok(m) = fs::symlink_metadata(&info.dir)
        && m.file_type().is_symlink()
    {
        return Err(conflict(
            format!(
                "hooks directory {} is a symlink; refusing to write through it",
                info.dir.display()
            ),
            DELEGATE,
        ));
    }
    match info.state {
        HookState::Owned { strict: s } if s == strict => {
            out.result(Final {
                status: "unchanged".into(),
                message: format!("{EVENT} hook already installed ({})", info.path.display()),
                ..Default::default()
            });
            Ok(0)
        }
        HookState::Owned { .. } => Err(conflict(
            format!(
                "a tool-owned {EVENT} hook with the other failure policy is installed at {}",
                info.path.display()
            ),
            &format!("Run `git-replicate hook uninstall {EVENT}` first, then install again."),
        )),
        HookState::Absent => {
            refuse_symlink(&info.path).map_err(|e| conflict(e.to_string(), DELEGATE))?;
            fs::create_dir_all(&info.dir).map_err(|e| {
                conflict(
                    format!("could not create {}: {e}", info.dir.display()),
                    DELEGATE,
                )
            })?;
            let mut f = crate::util::create_new(&info.path, 0o755).map_err(|e| {
                conflict(
                    format!("could not create {}: {e}", info.path.display()),
                    DELEGATE,
                )
            })?;
            f.write_all(script(strict).as_bytes())
                .map_err(|e| conflict(format!("could not write hook: {e}"), DELEGATE))?;
            drop(f);
            out.result(Final {
                status: "installed".into(),
                message: format!(
                    "installed {} {EVENT} hook at {}",
                    if strict { "strict" } else { "best-effort" },
                    info.path.display()
                ),
                ..Default::default()
            });
            Ok(0)
        }
        HookState::Modified | HookState::Foreign => Err(conflict(
            format!(
                "an existing {EVENT} hook at {} was not created (or was modified) by git-replicate; it was not touched",
                info.path.display()
            ),
            DELEGATE,
        )),
    }
}

pub fn uninstall(ctx: &Ctx) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let info = inspect(&repo);
    match info.state {
        HookState::Absent => {
            ctx.out.result(Final {
                status: "unchanged".into(),
                message: format!("no {EVENT} hook installed"),
                ..Default::default()
            });
            Ok(0)
        }
        HookState::Owned { .. } => {
            fs::remove_file(&info.path).map_err(|e| {
                conflict(format!("could not remove {}: {e}", info.path.display()), "")
            })?;
            ctx.out.result(Final {
                status: "removed".into(),
                message: format!("removed {}", info.path.display()),
                ..Default::default()
            });
            Ok(0)
        }
        _ => Err(conflict(
            format!(
                "{} is not an unmodified git-replicate hook; refusing to remove it",
                info.path.display()
            ),
            "Edit it manually and remove the git-replicate delegation line if you added one.",
        )),
    }
}

pub fn status(ctx: &Ctx) -> R<i32> {
    let repo = repo::discover(&ctx.cwd)?;
    let info = inspect(&repo);
    let (st, msg) = match info.state {
        HookState::Absent => (
            "unchanged",
            "not installed (ordinary `git push` is NOT mirrored)".to_string(),
        ),
        HookState::Owned { strict } => (
            "ok",
            format!(
                "installed ({})",
                if strict { "strict" } else { "best-effort" }
            ),
        ),
        HookState::Modified => (
            "warn",
            "tool marker present but the hook was modified; not removable by uninstall".into(),
        ),
        HookState::Foreign => (
            "warn",
            "a different hook exists; use manual delegation".into(),
        ),
    };
    ctx.out.info(&format!(
        "Hook directory: {}{}",
        info.dir.display(),
        if info.custom_path {
            " (core.hooksPath)"
        } else {
            ""
        }
    ));
    ctx.out.check(None, st, &format!("{EVENT} hook: {msg}"));
    ctx.out.result(Final {
        status: "ok".into(),
        message: format!("{EVENT}: {msg}"),
        ..Default::default()
    });
    Ok(0)
}

fn soft(ctx: &Ctx, strict: bool, code: i32, msg: String) -> R<i32> {
    ctx.out.warning(&msg);
    let c = if strict { code } else { 0 };
    ctx.out.result(Final {
        status: if strict { "error" } else { "skipped" }.into(),
        code: c,
        message: msg,
        ..Default::default()
    });
    Ok(c)
}

fn skipped(ctx: &Ctx, msg: &str) -> R<i32> {
    ctx.out.info(&format!("git-replicate: {msg}"));
    ctx.out.result(Final {
        status: "skipped".into(),
        message: msg.into(),
        ..Default::default()
    });
    Ok(0)
}

pub fn run(ctx: &Ctx, strict: bool, args: Vec<String>) -> R<i32> {
    if std::env::var_os("GIT_REPLICATE_SKIP_HOOK")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(0);
    }
    let out = &ctx.out;
    let mut input = String::new();
    if !std::io::stdin().is_terminal() {
        let mut buf = Vec::new();
        let _ = std::io::stdin().take(1 << 20).read_to_end(&mut buf);
        input = String::from_utf8_lossy(&buf).into_owned();
    }
    let repo = match repo::discover(&ctx.cwd) {
        Ok(r) => r,
        Err(e) => return soft(ctx, strict, 2, format!("mirroring skipped: {}", e.msg)),
    };
    let cfg = match config::load(&repo.root) {
        Ok(c) => c,
        Err(e) => return soft(ctx, strict, 2, format!("mirroring skipped: {}", e.msg)),
    };
    let remote = args.first().map(String::as_str).unwrap_or("");
    if remote != cfg.primary.remote {
        return skipped(
            ctx,
            &format!(
                "push to '{}' is not the configured primary '{}'; not mirrored",
                crate::util::redact(remote),
                cfg.primary.remote
            ),
        );
    }
    if let Err(e) = repo::check_remote(
        &repo,
        &repo.remotes()?,
        &cfg.primary.remote,
        &cfg.primary.url,
    ) {
        return soft(ctx, strict, 2, format!("mirroring skipped: primary: {e}"));
    }
    if let Some(u) = args.get(1)
        && *u != cfg.primary.url
    {
        return soft(
            ctx,
            strict,
            2,
            "mirroring skipped: the push URL Git reported differs from .gitreplicate".into(),
        );
    }
    let mut eligible: BTreeMap<String, String> = BTreeMap::new();
    for line in input.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 4 {
            out.warning("ignoring malformed pre-push line");
            continue;
        }
        let (lref, loid, rref) = (f[0], f[1], f[2]);
        if is_zero_oid(loid) {
            out.info(&format!("git-replicate: skipping deletion of {rref}"));
        } else if rref.starts_with("refs/tags/") || lref.starts_with("refs/tags/") {
            out.info(&format!("git-replicate: skipping tag {rref}"));
        } else if !(lref.starts_with("refs/heads/") || lref == "HEAD")
            || !rref.starts_with("refs/heads/")
        {
            out.info(&format!("git-replicate: skipping non-branch ref {rref}"));
        } else if !is_hex_oid(loid) || !repo.valid_ref(rref) {
            out.info(&format!(
                "git-replicate: skipping invalid ref update for {}",
                crate::util::redact(rref)
            ));
        } else if eligible.contains_key(rref) {
            out.warning(&format!("duplicate update for {rref}; using the first"));
        } else {
            eligible.insert(rref.to_string(), loid.to_string());
        }
    }
    if eligible.is_empty() {
        return skipped(ctx, "no eligible branch updates to mirror");
    }
    if cfg.mirrors.is_empty() {
        return skipped(ctx, "no mirrors configured");
    }
    match repo::dest_problems(&repo, &cfg) {
        Ok(p) if p.is_empty() => {}
        Ok(p) => {
            let m = p
                .iter()
                .map(|(r, e)| format!("{r}: {e}"))
                .collect::<Vec<_>>()
                .join("; ");
            return soft(ctx, strict, 2, format!("mirroring skipped: {m}"));
        }
        Err(e) => return soft(ctx, strict, 2, e.msg),
    }
    let _lock = match Lock::acquire(&repo, LOCK_WAIT) {
        Ok(l) => l,
        Err(e) => return soft(ctx, strict, 2, format!("mirroring skipped: {}", e.msg)),
    };
    let mut st = match state::load(&repo) {
        Ok(s) => Some(s),
        Err(e) => {
            out.warning(&format!("attempt will not be recorded: {}", e.msg));
            None
        }
    };
    let mut failed = Vec::new();
    let mut all_results = Vec::new();
    for (rref, oid) in &eligible {
        let branch = rref.trim_start_matches("refs/heads/");
        let mut att = Attempt::new(branch, oid, St::NotRecorded);
        for m in &cfg.mirrors {
            out.info(&format!(
                "git-replicate: mirroring {} to '{}'...",
                short(oid),
                m.remote
            ));
            let p = push_ref(&repo, &m.remote, oid, rref);
            out.detail(&p.raw);
            att.set(&m.remote, p.st, Some(p.msg.clone()));
            out.remote_result(&m.remote, Some(branch), Some(oid), p.st.as_str(), &p.msg);
            if p.st != St::Ok {
                failed.push(format!("{}:{branch}", m.remote));
            }
            all_results.push(RemoteRes {
                remote: format!("{}:{branch}", m.remote),
                status: p.st.as_str().into(),
                message: Some(p.msg),
            });
        }
        if let Some(s) = st.as_mut()
            && let Err(e) = state::record(&repo, s, &att, true)
        {
            out.warning(&format!("could not record hook attempt: {e}"));
        }
    }
    let branch = (eligible.len() == 1).then(|| {
        eligible
            .keys()
            .next()
            .unwrap()
            .trim_start_matches("refs/heads/")
            .to_string()
    });
    let oid = (eligible.len() == 1).then(|| eligible.values().next().unwrap().clone());
    if failed.is_empty() {
        out.result(Final {
            status: "ok".into(),
            message: "mirrors updated before the primary push".into(),
            branch,
            oid,
            primary_status: Some("not_recorded".into()),
            mirror_results: Some(all_results),
            partial: Some(false),
            ..Default::default()
        });
        return Ok(0);
    }
    let msg = format!(
        "mirror update failed for {}. {}",
        failed.join(", "),
        if strict {
            "Blocking the primary push (--strict); other mirrors may already have changed."
        } else {
            "Continuing the primary push (best-effort); mirrors may now be out of sync."
        }
    );
    out.warning(&msg);
    let code = if strict { 4 } else { 0 };
    out.result(Final {
        status: "partial".into(),
        code,
        message: msg,
        branch,
        oid,
        primary_status: Some("not_recorded".into()),
        mirror_results: Some(all_results),
        partial: Some(true),
    });
    Ok(code)
}
