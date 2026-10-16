//! Where events go: always stdout as one JSON object per line, and optionally
//! a webhook and/or a local program. Both are opt-in; nothing leaves the
//! machine unless the operator passes `--webhook` or `--exec`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::monitor::Event;

/// Upper bound on a metrics page we are willing to read.
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

pub struct Emitter {
    webhook: Option<String>,
    exec: Option<String>,
}

impl Emitter {
    pub fn new(webhook: Option<String>, exec: Option<String>) -> Self {
        Self { webhook, exec }
    }

    pub fn emit(&self, event: &Event) {
        let line = match serde_json::to_string(event) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("nexusguard: could not encode event: {e}");
                return;
            }
        };
        println!("{line}");

        if let Some(url) = &self.webhook {
            if let Err(e) = post_json(url, &line) {
                eprintln!("nexusguard: webhook delivery failed: {e}");
            }
        }
        if let Some(program) = &self.exec {
            if let Err(e) = run_hook(program, &line) {
                eprintln!("nexusguard: exec hook failed: {e}");
            }
        }
    }
}

/// Fetches a metrics page. Only `http://` and `https://` URLs are accepted.
pub fn fetch(url: &str, timeout: Duration) -> Result<String, String> {
    check_scheme(url)?;
    let resp = ureq::get(url)
        .timeout(timeout)
        .call()
        .map_err(|e| e.to_string())?;
    let mut body = String::new();
    resp.into_reader()
        .take(MAX_BODY_BYTES)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(body)
}

fn post_json(url: &str, body: &str) -> Result<(), String> {
    check_scheme(url)?;
    ureq::post(url)
        .timeout(Duration::from_secs(5))
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn check_scheme(url: &str) -> Result<(), String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(format!(
            "only http:// and https:// URLs are supported, got {url:?}"
        ))
    }
}

/// Runs `program` directly (no shell) with the event JSON on its stdin.
fn run_hook(program: &str, json: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that does not read its stdin must not wedge the monitor.
        let _ = writeln!(stdin, "{json}");
    }
    // Reap it in the background so finished hooks do not pile up as zombies.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_schemes() {
        assert!(check_scheme("file:///etc/passwd").is_err());
        assert!(check_scheme("ftp://x").is_err());
        assert!(check_scheme("http://localhost:8000/metrics").is_ok());
        assert!(check_scheme("https://example.com/m").is_ok());
    }

    #[test]
    fn missing_hook_program_is_an_error_not_a_panic() {
        assert!(run_hook("/definitely/not/a/program", "{}").is_err());
    }
}
