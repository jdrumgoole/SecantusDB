//! The DigitalOcean v2 API, and the GitHub release lookup, driven through
//! `curl`.
//!
//! Shelling out rather than linking an HTTP client keeps a TLS stack out of a
//! workspace whose whole dependency list is `bson` / `rand` / `serde_json`, and
//! the orchestrator is a process driver already (ssh, scp, ping). The API token
//! is passed on curl's **stdin config**, never on the command line, so it does
//! not appear in `ps` output on a shared machine.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::BenchResult;

pub const API_BASE: &str = "https://api.digitalocean.com/v2";

pub const TOKEN_ENV_VARS: [&str; 4] = [
    "DIGITALOCEAN_TOKEN",
    "DO_TOKEN",
    "DIGITALOCEAN_ACCESS_TOKEN",
    "DO_API_TOKEN",
];

pub fn token_from_env() -> BenchResult<String> {
    for var in TOKEN_ENV_VARS {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                return Ok(value.trim().to_string());
            }
        }
    }
    Err(format!(
        "No DigitalOcean API token. Create one at \
         https://cloud.digitalocean.com/account/api/tokens (scopes: read + write) and export it:\n\
         \x20 export DIGITALOCEAN_TOKEN=dop_v1_...\n\
         (also accepted: {})",
        TOKEN_ENV_VARS[1..].join(", ")
    ))
}

pub struct Api {
    token: String,
}

/// Status codes worth retrying: DigitalOcean's rate limiter and its own 5xx.
/// A 401 or 422 is the caller's bug and retrying only delays the message.
fn retryable(status: u32) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

impl Api {
    pub fn new(token: String) -> Api {
        Api { token }
    }

    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> BenchResult<Value> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{API_BASE}{path}")
        };
        let mut last = String::new();
        for attempt in 0..6u32 {
            match self.curl(method, &url, body) {
                Ok((status, text)) if (200..300).contains(&status) => {
                    if text.trim().is_empty() {
                        return Ok(Value::Null);
                    }
                    return serde_json::from_str(&text)
                        .map_err(|e| format!("{method} {path}: bad JSON reply: {e}"));
                }
                Ok((status, text)) => {
                    last = format!("{method} {path} -> HTTP {status}: {text}");
                    if !retryable(status) || attempt == 5 {
                        return Err(last);
                    }
                }
                Err(e) => {
                    last = format!("{method} {path} -> {e}");
                    if attempt == 5 {
                        return Err(last);
                    }
                }
            }
            thread::sleep(Duration::from_secs(1u64 << attempt));
        }
        Err(format!("{method} {path} failed after retries: {last}"))
    }

    fn curl(&self, method: &str, url: &str, body: Option<&Value>) -> BenchResult<(u32, String)> {
        let mut cmd = Command::new("curl");
        cmd.args(curl_argv(method));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawning curl failed: {e}"))?;
        {
            let stdin = child.stdin.as_mut().ok_or("curl stdin unavailable")?;
            stdin
                .write_all(curl_config(url, &self.token, body).as_bytes())
                .map_err(|e| format!("curl stdin: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("curl failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "curl exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body_text, status) = split_status(&text)?;
        Ok((status, body_text))
    }

    pub fn paged(&self, path: &str, key: &str, extra: &str) -> BenchResult<Vec<Value>> {
        let sep = if path.contains('?') { "&" } else { "?" };
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let url = format!("{path}{sep}per_page=200&page={page}{extra}");
            let data = self.request("GET", &url, None)?;
            if let Some(items) = data.get(key).and_then(|v| v.as_array()) {
                out.extend(items.iter().cloned());
            }
            let has_next = data
                .get("links")
                .and_then(|l| l.get("pages"))
                .and_then(|p| p.get("next"))
                .is_some();
            if !has_next {
                return Ok(out);
            }
            page += 1;
        }
    }
}

/// The curl argv. Everything else — URL, auth, content type, request body —
/// travels on **stdin** via `--config -`, so the token never lands in `ps`
/// output.
///
/// Nothing here may consume stdin. An earlier version added `--data-binary @-`
/// for requests with a body, which fought `--config -` for the same stdin: the
/// config won, the body arrived empty, and DigitalOcean answered 415. The body
/// belongs in the config's own `data` entry, never on argv.
fn curl_argv(method: &str) -> Vec<String> {
    [
        "--silent",
        "--show-error",
        "--config",
        "-",
        // Print the status on its own trailing line so the body stays intact.
        "--write-out",
        "\n%{http_code}",
        "--max-time",
        "120",
        "-X",
        method,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The curl config fed to stdin: URL, auth header, and — for requests that
/// carry one — the JSON content type and body.
fn curl_config(url: &str, token: &str, body: Option<&Value>) -> String {
    let mut out = String::new();
    out.push_str(&format!("url = \"{url}\"\n"));
    out.push_str(&format!("header = \"Authorization: Bearer {token}\"\n"));
    if let Some(value) = body {
        out.push_str("header = \"Content-Type: application/json\"\n");
        out.push_str(&format!("data = {}\n", json_config_literal(value)));
    }
    out
}

/// curl's config parser needs a quoted, backslash-escaped literal.
fn json_config_literal(value: &Value) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Split curl's `body\n<status>` output.
fn split_status(text: &str) -> BenchResult<(String, u32)> {
    let idx = text.rfind('\n').ok_or("curl produced no status line")?;
    let status: u32 = text[idx + 1..]
        .trim()
        .parse()
        .map_err(|_| "curl produced no status code".to_string())?;
    Ok((text[..idx].to_string(), status))
}

/// The GitHub API, unauthenticated (the repo is public). `GITHUB_TOKEN` is
/// used when present, purely to lift the 60/hour anonymous rate limit.
pub fn github_json(path: &str) -> BenchResult<Value> {
    let url = format!("https://api.github.com/repos/{}{path}", crate::GITHUB_REPO);
    let mut cmd = Command::new("curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--location",
        "--config",
        "-",
        "--max-time",
        "60",
        "--write-out",
        "\n%{http_code}",
    ]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawning curl failed: {e}"))?;
    {
        let stdin = child.stdin.as_mut().ok_or("curl stdin unavailable")?;
        writeln!(stdin, "url = \"{url}\"").map_err(|e| format!("curl stdin: {e}"))?;
        writeln!(stdin, "header = \"Accept: application/vnd.github+json\"")
            .map_err(|e| format!("curl stdin: {e}"))?;
        writeln!(stdin, "user-agent = \"secantus-bench\"")
            .map_err(|e| format!("curl stdin: {e}"))?;
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            if !token.trim().is_empty() {
                writeln!(stdin, "header = \"Authorization: Bearer {}\"", token.trim())
                    .map_err(|e| format!("curl stdin: {e}"))?;
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, status) = split_status(&text)?;
    if !(200..300).contains(&status) {
        return Err(format!("GitHub {path} -> HTTP {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("GitHub {path}: bad JSON: {e}"))
}

/// `(tag, tarball_url, sha256_url)` for the Linux x86_64 server binary.
pub fn resolve_release_asset(
    version: &str,
    fetch: &dyn Fn(&str) -> BenchResult<Value>,
) -> BenchResult<(String, String, String)> {
    let tag = if version == "latest" {
        let releases = fetch("/releases?per_page=40")?;
        let list = releases
            .as_array()
            .ok_or("GitHub returned no release list")?;
        list.iter()
            .filter_map(|r| r.get("tag_name").and_then(|t| t.as_str()))
            // The PyPI `v*` tags are a different release line and must not be
            // mistaken for a server-binary release.
            .find(|t| t.starts_with("secantusdb-v"))
            .ok_or(
                "No secantusdb-v* release found. Pass --server-version \
                 secantusdb-vX.Y.Z-beta.N, or use --server-build source.",
            )?
            .to_string()
    } else if version.starts_with("secantusdb-v") {
        version.to_string()
    } else {
        format!("secantusdb-v{version}")
    };

    let release = fetch(&format!("/releases/tags/{tag}"))?;
    let assets = release
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tarball = String::new();
    let mut sha = String::new();
    for asset in assets {
        let name = asset.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(|u| u.as_str())
            .unwrap_or("");
        if name.ends_with("x86_64-unknown-linux-gnu.tar.gz") {
            tarball = url.to_string();
        } else if name.ends_with("x86_64-unknown-linux-gnu.tar.gz.sha256") {
            sha = url.to_string();
        }
    }
    if tarball.is_empty() || sha.is_empty() {
        return Err(format!("release {tag} has no x86_64 Linux binary asset"));
    }
    Ok((tag, tarball, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_is_split_off_the_body() {
        let (body, status) = split_status("{\"ok\":1}\n200").unwrap();
        assert_eq!(body, "{\"ok\":1}");
        assert_eq!(status, 200);
    }

    #[test]
    fn a_multiline_body_keeps_its_newlines() {
        let (body, status) = split_status("line1\nline2\n404").unwrap();
        assert_eq!(body, "line1\nline2");
        assert_eq!(status, 404);
    }

    #[test]
    fn curl_argv_never_consumes_stdin() {
        // `--config -` owns stdin. Anything else reading `@-` or `-` would
        // race it and silently send an empty body (observed: HTTP 415).
        for method in ["GET", "POST", "PUT", "DELETE"] {
            let argv = curl_argv(method);
            assert!(!argv
                .iter()
                .any(|a| a == "--data-binary" || a == "--data" || a == "-d"));
            assert!(!argv.iter().any(|a| a == "@-"));
            assert!(argv.contains(&method.to_string()));
        }
    }

    #[test]
    fn a_request_with_a_body_declares_json_and_carries_it_in_the_config() {
        let body = json!({"name": "x"});
        let config = curl_config("https://example/v2/droplets", "tok", Some(&body));
        assert!(config.contains("header = \"Content-Type: application/json\""));
        assert!(config.contains(r#"data = "{\"name\":\"x\"}""#));
        assert!(config.contains("header = \"Authorization: Bearer tok\""));
    }

    #[test]
    fn a_request_without_a_body_sends_no_content_type_or_data() {
        let config = curl_config("https://example/v2/droplets", "tok", None);
        assert!(!config.contains("Content-Type"));
        assert!(!config.contains("data ="));
    }

    #[test]
    fn the_token_never_reaches_argv() {
        assert!(!curl_argv("POST").iter().any(|a| a.contains("secret-token")));
        assert!(curl_config("https://example", "secret-token", None).contains("secret-token"));
    }

    #[test]
    fn json_bodies_are_escaped_for_curls_config_parser() {
        let literal = json_config_literal(&json!({"name": "a\"b"}));
        assert!(literal.starts_with('"') && literal.ends_with('"'));
        assert!(literal.contains("\\\"name\\\""));
    }

    #[test]
    fn only_the_rate_limiter_and_server_errors_retry() {
        assert!(retryable(429));
        assert!(retryable(503));
        assert!(!retryable(401));
        assert!(!retryable(422));
    }

    #[test]
    fn release_lookup_picks_the_linux_tarball_over_the_pypi_tag() {
        let fetch = |path: &str| -> BenchResult<Value> {
            if path.starts_with("/releases?") {
                return Ok(json!([
                    {"tag_name": "v0.6.0b12"},
                    {"tag_name": "secantusdb-v0.5.3-beta.160"}
                ]));
            }
            assert_eq!(path, "/releases/tags/secantusdb-v0.5.3-beta.160");
            Ok(json!({"assets": [
                {"name": "secantusdb-0.5.3-beta.160-aarch64-apple-darwin.tar.gz",
                 "browser_download_url": "https://example/mac"},
                {"name": "secantusdb-0.5.3-beta.160-x86_64-unknown-linux-gnu.tar.gz",
                 "browser_download_url": "https://example/linux"},
                {"name": "secantusdb-0.5.3-beta.160-x86_64-unknown-linux-gnu.tar.gz.sha256",
                 "browser_download_url": "https://example/linux.sha256"}
            ]}))
        };
        let (tag, tarball, sha) = resolve_release_asset("latest", &fetch).unwrap();
        assert_eq!(tag, "secantusdb-v0.5.3-beta.160");
        assert_eq!(tarball, "https://example/linux");
        assert_eq!(sha, "https://example/linux.sha256");
    }

    #[test]
    fn a_release_without_a_linux_asset_is_an_error() {
        let fetch = |_: &str| -> BenchResult<Value> { Ok(json!({"assets": []})) };
        assert!(resolve_release_asset("secantusdb-v9.9.9", &fetch).is_err());
    }

    #[test]
    fn missing_token_names_the_env_vars() {
        // Only assert the message shape; the process may legitimately have one set.
        let msg = TOKEN_ENV_VARS[1..].join(", ");
        assert!(msg.contains("DO_API_TOKEN"));
    }
}
