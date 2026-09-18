//! Thin, typed wrapper around the official `sc` CLI.
//!
//! Contract (from `sc capabilities --json`):
//!   envelope : {"ok": bool, "command": str, "data": <any>}
//!   error    : {"ok": false, "command": str, "error": {"code","message"}, "hints": [..]}
//!   exit     : 0 ok | 10 validation | 20 auth/config | 30 network/backend | 1 generic

use serde_json::Value;
use std::process::Command;
use std::time::{Duration, Instant};

pub const BIN: &str = "sc";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScErrorKind {
    Validation,
    Auth,
    Network,
    /// Backend rate limit. Distinct from a generic network error: retrying
    /// immediately makes it worse, so the caller must back off.
    RateLimited,
    /// The Mac is locked, so the Secure Enclave cannot sign the request. This
    /// arrives with the same exit code as an auth failure but is not one: the
    /// session is intact and the fix is to unlock the machine, not to log in
    /// again. Telling the user to run `sc login` here would be wrong.
    DeviceLocked,
    Generic,
}

impl ScErrorKind {
    fn from_code(code: i32) -> Self {
        match code {
            10 => ScErrorKind::Validation,
            20 => ScErrorKind::Auth,
            30 => ScErrorKind::Network,
            _ => ScErrorKind::Generic,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScError {
    pub kind: ScErrorKind,
    pub code: String,
    pub message: String,
    pub hints: Vec<String>,
}

impl std::fmt::Display for ScError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if !self.hints.is_empty() {
            write!(f, " — {}", self.hints.join("; "))?;
        }
        Ok(())
    }
}

impl std::error::Error for ScError {}

/// One CLI invocation plus timing, so the UI can show the real cost of a poll.
#[derive(Debug, Clone)]
pub struct Call {
    pub argv: Vec<String>,
    pub elapsed: Duration,
    pub raw: String,
    pub data: Result<Value, ScError>,
}

pub fn run(args: &[&str]) -> Call {
    let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if !argv.iter().any(|a| a == "--json") {
        argv.push("--json".into());
    }

    let started = Instant::now();
    let out = Command::new(BIN).args(&argv).output();
    let elapsed = started.elapsed();

    let (raw, data) = match out {
        Err(e) => (
            String::new(),
            Err(ScError {
                kind: ScErrorKind::Generic,
                code: "spawn_failed".into(),
                message: format!("cannot run `{BIN}`: {e}"),
                hints: vec!["brew install scalable-cli".into()],
            }),
        ),
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let exit = o.status.code().unwrap_or(1);
            let body = if stdout.trim().is_empty() { stderr.clone() } else { stdout.clone() };
            let parsed = parse_envelope(&body, exit);
            (body, parsed)
        }
    };

    Call { argv, elapsed, raw, data }
}

impl Call {
    /// The exact command line that ran — the audit trail behind every row in the log.
    pub fn cmdline(&self) -> String {
        format!("{BIN} {}", self.argv.join(" "))
    }

    /// Raw stdout, truncated. Only useful when parsing failed.
    pub fn raw_head(&self, n: usize) -> String {
        self.raw.chars().take(n).collect()
    }
}

fn parse_envelope(body: &str, exit: i32) -> Result<Value, ScError> {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Err(ScError {
                kind: ScErrorKind::from_code(exit),
                code: "bad_json".into(),
                message: format!("non-JSON output ({e}): {}", body.chars().take(400).collect::<String>()),
                hints: vec![],
            })
        }
    };

    // `sc` reports logical failure inside the envelope even on exit 0 (e.g. no_session).
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v.get("data").cloned().unwrap_or(Value::Null));
    }

    let err = v.get("error");
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let kind = match code.as_str() {
        "rate_limited" => ScErrorKind::RateLimited,
        "device_locked" => ScErrorKind::DeviceLocked,
        _ => ScErrorKind::from_code(exit),
    };
    Err(ScError {
        kind,
        code: err
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        message: err
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string(),
        hints: v
            .get("hints")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    })
}

/// Every read endpoint nests its payload under `result` alongside the
/// account/portfolio context. Unwrap once, here.
pub fn result(v: &Value) -> &Value {
    v.get("result").unwrap_or(v)
}

/// Walk a JSON pointer-ish path, tolerating shape drift by trying several candidates.
pub fn pick<'a>(v: &'a Value, paths: &[&str]) -> Option<&'a Value> {
    for p in paths {
        let mut cur = v;
        let mut ok = true;
        for seg in p.split('/').filter(|s| !s.is_empty()) {
            match cur.get(seg) {
                Some(next) => cur = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && !cur.is_null() {
            return Some(cur);
        }
    }
    None
}

pub fn f64_at(v: &Value, paths: &[&str]) -> Option<f64> {
    pick(v, paths).and_then(|x| match x {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.replace(',', ".").trim().parse::<f64>().ok(),
        _ => None,
    })
}

pub fn str_at(v: &Value, paths: &[&str]) -> Option<String> {
    pick(v, paths).and_then(|x| match x {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}
