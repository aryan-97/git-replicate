use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '/' || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let auth = &tail[..end];
        match auth.rfind('@') {
            Some(at) => {
                out.push_str("***@");
                out.push_str(&auth[at + 1..]);
            }
            None => out.push_str(auth),
        }
        let after = &tail[end..];
        let tend = after
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(after.len());
        let seg = &after[..tend];
        match seg.find(['?', '#']) {
            Some(q) => {
                out.push_str(&seg[..q]);
                out.push_str("?<redacted>");
            }
            None => out.push_str(seg),
        }
        rest = &after[tend..];
    }
    out.push_str(rest);
    out
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 255
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

pub fn validate_url(u: &str) -> Result<(), String> {
    if u.is_empty() || u.len() > 2048 {
        return Err("URL is empty or too long".into());
    }
    if u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL contains whitespace or control characters".into());
    }
    if u.starts_with('-') {
        return Err("URL must not start with '-'".into());
    }
    if u.contains('#') || u.contains('?') {
        return Err("URL fragments and query strings are not supported".into());
    }
    if let Some((scheme, rest)) = u.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        let end = rest.find('/').unwrap_or(rest.len());
        let (auth, path) = rest.split_at(end);
        if path.len() < 2 {
            return Err("URL has no repository path".into());
        }
        let hostport = match auth.rsplit_once('@') {
            Some((user, hp)) => {
                if scheme != "ssh" {
                    return Err("embedded credentials are not allowed in HTTPS URLs".into());
                }
                if user.contains(':') {
                    return Err("embedded passwords are not allowed in URLs".into());
                }
                if !valid_user(user) {
                    return Err("invalid user name in URL".into());
                }
                hp
            }
            None => auth,
        };
        match scheme.as_str() {
            "https" | "ssh" => {}
            "http" => {
                return Err("insecure http:// URLs are not supported; use https:// or SSH".into());
            }
            other => {
                return Err(format!(
                    "unsupported transport '{other}://'; use SSH or HTTPS"
                ));
            }
        }
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        };
        if !valid_host(host) {
            return Err("invalid host in URL".into());
        }
        if let Some(p) = port
            && (p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
        {
            return Err("invalid port in URL".into());
        }
        return Ok(());
    }
    if u.contains("::") {
        return Err("remote helpers (such as ext::) are not supported".into());
    }
    let Some((left, path)) = u.split_once(':') else {
        return Err("local paths are not supported; use an SSH or HTTPS URL".into());
    };
    if path.is_empty() || left.contains('/') {
        return Err("not a valid SSH (user@host:path) or HTTPS URL".into());
    }
    let host = match left.split_once('@') {
        Some((user, h)) => {
            if !valid_user(user) {
                return Err("invalid user name in URL".into());
            }
            h
        }
        None => left,
    };
    if host.len() < 2 || !valid_host(host) {
        return Err("not a valid SSH (user@host:path) or HTTPS URL".into());
    }
    Ok(())
}

pub fn dest_key(u: &str) -> String {
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let end = rest.find('/').unwrap_or(rest.len());
        let (auth, path) = rest.split_at(end);
        (auth.rsplit('@').next().unwrap_or(auth), path)
    } else {
        let (left, path) = u.split_once(':').unwrap_or((u, ""));
        (left.rsplit('@').next().unwrap_or(left), path)
    };
    let p = path.trim_matches('/');
    let p = p.strip_suffix(".git").unwrap_or(p);
    format!("{}/{}", host.to_ascii_lowercase(), p)
}

pub fn valid_ident(s: &str, max: usize) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic())
        && s.len() <= max
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn is_hex_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn is_zero_oid(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '0')
}

pub fn short(oid: &str) -> &str {
    &oid[..oid.len().min(10)]
}

pub fn refuse_symlink(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(io::Error::other(format!(
            "refusing to use symlink {}",
            p.display()
        ))),
        _ => Ok(()),
    }
}

pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    refuse_symlink(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let res = (|| {
        let mut f = create_new(&tmp, mode)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

pub fn create_new(path: &Path, mode: u32) -> io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    o.open(path)
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn iso(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in [
            "git@github.com:team/project.git",
            "ssh://git@example.org/team/project.git",
            "ssh://git@example.org:2222/team/project.git",
            "https://github.com/team/project.git",
        ] {
            assert!(validate_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://x.org/a.git",
            "file:///tmp/x",
            "/tmp/x",
            "../x",
            "ext::sh -c id",
            "git://x.org/a",
            "https://user:pw@x.org/a",
            "https://token@x.org/a",
            "ssh://u:pw@x.org/a",
            "https://x.org/a#frag",
            "https://x.org/a?token=1",
            "-oProxyCommand=x",
            "C:\\repo",
            "",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn redaction() {
        assert_eq!(
            redact("fatal: https://user:tok@host.com/a.git failed"),
            "fatal: https://***@host.com/a.git failed"
        );
        assert_eq!(
            redact("https://h/a?token=abc x"),
            "https://h/a?<redacted> x"
        );
        assert_eq!(redact("plain"), "plain");
    }

    #[test]
    fn keys() {
        assert_eq!(
            dest_key("git@GitHub.com:a/b.git"),
            dest_key("ssh://git@github.com/a/b")
        );
    }

    #[test]
    fn time() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
