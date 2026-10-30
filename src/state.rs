use crate::out::{CliError, R};
use crate::repo::Repo;
use crate::util::{create_new, iso, now_unix, refuse_symlink, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SCHEMA: u32 = 1;
const STALE_SECS: u64 = 6 * 3600;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum St {
    Ok,
    Failed,
    Unknown,
    Pending,
    Skipped,
    NotRecorded,
}

impl St {
    pub fn as_str(self) -> &'static str {
        match self {
            St::Ok => "ok",
            St::Failed => "failed",
            St::Unknown => "unknown",
            St::Pending => "pending",
            St::Skipped => "skipped",
            St::NotRecorded => "not_recorded",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MirrorState {
    pub status: St,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Attempt {
    pub branch: String,
    pub oid: String,
    pub primary: St,
    pub mirrors: BTreeMap<String, MirrorState>,
    pub timestamp: String,
    pub unix: u64,
}

impl Attempt {
    pub fn new(branch: &str, oid: &str, primary: St) -> Attempt {
        let unix = now_unix();
        Attempt {
            branch: branch.into(),
            oid: oid.into(),
            primary,
            mirrors: BTreeMap::new(),
            timestamp: iso(unix),
            unix,
        }
    }
    pub fn set(&mut self, mirror: &str, status: St, message: Option<String>) {
        self.mirrors
            .insert(mirror.into(), MirrorState { status, message });
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct BranchState {
    pub wrapper: Option<Attempt>,
    pub hook: Option<Attempt>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct State {
    pub schema: u32,
    #[serde(default)]
    pub branches: BTreeMap<String, BranchState>,
}

impl Default for State {
    fn default() -> Self {
        State {
            schema: SCHEMA,
            branches: BTreeMap::new(),
        }
    }
}

pub fn dir(repo: &Repo) -> PathBuf {
    repo.common_dir.join("gitreplicate")
}

fn ensure_dir(repo: &Repo) -> R<PathBuf> {
    let d = dir(repo);
    refuse_symlink(&d)?;
    if !d.exists() {
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&d)
            .map_err(|e| CliError::usage(format!("could not create {}: {e}", d.display())))?;
    }
    Ok(d)
}

pub fn state_path(repo: &Repo) -> PathBuf {
    dir(repo).join("state.json")
}

pub fn load(repo: &Repo) -> R<State> {
    let p = state_path(repo);
    match fs::symlink_metadata(&p) {
        Err(_) => return Ok(State::default()),
        Ok(m) if m.file_type().is_symlink() || !m.is_file() || m.len() > 1 << 20 => {
            return Err(corrupt(&p, "unexpected file type or size"));
        }
        Ok(_) => {}
    }
    let s = fs::read_to_string(&p).map_err(|e| corrupt(&p, &e.to_string()))?;
    let st: State = serde_json::from_str(&s).map_err(|e| corrupt(&p, &e.to_string()))?;
    if st.schema != SCHEMA {
        return Err(corrupt(&p, &format!("unsupported schema {}", st.schema)));
    }
    Ok(st)
}

fn corrupt(p: &Path, why: &str) -> CliError {
    CliError::usage(format!("state file {} is unreadable: {why}", p.display())).hint(format!(
        "Inspect it with `git-replicate status --remote`, then move or delete {} to reset recorded attempts (remotes are not affected).",
        p.display()
    ))
}

pub fn save(repo: &Repo, st: &State) -> Result<(), String> {
    let d = ensure_dir(repo).map_err(|e| e.msg)?;
    let data = serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?;
    write_atomic(&d.join("state.json"), &data, 0o600).map_err(|e| e.to_string())
}

pub fn record(repo: &Repo, st: &mut State, a: &Attempt, hook: bool) -> Result<(), String> {
    let b = st.branches.entry(a.branch.clone()).or_default();
    if hook {
        b.hook = Some(a.clone());
    } else {
        b.wrapper = Some(a.clone());
    }
    save(repo, st)
}

pub struct Lock {
    path: PathBuf,
    token: String,
}

fn pid_alive(pid: u32) -> bool {
    if cfg!(unix) {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    } else {
        true
    }
}

pub fn lock_is_stale(path: &Path) -> bool {
    let Ok(s) = fs::read_to_string(path) else {
        return false;
    };
    let mut l = s.lines();
    let pid = l.next().and_then(|v| v.parse::<u32>().ok());
    let t = l.next().and_then(|v| v.parse::<u64>().ok());
    let aged = t
        .map(|t| now_unix().saturating_sub(t) > STALE_SECS)
        .unwrap_or(false);
    aged || pid.map(|p| !pid_alive(p)).unwrap_or(false)
}

impl Lock {
    pub fn acquire(repo: &Repo, wait: Duration) -> R<Lock> {
        let d = ensure_dir(repo)?;
        let path = d.join("lock");
        let token = format!("{}\n{}\n", std::process::id(), now_unix());
        let start = Instant::now();
        loop {
            match create_new(&path, 0o600) {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(token.as_bytes())?;
                    return Ok(Lock { path, token });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    refuse_symlink(&path)?;
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() >= wait {
                        return Err(CliError::usage("another git-replicate operation holds the repository lock")
                            .hint(format!(
                                "Wait for it to finish. If you are sure no git-replicate process is running, delete {}",
                                path.display()
                            )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(CliError::usage(format!(
                        "could not create lock {}: {e}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .map(|s| s == self.token)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub const LOCK_WAIT: Duration = Duration::from_secs(10);
