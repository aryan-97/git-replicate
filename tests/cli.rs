use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_git-replicate");

struct Env {
    dir: TempDir,
    work: PathBuf,
}

struct Res {
    code: i32,
    out: String,
    err: String,
}

impl Res {
    fn json(&self) -> Vec<serde_json::Value> {
        self.out
            .lines()
            .map(|l| {
                serde_json::from_str(l).unwrap_or_else(|e| panic!("bad JSONL line {l:?}: {e}"))
            })
            .collect()
    }
    fn result(&self) -> serde_json::Value {
        self.json()
            .into_iter()
            .rfind(|v| v["event"] == "result")
            .expect("result event")
    }
}

fn base(env: &Env) -> PathBuf {
    env.dir.path().canonicalize().unwrap()
}

fn sh_run(dir: &Path, program: &str, args: &[&str], env: &[(&str, String)]) -> Res {
    let bin_dir = Path::new(BIN).parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(program);
    c.args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_REPLICATE_TIMEOUT", "30")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.org")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.org")
        .env_remove("NO_COLOR")
        .env_remove("GIT_REPLICATE_UILESS")
        .env_remove("GIT_REPLICATE_SKIP_HOOK")
        .stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    Res {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

impl Env {
    fn new() -> Env {
        let dir = TempDir::new().unwrap();
        let b = dir.path().canonicalize().unwrap();
        let ssh = b.join("fakessh.sh");
        fs::write(
            &ssh,
            "#!/bin/sh\neval \"cmd=\\${$#}\"\nexec sh -c \"$cmd\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let work = b.join("work dir");
        fs::create_dir_all(&work).unwrap();
        let e = Env { dir, work };
        e.git(&["init", "-q", "-b", "main"]);
        e
    }

    fn envs(&self) -> Vec<(&'static str, String)> {
        vec![(
            "GIT_SSH_COMMAND",
            self.dir
                .path()
                .canonicalize()
                .unwrap()
                .join("fakessh.sh")
                .display()
                .to_string(),
        )]
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Res {
        let r = sh_run(dir, "git", args, &self.envs());
        assert_eq!(r.code, 0, "git {args:?} failed: {}", r.err);
        r
    }

    fn git(&self, args: &[&str]) -> Res {
        self.git_in(&self.work, args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> Res {
        sh_run(dir, BIN, args, &self.envs())
    }

    fn run(&self, args: &[&str]) -> Res {
        self.run_in(&self.work, args)
    }

    fn bare(&self, name: &str) -> (PathBuf, String) {
        let p = base(self).join(format!("{name}.git"));
        self.git_in(&base(self), &["init", "-q", "--bare", p.to_str().unwrap()]);
        let url = format!("ssh://git@fake{}", p.display());
        (p, url)
    }

    fn reject(&self, bare: &Path, on: bool) {
        let h = bare.join("hooks/pre-receive");
        if on {
            fs::create_dir_all(bare.join("hooks")).unwrap();
            fs::write(&h, "#!/bin/sh\necho 'policy says no' >&2\nexit 1\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&h, fs::Permissions::from_mode(0o755)).unwrap();
            }
        } else {
            let _ = fs::remove_file(&h);
        }
    }

    fn commit(&self, msg: &str) -> String {
        fs::write(self.work.join("f.txt"), msg).unwrap();
        self.git(&["add", "f.txt"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["rev-parse", "HEAD"]).out.trim().to_string()
    }

    fn head(&self, bare: &Path, branch: &str) -> Option<String> {
        let r = sh_run(
            bare,
            "git",
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            &[],
        );
        (r.code == 0).then(|| r.out.trim().to_string())
    }

    fn has_ref(&self, bare: &Path, r: &str) -> bool {
        sh_run(bare, "git", &["rev-parse", "--verify", "--quiet", r], &[]).code == 0
    }
}

struct Topo {
    env: Env,
    primary: PathBuf,
    m1: PathBuf,
    m2: PathBuf,
}

fn topo() -> Topo {
    let env = Env::new();
    let (primary, purl) = env.bare("primary");
    let (m1, u1) = env.bare("mirror1");
    let (m2, u2) = env.bare("mirror2");
    env.git(&["remote", "add", "origin", &purl]);
    env.commit("one");
    assert_eq!(env.run(&["setup"]).code, 0);
    assert_eq!(env.run(&["mirror", "add", "one", "--url", &u1]).code, 0);
    assert_eq!(env.run(&["mirror", "add", "two", "--url", &u2]).code, 0);
    Topo {
        env,
        primary,
        m1,
        m2,
    }
}

fn snapshot(name: &str, text: &str) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !p.exists() {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
    }
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        text,
        "snapshot {name} differs (UPDATE_SNAPSHOTS=1 to refresh)"
    );
}

#[test]
fn help_works_outside_repo_and_is_snapshotted() {
    let d = TempDir::new().unwrap();
    let e = Env {
        dir: TempDir::new().unwrap(),
        work: d.path().to_path_buf(),
    };
    for (name, args) in [
        ("root", vec!["--help"]),
        ("root_uiless", vec!["--uiless", "--help"]),
        ("mirror_add", vec!["mirror", "add", "--help"]),
        ("hook_install", vec!["hook", "install", "--help"]),
        ("push", vec!["push", "--help"]),
        ("setup", vec!["help", "setup"]),
    ] {
        let r = e.run(&args);
        assert_eq!(r.code, 0, "{args:?}: {}", r.err);
        assert!(!r.out.contains('\u{1b}'));
        snapshot(&format!("help_{name}"), &r.out);
    }
    let r = e.run(&["--help"]);
    for s in [
        "NAME",
        "SYNOPSIS",
        "DESCRIPTION",
        "COMMANDS",
        "OPTIONS",
        "EXAMPLES",
        "EXIT STATUS",
        "CONFIGURATION FILE",
        "SECURITY",
    ] {
        assert!(
            r.out.contains(&format!("\n{s}\n")) || r.out.starts_with(&format!("{s}\n")),
            "missing {s}"
        );
    }
    assert_eq!(e.run(&["--version"]).code, 0);
    assert_eq!(
        e.run(&["--uiless", "--version"]).out.chars().next(),
        Some('g')
    );
}

#[test]
fn usage_errors() {
    let e = Env::new();
    let r = e.run(&["frobnicate"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("--help") || r.err.contains("Usage"));
    let r = e.run(&["--uiless", "push", "--bogus"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.result()["status"], "error");
    let r = e.run(&["mirror", "add", "x"]);
    assert_eq!(r.code, 2);
}

#[test]
fn outside_repo_and_special_repos() {
    let d = TempDir::new().unwrap();
    let e = Env {
        dir: TempDir::new().unwrap(),
        work: d.path().to_path_buf(),
    };
    let r = e.run(&["setup"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("not a Git repository"));
    assert!(fs::read_dir(d.path()).unwrap().next().is_none());
    let bare = d.path().join("b.git");
    e.git_in(d.path(), &["init", "-q", "--bare", bare.to_str().unwrap()]);
    let r = e.run_in(&bare, &["status"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("bare"));
}

#[test]
fn setup_is_idempotent_and_requires_remote() {
    let e = Env::new();
    let r = e.run(&["setup"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("no remotes"));
    let (_, purl) = e.bare("primary");
    e.git(&["remote", "add", "origin", &purl]);
    let r = e.run(&["--uiless", "setup"]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(r.result()["status"], "created");
    let cfg = fs::read_to_string(e.work.join(".gitreplicate")).unwrap();
    assert!(cfg.contains("version = 1") && cfg.contains(&purl));
    let r = e.run(&["--uiless", "setup"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.result()["status"], "unchanged");
    assert_eq!(
        fs::read_to_string(e.work.join(".gitreplicate")).unwrap(),
        cfg
    );
    let r = e.run(&["setup"]);
    assert!(r.out.contains("Already set up; nothing to do"));
    let sub = e.work.join("a/b");
    fs::create_dir_all(&sub).unwrap();
    let r = e.run_in(&sub, &["setup"]);
    assert!(r.out.contains("Already set up"));
    assert!(!sub.join(".gitreplicate").exists());
}

#[test]
fn setup_ambiguous_primary_and_bad_urls() {
    let e = Env::new();
    let (_, a) = e.bare("a");
    let (_, b) = e.bare("b");
    e.git(&["remote", "add", "up", &a]);
    e.git(&["remote", "add", "down", &b]);
    let r = e.run(&["--uiless", "setup"]);
    assert_eq!(r.code, 2);
    assert!(
        r.result()["message"]
            .as_str()
            .unwrap()
            .contains("ambiguous")
    );
    assert!(!e.work.join(".gitreplicate").exists());
    assert_eq!(
        e.run(&["--uiless", "setup", "--primary", "up", "--yes"])
            .code,
        0
    );
    assert!(
        fs::read_to_string(e.work.join(".gitreplicate"))
            .unwrap()
            .contains("remote = \"up\"")
    );

    let e2 = Env::new();
    e2.git(&[
        "remote",
        "add",
        "origin",
        "https://user:s3cr3t@example.org/x.git",
    ]);
    let r = e2.run(&["setup"]);
    assert_eq!(r.code, 2);
    assert!(!r.err.contains("s3cr3t") && !r.out.contains("s3cr3t"));
    assert!(!e2.work.join(".gitreplicate").exists());
}

#[test]
fn mirror_management_and_validation() {
    let t = topo();
    let e = &t.env;
    let cfg_before = fs::read_to_string(e.work.join(".gitreplicate")).unwrap();
    for bad in [
        "file:///tmp/x.git",
        "http://example.org/x.git",
        "git://example.org/x.git",
        "/tmp/x.git",
        "ext::sh -c id",
        "https://user:pw@example.org/x.git",
        "https://example.org/x.git#frag",
    ] {
        let r = e.run(&["mirror", "add", "bad", "--url", bad]);
        assert_eq!(r.code, 2, "{bad}");
        assert!(!r.err.contains(":pw@"));
    }
    assert_eq!(
        fs::read_to_string(e.work.join(".gitreplicate")).unwrap(),
        cfg_before
    );
    let r = e.run(&["mirror", "add", "one", "--url", "git@example.org:x/y.git"]);
    assert_eq!(r.code, 2);
    let cfg: String = cfg_before.clone();
    let dup = cfg
        .lines()
        .find(|l| l.contains("mirror1"))
        .unwrap()
        .split('"')
        .nth(1)
        .unwrap()
        .to_string();
    let r = e.run(&["mirror", "add", "three", "--url", &dup]);
    assert_eq!(r.code, 2, "duplicate destination");
    assert!(e.git(&["remote"]).out.contains("gitreplicate-one"));
    assert!(!e.git(&["remote"]).out.contains("gitreplicate-three"));
    let r = e.run(&["mirror", "add", "p", "--remote", "origin"]);
    assert_eq!(r.code, 2);
    e.git(&["remote", "add", "extra", "git@example.org:team/extra.git"]);
    assert_eq!(e.run(&["mirror", "add", "ex", "--remote", "extra"]).code, 0);
    let r = e.run(&["--uiless", "mirror", "list"]);
    assert_eq!(r.code, 0);
    assert_eq!(
        r.json()
            .iter()
            .filter(|v| v["event"] == "remote_result")
            .count(),
        3
    );
    let human = e.run(&["mirror", "list"]);
    assert!(human.out.contains("gitreplicate-one"));

    let r = e.run(&["mirror", "remove", "ex", "--remove-managed-remote", "--yes"]);
    assert_eq!(r.code, 2, "adopted remote is not tool-owned");
    assert_eq!(e.run(&["mirror", "remove", "ex"]).code, 0);
    assert!(e.git(&["remote"]).out.contains("extra"));
    assert_eq!(
        e.run(&[
            "mirror",
            "remove",
            "two",
            "--remove-managed-remote",
            "--yes"
        ])
        .code,
        0
    );
    assert!(!e.git(&["remote"]).out.contains("gitreplicate-two"));
    assert_eq!(e.run(&["mirror", "remove", "one"]).code, 0);
    assert!(e.git(&["remote"]).out.contains("gitreplicate-one"));
}

#[test]
fn config_validation() {
    let t = topo();
    let e = &t.env;
    let p = e.work.join(".gitreplicate");
    let good = fs::read_to_string(&p).unwrap();
    for (bad, why) in [
        (good.replace("version = 1", "version = 2"), "unsupported"),
        (format!("{good}\nunknown_policy = true\n"), "unknown"),
        (
            good.replace("[primary]", "[primary]\nextra = \"x\""),
            "unknown",
        ),
        ("version = 1\n[primary\n".to_string(), "invalid"),
        (good.replace("ssh://git@fake", "http://fake"), "http"),
    ] {
        fs::write(&p, &bad).unwrap();
        let r = e.run(&["push", "--dry-run"]);
        assert_eq!(r.code, 2, "{why}");
    }
    fs::write(&p, vec![b'#'; 70_000]).unwrap();
    assert_eq!(e.run(&["push", "--dry-run"]).code, 2);
    fs::remove_file(&p).unwrap();
    #[cfg(unix)]
    {
        fs::write(e.dir.path().join("real"), &good).unwrap();
        std::os::unix::fs::symlink(e.dir.path().join("real"), &p).unwrap();
        let r = e.run(&["push", "--dry-run"]);
        assert_eq!(r.code, 2);
        assert!(r.err.contains("symlink"));
    }
}

#[test]
fn new_clone_applies_local_setup() {
    let t = topo();
    let e = &t.env;
    fs::write(e.work.join(".gitignore"), "").unwrap();
    e.git(&["add", ".gitreplicate", ".gitignore"]);
    e.git(&["commit", "-q", "-m", "config"]);
    let clone = base(e).join("clone");
    e.git_in(
        &base(e),
        &[
            "clone",
            "-q",
            e.work.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let purl = e
        .git(&["remote", "get-url", "origin"])
        .out
        .trim()
        .to_string();
    e.git_in(&clone, &["remote", "set-url", "origin", &purl]);
    let r = e.run_in(&clone, &["--uiless", "setup"]);
    assert_eq!(r.code, 2, "needs --yes in automation");
    assert!(
        !e.git_in(&clone, &["remote"])
            .out
            .contains("gitreplicate-one")
    );
    let r = e.run_in(&clone, &["--uiless", "setup", "--yes"]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(r.result()["status"], "created");
    let remotes = e.git_in(&clone, &["remote"]).out;
    assert!(remotes.contains("gitreplicate-one") && remotes.contains("gitreplicate-two"));
    let r = e.run_in(&clone, &["--uiless", "setup"]);
    assert_eq!(r.result()["status"], "unchanged");
    let r = e.run_in(&clone, &["push", "--dry-run"]);
    assert_eq!(r.code, 0);
}

#[test]
fn push_success_jsonl_and_idempotent() {
    let t = topo();
    let e = &t.env;
    let oid = e.git(&["rev-parse", "HEAD"]).out.trim().to_string();
    let r = e.run(&["--uiless", "push"]);
    assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
    assert!(r.err.is_empty());
    for v in r.json() {
        assert_eq!(v["schema"], 1);
        for k in [
            "event",
            "operation",
            "remote",
            "branch",
            "oid",
            "status",
            "code",
            "message",
            "primary_status",
            "mirror_results",
            "partial",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
    }
    let res = r.result();
    assert_eq!(res["primary_status"], "ok");
    assert_eq!(res["partial"], false);
    assert_eq!(res["mirror_results"].as_array().unwrap().len(), 2);
    for b in [&t.primary, &t.m1, &t.m2] {
        assert_eq!(e.head(b, "main").as_deref(), Some(oid.as_str()));
    }
    assert!(!r.out.contains('\u{1b}'));
    let r = e.run(&["push"]);
    assert_eq!(r.code, 0);
    let r = e.run(&["status"]);
    assert_eq!(r.code, 0);
    assert!(r.out.contains("primary=ok"));
    let r = e.run(&["--uiless", "status", "--remote"]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.out.contains("matches local"));
    let lock = e.work.join(".git/gitreplicate/lock");
    assert!(!lock.exists());
    assert!(e.work.join(".git/gitreplicate/state.json").exists());
}

#[test]
fn push_nested_branch_and_explicit_branch() {
    let t = topo();
    let e = &t.env;
    e.git(&["checkout", "-q", "-b", "feature/x-y"]);
    let oid = e.commit("feat");
    e.git(&["checkout", "-q", "main"]);
    let r = e.run(&["push", "--branch", "feature/x-y"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(e.head(&t.m1, "feature/x-y").as_deref(), Some(oid.as_str()));
    assert!(e.head(&t.m1, "main").is_none());
    assert_eq!(e.run(&["push", "--branch", "nope"]).code, 2);
    assert_eq!(e.run(&["push", "--branch", "-x"]).code, 2);
    assert_eq!(e.run(&["push", "--branch", "a..b"]).code, 2);
}

#[test]
fn push_dry_run_is_local_only() {
    let t = topo();
    let e = &t.env;
    let r = e.run(&["--uiless", "push", "--dry-run"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json().iter().filter(|v| v["event"] == "plan").count(), 3);
    assert!(e.head(&t.primary, "main").is_none());
    assert!(!e.work.join(".git/gitreplicate/state.json").exists());
}

#[test]
fn primary_failure_skips_mirrors() {
    let t = topo();
    let e = &t.env;
    e.reject(&t.primary, true);
    let r = e.run(&["--uiless", "push"]);
    assert_eq!(r.code, 3, "{}", r.out);
    assert_eq!(r.result()["primary_status"], "failed");
    assert!(e.head(&t.m1, "main").is_none() && e.head(&t.m2, "main").is_none());
    let r = e.run(&["retry"]);
    assert_eq!(r.code, 2, "nothing to retry when primary failed");
}

#[test]
fn partial_failure_then_retry_without_force() {
    let t = topo();
    let e = &t.env;
    let oid = e.git(&["rev-parse", "HEAD"]).out.trim().to_string();
    e.reject(&t.m1, true);
    let r = e.run(&["--uiless", "push"]);
    assert_eq!(r.code, 4, "{}", r.out);
    let res = r.result();
    assert_eq!(res["partial"], true);
    let mr = res["mirror_results"].as_array().unwrap();
    assert_eq!(mr[0]["status"], "failed");
    assert_eq!(mr[1]["status"], "ok");
    assert_eq!(e.head(&t.primary, "main").as_deref(), Some(oid.as_str()));
    assert!(e.head(&t.m1, "main").is_none());
    assert_eq!(e.head(&t.m2, "main").as_deref(), Some(oid.as_str()));
    let r = e.run(&["--uiless", "retry"]);
    assert_eq!(r.code, 4);
    e.reject(&t.m1, false);
    e.commit("two");
    let r = e.run(&["retry"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(
        e.head(&t.m1, "main").as_deref(),
        Some(oid.as_str()),
        "retry pushes the recorded commit only"
    );
    assert_eq!(
        e.run(&["--uiless", "retry"]).result()["status"],
        "unchanged"
    );
}

#[test]
fn retry_refuses_when_primary_moved() {
    let t = topo();
    let e = &t.env;
    e.reject(&t.m2, true);
    assert_eq!(e.run(&["push"]).code, 4);
    e.reject(&t.m2, false);
    e.commit("two");
    e.git(&["push", "-q", "origin", "main"]);
    let r = e.run(&["retry"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("refusing to retry"));
    assert_eq!(e.run(&["push"]).code, 0);
    assert!(e.head(&t.m2, "main").is_some());
}

#[test]
fn non_fast_forward_is_never_forced() {
    let t = topo();
    let e = &t.env;
    assert_eq!(e.run(&["push"]).code, 0);
    let before = e.head(&t.m1, "main").unwrap();
    e.git(&["checkout", "-q", "--orphan", "other"]);
    let other = e.commit("unrelated");
    e.git(&["branch", "-f", "main", "HEAD"]);
    e.git(&["checkout", "-q", "main"]);
    let r = e.run(&["push"]);
    assert_eq!(r.code, 3, "primary rejects non-ff: {}", r.out);
    assert_eq!(e.head(&t.m1, "main").as_deref(), Some(before.as_str()));
    assert_ne!(other, before);
}

#[test]
fn url_drift_blocks_writes() {
    let t = topo();
    let e = &t.env;
    e.git(&[
        "config",
        "remote.gitreplicate-one.pushurl",
        "ssh://git@fake/elsewhere.git",
    ]);
    let r = e.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("push URL changed"));
    assert!(e.head(&t.primary, "main").is_none());
    e.git(&["config", "--unset", "remote.gitreplicate-one.pushurl"]);
    e.git(&["remote", "remove", "gitreplicate-two"]);
    let r = e.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("missing"));
    assert!(e.head(&t.primary, "main").is_none());
    let r = e.run(&["doctor"]);
    assert_eq!(r.code, 2);
}

#[test]
fn detached_and_unborn_head() {
    let t = topo();
    let e = &t.env;
    e.git(&["checkout", "-q", "--detach"]);
    let r = e.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("detached"));
    assert_eq!(e.run(&["push", "--branch", "main"]).code, 0);

    let e2 = Env::new();
    let (_, purl) = e2.bare("p");
    e2.git(&["remote", "add", "origin", &purl]);
    assert_eq!(e2.run(&["setup"]).code, 0);
    let r = e2.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("no commits"));
}

#[test]
fn linked_worktree_shares_state_dir() {
    let t = topo();
    let e = &t.env;
    let wt = base(e).join("wt");
    e.git(&["worktree", "add", "-q", "-b", "wtb", wt.to_str().unwrap()]);
    fs::write(wt.join("g.txt"), "g").unwrap();
    e.git_in(&wt, &["add", "g.txt"]);
    e.git_in(&wt, &["commit", "-q", "-m", "g"]);
    fs::copy(e.work.join(".gitreplicate"), wt.join(".gitreplicate")).unwrap();
    let r = e.run_in(&wt, &["push"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(e.work.join(".git/gitreplicate/state.json").exists());
}

#[test]
fn stale_lock_is_recovered_and_live_lock_blocks() {
    let t = topo();
    let e = &t.env;
    let dir = e.work.join(".git/gitreplicate");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("lock"), "999999\n1\n").unwrap();
    assert_eq!(e.run(&["push"]).code, 0);
    fs::write(
        dir.join("lock"),
        format!("{}\n{}\n", std::process::id(), 4_000_000_000u64),
    )
    .unwrap();
    let r = e.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("lock"));
    assert!(dir.join("lock").exists());
}

#[test]
fn corrupt_state_is_reported() {
    let t = topo();
    let e = &t.env;
    let dir = e.work.join(".git/gitreplicate");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("state.json"), "{not json").unwrap();
    let r = e.run(&["push"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("state file"));
    assert!(e.head(&t.primary, "main").is_none());
}

#[test]
fn hook_install_and_conflicts() {
    let t = topo();
    let e = &t.env;
    let hook = e.work.join(".git/hooks/pre-push");
    assert_eq!(e.run(&["hook", "status"]).code, 0);
    let r = e.run(&["hook", "install", "pre-push"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.err.contains("no post-push hook"));
    assert!(hook.exists());
    assert_eq!(
        e.run(&["--uiless", "hook", "install", "pre-push"]).result()["status"],
        "unchanged"
    );
    assert_eq!(e.run(&["hook", "install", "pre-push", "--strict"]).code, 5);
    let orig = fs::read(&hook).unwrap();
    fs::write(&hook, [orig.clone(), b"# edited\n".to_vec()].concat()).unwrap();
    assert_eq!(e.run(&["hook", "uninstall", "pre-push"]).code, 5);
    assert!(hook.exists());
    fs::write(&hook, "#!/bin/sh\necho mine\n").unwrap();
    let r = e.run(&["hook", "install", "pre-push"]);
    assert_eq!(r.code, 5);
    assert!(r.err.contains("hook run pre-push"));
    assert_eq!(fs::read_to_string(&hook).unwrap(), "#!/bin/sh\necho mine\n");
    fs::write(&hook, &orig).unwrap();
    assert_eq!(e.run(&["hook", "uninstall", "pre-push"]).code, 0);
    assert!(!hook.exists());
    assert_eq!(e.run(&["hook", "uninstall", "pre-push"]).code, 0);

    e.git(&["config", "core.hooksPath", "myhooks"]);
    assert_eq!(e.run(&["hook", "install", "pre-push"]).code, 0);
    assert!(e.work.join("myhooks/pre-push").exists());
    assert!(!hook.exists());
    assert_eq!(e.run(&["hook", "uninstall", "pre-push"]).code, 0);
    e.git(&[
        "config",
        "core.hooksPath",
        base(e).join("outside").to_str().unwrap(),
    ]);
    assert_eq!(e.run(&["hook", "install", "pre-push"]).code, 5);
}

#[test]
fn hook_mirrors_ordinary_push_and_respects_policy() {
    let t = topo();
    let e = &t.env;
    assert_eq!(e.run(&["hook", "install", "pre-push"]).code, 0);
    let oid = e.git(&["rev-parse", "HEAD"]).out.trim().to_string();
    e.git(&["push", "-q", "origin", "main"]);
    assert_eq!(e.head(&t.m1, "main").as_deref(), Some(oid.as_str()));
    assert_eq!(e.head(&t.m2, "main").as_deref(), Some(oid.as_str()));
    let st: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(e.work.join(".git/gitreplicate/state.json")).unwrap(),
    )
    .unwrap();
    assert!(st["branches"]["main"]["wrapper"].is_null());
    assert_eq!(st["branches"]["main"]["hook"]["primary"], "not_recorded");

    e.git(&["tag", "v1"]);
    e.git(&["push", "-q", "origin", "v1"]);
    assert!(!e.has_ref(&t.m1, "refs/tags/v1"));
    e.git(&["push", "-q", "origin", "--delete", "main"]);
    assert!(
        e.head(&t.m1, "main").is_some(),
        "deletions are never mirrored"
    );

    let oid2 = e.commit("two");
    e.git(&["checkout", "-q", "-b", "side"]);
    let oid3 = e.commit("three");
    e.git(&["push", "-q", "origin", "main", "side:renamed"]);
    assert_eq!(e.head(&t.m1, "main").as_deref(), Some(oid2.as_str()));
    assert_eq!(e.head(&t.m2, "renamed").as_deref(), Some(oid3.as_str()));
    assert!(e.head(&t.m2, "side").is_none());

    let oid4 = e.commit("four");
    e.git(&["push", "-q", "--no-verify", "origin", "side"]);
    assert!(
        e.head(&t.m1, "side").is_none(),
        "bypassed hook does not mirror"
    );
    assert_ne!(oid4, oid3);

    e.reject(&t.m1, true);
    e.commit("five");
    let r = e.git(&["push", "origin", "side"]);
    assert!(r.err.contains("mirror update failed"));
    assert!(
        e.head(&t.primary, "side").is_some(),
        "best-effort lets the primary push continue"
    );

    assert_eq!(e.run(&["hook", "uninstall", "pre-push"]).code, 0);
    assert_eq!(e.run(&["hook", "install", "pre-push", "--strict"]).code, 0);
    e.commit("six");
    let before = e.head(&t.primary, "side");
    let r = sh_run(&e.work, "git", &["push", "origin", "side"], &e.envs());
    assert_ne!(r.code, 0, "strict hook blocks the primary push");
    assert_eq!(e.head(&t.primary, "side"), before);
    assert!(r.err.contains("mirror"));
}

#[test]
fn hook_skips_other_remotes_and_recursion_guard() {
    let t = topo();
    let e = &t.env;
    assert_eq!(e.run(&["hook", "install", "pre-push"]).code, 0);
    let (other, ourl) = e.bare("other");
    e.git(&["remote", "add", "other", &ourl]);
    let r = e.git(&["push", "other", "main"]);
    assert!(format!("{}{}", r.out, r.err).contains("not the configured primary"));
    assert!(e.head(&other, "main").is_some());
    assert!(e.head(&t.m1, "main").is_none());
    let mut env = e.envs();
    env.push(("GIT_REPLICATE_SKIP_HOOK", "1".into()));
    let r = sh_run(&e.work, "git", &["push", "origin", "main"], &env);
    assert_eq!(r.code, 0);
    assert!(e.head(&t.m1, "main").is_none());
    let r = sh_run(
        &e.work,
        BIN,
        &["hook", "run", "pre-push", "origin", "x"],
        &env,
    );
    assert_eq!(r.code, 0);
    assert!(r.out.is_empty());
}

#[test]
fn hook_run_uiless_jsonl_for_delegation() {
    let t = topo();
    let e = &t.env;
    let oid = e.git(&["rev-parse", "HEAD"]).out.trim().to_string();
    let purl = e
        .git(&["remote", "get-url", "origin"])
        .out
        .trim()
        .to_string();
    let input = format!(
        "refs/heads/main {oid} refs/heads/main {}\nrefs/tags/t {oid} refs/tags/t {}\n",
        "0".repeat(40),
        "0".repeat(40)
    );
    let mut c = Command::new(BIN);
    c.args(["--uiless", "hook", "run", "pre-push", "origin", &purl])
        .current_dir(&e.work)
        .env("GIT_SSH_COMMAND", e.envs()[0].1.clone())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    let last: serde_json::Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"], "result");
    assert_eq!(last["primary_status"], "not_recorded");
    assert_eq!(e.head(&t.m1, "main").as_deref(), Some(oid.as_str()));
}

#[test]
fn doctor_and_status_remote_checks() {
    let t = topo();
    let e = &t.env;
    let r = e.run(&["doctor"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("Trust boundary"));
    let r = e.run(&["--uiless", "doctor", "--check-remote"]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(
        r.json()
            .iter()
            .filter(|v| v["event"] == "check"
                && v["status"] == "ok"
                && v["message"].as_str().unwrap().contains("ls-remote"))
            .count(),
        3
    );
    fs::remove_dir_all(&t.m2).unwrap();
    let r = e.run(&["doctor", "--check-remote"]);
    assert_eq!(r.code, 6, "{}", r.out);
    assert!(r.out.contains("ssh-add") || r.out.contains("SSH"));
    let r = e.run(&["status", "--remote"]);
    assert_eq!(r.code, 6);
}

#[test]
fn secrets_never_appear_in_output() {
    let t = topo();
    let e = &t.env;
    e.git(&[
        "remote",
        "add",
        "weird",
        "https://alice:hunter2@example.org/x.git",
    ]);
    let r = e.run(&["mirror", "add", "w", "--remote", "weird"]);
    assert_eq!(r.code, 2);
    assert!(!r.out.contains("hunter2") && !r.err.contains("hunter2"));
    e.git(&[
        "config",
        "remote.origin.pushurl",
        "https://alice:hunter2@example.org/x.git",
    ]);
    for args in [
        vec!["push"],
        vec!["status"],
        vec!["doctor"],
        vec!["--uiless", "push"],
    ] {
        let r = e.run(&args);
        assert!(
            !r.out.contains("hunter2") && !r.err.contains("hunter2"),
            "{args:?}"
        );
    }
}
