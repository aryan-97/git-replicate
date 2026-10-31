use crate::out::{CliError, R};
use crate::util::{dest_key, redact, valid_ident, validate_url, write_atomic};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const FILE: &str = ".gitreplicate";
pub const MAX_BYTES: u64 = 64 * 1024;
pub const MAX_MIRRORS: usize = 32;
pub const MAX_NAME: usize = 48;
pub const MAX_REMOTE: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Primary {
    pub remote: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    pub name: String,
    pub remote: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub branches: Vec<String>,
    pub primary: Primary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mirrors: Vec<Mirror>,
}

pub const MAX_PATTERNS: usize = 64;
pub const MAX_PATTERN_LEN: usize = 100;

pub fn validate_pattern(p: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("invalid branch pattern '{p}': {why}"));
    if p.is_empty() || p.len() > MAX_PATTERN_LEN {
        return bad(&format!("must be 1-{MAX_PATTERN_LEN} characters"));
    }
    if !p
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/+@*".contains(c))
    {
        return bad("only letters, digits and . _ - / + @ * are allowed");
    }
    if p.starts_with(['-', '/']) || p.ends_with('/') || p.ends_with(".lock") {
        return bad("must not start with '-' or '/', or end with '/' or '.lock'");
    }
    if p.contains("//") || p.contains("..") || p.contains("***") || p == "@" {
        return bad("contains an unsupported sequence ('//', '..', '***')");
    }
    Ok(())
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => (0..=s.len()).any(|i| glob(&p[2..], &s[i..])),
        Some(b'*') => {
            for i in 0..=s.len() {
                if glob(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        Some(c) => s.first() == Some(c) && glob(&p[1..], &s[1..]),
    }
}

pub fn glob_match(pattern: &str, branch: &str) -> bool {
    glob(pattern.as_bytes(), branch.as_bytes())
}

pub fn allowed_by<'a>(cfg: &'a Config, branch: &str) -> Option<&'a str> {
    cfg.branches
        .iter()
        .map(String::as_str)
        .find(|p| glob_match(p, branch))
}

impl Config {
    pub fn new(remote: &str, url: &str) -> Config {
        Config {
            version: 1,
            branches: vec![],
            primary: Primary {
                remote: remote.into(),
                url: url.into(),
            },
            mirrors: vec![],
        }
    }
}

pub fn validate(c: &Config) -> Result<(), String> {
    if c.version != 1 {
        return Err(format!(
            "unsupported config version {}; this tool supports version 1",
            c.version
        ));
    }
    if c.mirrors.len() > MAX_MIRRORS {
        return Err(format!("too many mirrors (max {MAX_MIRRORS})"));
    }
    if c.branches.len() > MAX_PATTERNS {
        return Err(format!("too many branch patterns (max {MAX_PATTERNS})"));
    }
    for (i, p) in c.branches.iter().enumerate() {
        validate_pattern(p)?;
        if c.branches[..i].contains(p) {
            return Err(format!("duplicate branch pattern '{p}'"));
        }
    }
    if !valid_ident(&c.primary.remote, MAX_REMOTE) {
        return Err(format!(
            "invalid primary remote name '{}'",
            c.primary.remote
        ));
    }
    validate_url(&c.primary.url).map_err(|e| format!("primary url: {e}"))?;
    let mut keys = vec![(dest_key(&c.primary.url), "primary".to_string())];
    for (i, m) in c.mirrors.iter().enumerate() {
        if !valid_ident(&m.name, MAX_NAME) {
            return Err(format!("invalid mirror name '{}'", m.name));
        }
        if !valid_ident(&m.remote, MAX_REMOTE) {
            return Err(format!(
                "mirror '{}': invalid remote name '{}'",
                m.name, m.remote
            ));
        }
        validate_url(&m.url).map_err(|e| format!("mirror '{}': {e}", m.name))?;
        if m.remote == c.primary.remote {
            return Err(format!(
                "mirror '{}' uses the primary remote '{}'",
                m.name, m.remote
            ));
        }
        for o in &c.mirrors[..i] {
            if o.name.eq_ignore_ascii_case(&m.name) {
                return Err(format!("duplicate mirror name '{}'", m.name));
            }
            if o.remote == m.remote {
                return Err(format!(
                    "remote '{}' is used by more than one mirror",
                    m.remote
                ));
            }
        }
        let k = dest_key(&m.url);
        if let Some((_, who)) = keys.iter().find(|(kk, _)| *kk == k) {
            return Err(format!(
                "mirror '{}' has the same destination as {who}",
                m.name
            ));
        }
        keys.push((k, format!("mirror '{}'", m.name)));
    }
    Ok(())
}

pub fn exists(root: &Path) -> bool {
    fs::symlink_metadata(root.join(FILE)).is_ok()
}

pub fn load(root: &Path) -> R<Config> {
    let p = root.join(FILE);
    let meta = match fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(_) => {
            return Err(
                CliError::usage(format!("{FILE} not found in {}", root.display()))
                    .hint("Run `git-replicate setup` first."),
            );
        }
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(CliError::usage(format!(
            "{FILE} must be a regular file, not a symlink"
        )));
    }
    if meta.len() > MAX_BYTES {
        return Err(CliError::usage(format!(
            "{FILE} is too large (max {MAX_BYTES} bytes)"
        )));
    }
    let bytes = fs::read(&p)?;
    let s = String::from_utf8(bytes)
        .map_err(|_| CliError::usage(format!("{FILE} is not valid UTF-8")))?;
    let bad = |m: String| CliError::usage(format!("invalid {FILE}: {}", redact(&m)));
    let v: toml::Value = toml::from_str(&s).map_err(|e| bad(e.to_string()))?;
    match v.get("version").and_then(|v| v.as_integer()) {
        Some(1) => {}
        Some(n) => {
            return Err(bad(format!(
                "unsupported version {n}; this tool supports version 1"
            )));
        }
        None => return Err(bad("missing integer 'version'".into())),
    }
    let c: Config = v
        .try_into()
        .map_err(|e: toml::de::Error| bad(e.to_string()))?;
    validate(&c).map_err(bad)?;
    Ok(c)
}

pub fn render(c: &Config) -> R<String> {
    let body = toml::to_string_pretty(c)
        .map_err(|e| CliError::usage(format!("could not serialize config: {e}")))?;
    Ok(format!(
        "# git-replicate configuration. Contains no credentials; review changes before trusting.\n{body}"
    ))
}

pub fn create(root: &Path, c: &Config) -> R<()> {
    let p = root.join(FILE);
    let mut f = crate::util::create_new(&p, 0o644)
        .map_err(|e| CliError::usage(format!("could not create {FILE}: {e}")))?;
    std::io::Write::write_all(&mut f, render(c)?.as_bytes())?;
    Ok(())
}

pub fn save(root: &Path, c: &Config) -> R<()> {
    validate(c).map_err(CliError::usage)?;
    write_atomic(&root.join(FILE), render(c)?.as_bytes(), 0o644)
        .map_err(|e| CliError::usage(format!("could not write {FILE}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        for (p, b, want) in [
            ("main", "main", true),
            ("main", "main2", false),
            ("release/*", "release/1.x", true),
            ("release/*", "release/1.x/fix", false),
            ("release/*", "release", false),
            ("release/**", "release/1/2/3", true),
            ("feature/*-x", "feature/a-x", true),
            ("feature/*-x", "feature/a/b-x", false),
            ("*", "main", true),
            ("*", "a/b", false),
            ("**", "a/b/c", true),
            ("a*c", "abbbc", true),
            ("a*c", "abbbd", false),
        ] {
            assert_eq!(glob_match(p, b), want, "{p} vs {b}");
        }
    }

    #[test]
    fn patterns() {
        for ok in ["main", "release/*", "hotfix/**", "v1.2+x", "user@x"] {
            assert!(validate_pattern(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", " ", "-a", "/a", "a/", "a..b", "a//b", "a***", "a b", "a!b", "x.lock", "@",
        ] {
            assert!(validate_pattern(bad).is_err(), "{bad:?}");
        }
    }
}
