use crate::util::redact;
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::{IsTerminal, Write};

#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub msg: String,
    pub hint: Option<String>,
}

impl CliError {
    pub fn new(code: i32, msg: impl Into<String>) -> Self {
        CliError {
            code,
            msg: msg.into(),
            hint: None,
        }
    }
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::new(2, msg)
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
}

pub type R<T> = Result<T, CliError>;

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::usage(format!("I/O error: {e}"))
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct RemoteRes {
    pub remote: String,
    pub status: String,
    pub message: Option<String>,
}

#[derive(Serialize, Default)]
struct Event {
    schema: u32,
    event: String,
    operation: String,
    remote: Option<String>,
    branch: Option<String>,
    oid: Option<String>,
    status: Option<String>,
    code: Option<i32>,
    message: Option<String>,
    primary_status: Option<String>,
    mirror_results: Option<Vec<RemoteRes>>,
    partial: Option<bool>,
}

#[derive(Default)]
pub struct Final {
    pub status: String,
    pub code: i32,
    pub message: String,
    pub branch: Option<String>,
    pub oid: Option<String>,
    pub primary_status: Option<String>,
    pub mirror_results: Option<Vec<RemoteRes>>,
    pub partial: Option<bool>,
}

pub struct Out {
    pub json: bool,
    pub color: bool,
    pub verbose: bool,
    pub op: String,
}

impl Out {
    pub fn new(json: bool, no_color: bool, verbose: bool, op: &str) -> Out {
        let env_ok = std::env::var("NO_COLOR")
            .map(|v| v.is_empty())
            .unwrap_or(true)
            && std::env::var("CLICOLOR").map(|v| v != "0").unwrap_or(true)
            && std::env::var("TERM").map(|v| v != "dumb").unwrap_or(true);
        Out {
            json,
            color: !json && !no_color && env_ok && std::io::stdout().is_terminal(),
            verbose,
            op: op.to_string(),
        }
    }

    fn emit(&self, mut e: Event) {
        e.schema = 1;
        e.operation = self.op.clone();
        e.message = e.message.map(|m| redact(&m));
        if let Ok(s) = serde_json::to_string(&e) {
            let mut o = std::io::stdout().lock();
            let _ = writeln!(o, "{s}");
            let _ = o.flush();
        }
    }

    fn paint(&self, status: &str, s: &str) -> String {
        if !self.color {
            return s.to_string();
        }
        match status {
            "ok" | "created" | "unchanged" | "installed" | "removed" | "pass" | "planned" => {
                s.green().to_string()
            }
            "failed" | "error" | "mismatch" | "missing" => s.red().to_string(),
            "unknown" | "warn" | "partial" | "skipped" | "pending" => s.yellow().to_string(),
            _ => s.to_string(),
        }
    }

    fn tag(&self, status: &str) -> String {
        self.paint(status, &format!("[{status}]"))
    }

    pub fn heading(&self, m: &str) {
        if !self.json {
            if self.color {
                println!("{}", m.bold());
            } else {
                println!("{m}");
            }
        }
    }

    pub fn info(&self, m: &str) {
        if !self.json {
            println!("{}", redact(m));
        }
    }

    pub fn detail(&self, m: &str) {
        if self.verbose && !self.json {
            eprintln!("verbose: {}", redact(m));
        }
    }

    pub fn plan(&self, remote: Option<&str>, branch: Option<&str>, oid: Option<&str>, m: &str) {
        if self.json {
            self.emit(Event {
                event: "plan".into(),
                remote: remote.map(Into::into),
                branch: branch.map(Into::into),
                oid: oid.map(Into::into),
                status: Some("planned".into()),
                message: Some(m.into()),
                ..Default::default()
            });
        } else {
            println!("{} {}", self.tag("planned"), redact(m));
        }
    }

    pub fn remote_result(
        &self,
        remote: &str,
        branch: Option<&str>,
        oid: Option<&str>,
        status: &str,
        m: &str,
    ) {
        if self.json {
            self.emit(Event {
                event: "remote_result".into(),
                remote: Some(remote.into()),
                branch: branch.map(Into::into),
                oid: oid.map(Into::into),
                status: Some(status.into()),
                message: Some(m.into()),
                ..Default::default()
            });
        } else {
            println!("{} {}: {}", self.tag(status), remote, redact(m));
        }
    }

    pub fn check(&self, remote: Option<&str>, status: &str, m: &str) {
        if self.json {
            self.emit(Event {
                event: "check".into(),
                remote: remote.map(Into::into),
                status: Some(status.into()),
                message: Some(m.into()),
                ..Default::default()
            });
        } else {
            let label = match remote {
                Some(r) => format!("{r}: {m}"),
                None => m.to_string(),
            };
            println!("{} {}", self.tag(status), redact(&label));
        }
    }

    pub fn warning(&self, m: &str) {
        if self.json {
            self.emit(Event {
                event: "warning".into(),
                status: Some("warn".into()),
                message: Some(m.into()),
                ..Default::default()
            });
        } else {
            eprintln!("{} {}", self.paint("warn", "warning:"), redact(m));
        }
    }

    pub fn result(&self, f: Final) {
        if self.json {
            self.emit(Event {
                event: "result".into(),
                branch: f.branch,
                oid: f.oid,
                status: Some(f.status),
                code: Some(f.code),
                message: Some(f.message),
                primary_status: f.primary_status,
                mirror_results: f.mirror_results,
                partial: f.partial,
                ..Default::default()
            });
        } else {
            println!("{} {}", self.tag(&f.status), redact(&f.message));
        }
    }

    pub fn error(&self, e: &CliError) {
        if self.json {
            let m = match &e.hint {
                Some(h) => format!("{} ({h})", e.msg),
                None => e.msg.clone(),
            };
            self.emit(Event {
                event: "result".into(),
                status: Some("error".into()),
                code: Some(e.code),
                message: Some(m),
                ..Default::default()
            });
        } else {
            eprintln!("{} {}", self.paint("error", "error:"), redact(&e.msg));
            if let Some(h) = &e.hint {
                for l in h.lines() {
                    eprintln!("  {}", redact(l));
                }
            }
        }
    }

    pub fn table(&self, headers: &[&str], rows: Vec<Vec<String>>) {
        if self.json {
            return;
        }
        let mut t = comfy_table::Table::new();
        t.load_style(comfy_table::presets::ASCII_BORDERS_ONLY_CONDENSED);
        t.set_header(headers.to_vec());
        for r in rows {
            t.add_row(r.iter().map(|c| redact(c)).collect::<Vec<_>>());
        }
        println!("{t}");
    }
}
