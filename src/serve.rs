//! A small read-only web UI. Standard library only: one thread per
//! connection, three static files, a JSON state endpoint and the timeline SVG.
//!
//! It has no authentication. By default it listens on loopback only, and it
//! refuses requests whose `Host` header is not a loopback name, which stops a
//! web page in your browser from reading it through DNS rebinding.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::formula::Verdict;
use crate::monitor::{Event, Monitor};
use crate::render::{timeline_svg, RenderOptions};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_CSS: &str = include_str!("../web/app.css");
const APP_JS: &str = include_str!("../web/app.js");

const MAX_CONNECTIONS: usize = 32;
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_EVENTS: usize = 100;

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// What the web UI shows. Written by the scrape loop, read by request threads.
pub struct Shared {
    pub monitor: Monitor,
    pub events: VecDeque<Event>,
    pub mode: String,
    pub scrape_error: Option<String>,
    pub kv_threshold: Option<f64>,
    pub started: Instant,
}

impl Shared {
    pub fn new(monitor: Monitor, mode: impl Into<String>, kv_threshold: Option<f64>) -> Self {
        Self {
            monitor,
            events: VecDeque::new(),
            mode: mode.into(),
            scrape_error: None,
            kv_threshold,
            started: Instant::now(),
        }
    }

    pub fn push_event(&mut self, e: Event) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(e);
    }
}

pub fn serve(listener: TcpListener, shared: Arc<Mutex<Shared>>) -> io::Result<()> {
    let loopback_only = listener.local_addr()?.ip().is_loopback();
    let active = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = respond(
                &mut stream,
                503,
                "text/plain",
                b"too many connections\n",
                false,
            );
            continue;
        }
        let shared = Arc::clone(&shared);
        let active = Arc::clone(&active);
        std::thread::spawn(move || {
            let _ = handle(stream, &shared, loopback_only);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, shared: &Mutex<Shared>, loopback_only: bool) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let head = match read_head(&mut stream)? {
        Some(h) => h,
        None => return respond(&mut stream, 400, "text/plain", b"bad request\n", false),
    };
    let req = match parse_head(&head) {
        Some(r) => r,
        None => return respond(&mut stream, 400, "text/plain", b"bad request\n", false),
    };

    let head_only = req.method == "HEAD";
    if req.method != "GET" && !head_only {
        return respond(
            &mut stream,
            405,
            "text/plain",
            b"method not allowed\n",
            false,
        );
    }
    if loopback_only && !host_is_loopback(req.host.as_deref()) {
        return respond(&mut stream, 403, "text/plain", b"forbidden host\n", false);
    }

    let (path, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
    match path {
        "/" => respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
            head_only,
        ),
        "/app.css" => respond(
            &mut stream,
            200,
            "text/css; charset=utf-8",
            APP_CSS.as_bytes(),
            head_only,
        ),
        "/app.js" => respond(
            &mut stream,
            200,
            "text/javascript; charset=utf-8",
            APP_JS.as_bytes(),
            head_only,
        ),
        "/api/state" => {
            let body = state_json(&shared.lock().unwrap_or_else(|p| p.into_inner()));
            respond(
                &mut stream,
                200,
                "application/json",
                body.to_string().as_bytes(),
                head_only,
            )
        }
        "/timeline.svg" => {
            let last = query_number(query, "last").unwrap_or(150).clamp(10, 900);
            let (frames, rules, kv_threshold) = {
                let s = shared.lock().unwrap_or_else(|p| p.into_inner());
                let frames: Vec<_> = s.monitor.frames().iter().cloned().collect();
                (frames, s.monitor.rules().to_vec(), s.kv_threshold)
            };
            let svg = timeline_svg(
                &frames,
                &rules,
                &RenderOptions {
                    kv_threshold,
                    last_frames: Some(last),
                },
            );
            respond(&mut stream, 200, "image/svg+xml", svg.as_bytes(), head_only)
        }
        _ => respond(&mut stream, 404, "text/plain", b"not found\n", false),
    }
}

struct Request {
    method: String,
    target: String,
    host: Option<String>,
}

fn read_head(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8(buf).ok());
        }
        if buf.len() > MAX_REQUEST_BYTES {
            return Ok(None);
        }
    }
}

fn parse_head(head: &str) -> Option<Request> {
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    if !target.starts_with('/') || !parts.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let host = lines.take_while(|l| !l.is_empty()).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("host").then(|| v.trim().to_string())
    });
    Some(Request {
        method,
        target,
        host,
    })
}

/// `localhost`, `127.0.0.1` or `[::1]`, with an optional port.
fn host_is_loopback(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((ip, _)) => ip,
            None => return false,
        }
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn query_number(query: &str, key: &str) -> Option<usize> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Service Unavailable",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
Content-Security-Policy: {CSP}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn state_json(s: &Shared) -> Value {
    let frames = s.monitor.frames();
    let latest = frames.back();
    let rules: Vec<Value> = s
        .monitor
        .rules()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let firing = s.monitor.firing().get(i).copied().unwrap_or(false);
            let verdict = latest
                .and_then(|f| f.verdicts.get(i).copied())
                .unwrap_or(Verdict::Unknown);
            let state = if firing {
                "firing"
            } else if verdict == Verdict::False {
                "ok"
            } else {
                "unknown"
            };
            json!({
                "name": r.name,
                "severity": r.severity,
                "description": r.description,
                "pattern": r.pattern.to_string(),
                "lookback_s": r.pattern.lookback(),
                "state": state,
            })
        })
        .collect();

    json!({
        "mode": s.mode,
        "uptime_s": s.started.elapsed().as_secs_f64(),
        "scrape_error": s.scrape_error,
        "latest": latest.map(|f| json!({
            "t": f.t, "running": f.running, "waiting": f.waiting,
            "kv_usage": f.kv_usage, "token_rate": f.token_rate,
            "preemption_rate": f.preemption_rate,
        })),
        "rules": rules,
        "events": s.events.iter().rev().take(30).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{builtin_rules, RuleConfig};
    use crate::sim::{generate, Scenario};
    use std::net::TcpListener;

    #[test]
    fn host_header_allowlist() {
        assert!(host_is_loopback(Some("localhost:9100")));
        assert!(host_is_loopback(Some("127.0.0.1")));
        assert!(host_is_loopback(Some("127.0.0.1:9100")));
        assert!(host_is_loopback(Some("[::1]:9100")));
        assert!(!host_is_loopback(Some("evil.example:9100")));
        assert!(!host_is_loopback(Some("127.0.0.1.evil.example")));
        assert!(!host_is_loopback(Some("10.0.0.5:9100")));
        assert!(!host_is_loopback(None));
    }

    #[test]
    fn parses_a_request_and_its_host() {
        let r = parse_head("GET /x?last=5 HTTP/1.1\r\nHost: localhost:1\r\nAccept: */*\r\n\r\n")
            .unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.target, "/x?last=5");
        assert_eq!(r.host.as_deref(), Some("localhost:1"));
        assert!(parse_head("garbage").is_none());
        assert!(parse_head("GET nopath HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn query_numbers() {
        assert_eq!(query_number("last=30&x=1", "last"), Some(30));
        assert_eq!(query_number("x=1", "last"), None);
        assert_eq!(query_number("last=abc", "last"), None);
    }

    fn request(addr: std::net::SocketAddr, raw: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn serves_the_ui_state_and_timeline_and_refuses_bad_requests() {
        let rules = builtin_rules(&RuleConfig::default());
        let mut monitor = Monitor::new(rules);
        for s in generate(Scenario::Stall, 40, 2.0) {
            monitor.ingest(s).unwrap();
        }
        let shared = Arc::new(Mutex::new(Shared::new(monitor, "test", Some(0.9))));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = serve(listener, shared);
        });

        let host = format!("Host: {addr}\r\nConnection: close\r\n\r\n");

        let page = request(addr, &format!("GET / HTTP/1.1\r\n{host}"));
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("Content-Security-Policy"));
        assert!(page.contains("<html"));

        let state = request(addr, &format!("GET /api/state HTTP/1.1\r\n{host}"));
        assert!(state.contains("\"engine_stalled\""));
        assert!(state.contains("application/json"));

        let svg = request(
            addr,
            &format!("GET /timeline.svg?last=20 HTTP/1.1\r\n{host}"),
        );
        assert!(svg.contains("image/svg+xml") && svg.contains("<svg"));

        let rebinding = request(
            addr,
            "GET /api/state HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n",
        );
        assert!(rebinding.starts_with("HTTP/1.1 403"));

        let head = request(addr, &format!("HEAD /api/state HTTP/1.1\r\n{host}"));
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(
            !head.contains("engine_stalled"),
            "HEAD must not carry a body"
        );

        let post = request(addr, &format!("POST /api/state HTTP/1.1\r\n{host}"));
        assert!(post.starts_with("HTTP/1.1 405"));

        let missing = request(addr, &format!("GET /nope HTTP/1.1\r\n{host}"));
        assert!(missing.starts_with("HTTP/1.1 404"));

        let junk = request(addr, "hello\r\n\r\n");
        assert!(junk.starts_with("HTTP/1.1 400"));
    }
}
