use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);

pub fn net_timeout() -> Duration {
    let s = std::env::var("GIT_REPLICATE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120);
    Duration::from_secs(s)
}

#[derive(Debug, Default)]
pub struct GitOut {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub spawn_err: Option<String>,
}

impl GitOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out && self.spawn_err.is_none()
    }
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut b);
        }
        let _ = tx.send(b);
    });
    rx
}

pub fn run(cwd: &Path, args: &[&str], env: &[(String, String)], timeout: Duration) -> GitOut {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return GitOut {
                spawn_err: Some(format!("could not run git: {e}")),
                ..Default::default()
            };
        }
    };
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());
    let start = Instant::now();
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(s)) => break (s.code(), false),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break (None, false),
        }
    };
    let grace = Duration::from_secs(2);
    let stdout = out_rx.recv_timeout(grace).unwrap_or_default();
    let stderr = err_rx.recv_timeout(grace).unwrap_or_default();
    GitOut {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
        spawn_err: None,
    }
}

pub fn summarize(o: &GitOut) -> String {
    if let Some(e) = &o.spawn_err {
        return e.clone();
    }
    let mut lines: Vec<&str> = o
        .stdout
        .lines()
        .filter(|l| l.starts_with('!'))
        .chain(o.stderr.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(6);
    lines.drain(..skip);
    let mut s = crate::util::redact(&lines.join(" | "));
    if s.len() > 600 {
        let mut end = 600;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("...");
    }
    if s.is_empty() {
        match o.code {
            Some(c) => format!("git exited with status {c}"),
            None => "git terminated abnormally".into(),
        }
    } else {
        s
    }
}
