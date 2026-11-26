use crate::model::{Credits, FetchError, Identity, Limit, Snapshot, UsageEvent};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use reqwest::{StatusCode, blocking::Client, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, Metadata},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};
use walkdir::{DirEntry, WalkDir};

const BASE_URL: &str = "https://chatgpt.com/backend-api";
const AUTH_CLAIMS: &str = "https://api.openai.com/auth";
const DAY: u64 = 86_400;

struct Credentials {
    identity: Identity,
    workspace: String,
    access_token: String,
}

pub fn identity(path: &Path) -> Result<Identity, FetchError> {
    Ok(read_credentials(path)?.identity)
}

pub fn fetch(path: &Path, expected: &str, client: &Client) -> Result<Snapshot, FetchError> {
    fetch_from(path, expected, client, BASE_URL)
}

fn read_credentials(path: &Path) -> Result<Credentials, FetchError> {
    let bytes = fs::read(path.join("auth.json")).map_err(|error| match error.kind() {
        ErrorKind::NotFound => FetchError::Login,
        _ => FetchError::Invalid("Cannot read Codex auth.json".into()),
    })?;
    let root: Value = serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|_| FetchError::Invalid("Invalid Codex auth.json".into()))?;
    if matches!(root["auth_mode"].as_str(), Some("apikey" | "api_key")) {
        return Err(FetchError::Login);
    }

    let tokens = &root["tokens"];
    let access = nonempty(&tokens["access_token"]).ok_or(FetchError::Login)?;
    let access_claims = jwt_claims(access).unwrap_or(Value::Null);
    let id_claims = nonempty(&tokens["id_token"])
        .and_then(jwt_claims)
        .unwrap_or(Value::Null);
    let access_auth = &access_claims[AUTH_CLAIMS];
    let id_auth = &id_claims[AUTH_CLAIMS];
    let workspace = nonempty(&tokens["account_id"])
        .or_else(|| nonempty(&access_auth["chatgpt_account_id"]))
        .or_else(|| nonempty(&id_auth["chatgpt_account_id"]))
        .ok_or(FetchError::Login)?;
    if nonempty(&access_auth["chatgpt_account_id"]).is_some_and(|claim| claim != workspace)
        || nonempty(&id_auth["chatgpt_account_id"]).is_some_and(|claim| claim != workspace)
    {
        return Err(FetchError::Login);
    }
    let mut user = None;
    for field in ["chatgpt_user_id", "user_id"] {
        let access_user = nonempty(&access_auth[field]);
        let id_user = nonempty(&id_auth[field]);
        if access_user.zip(id_user).is_some_and(|(a, b)| a != b) {
            return Err(FetchError::Login);
        }
        user = access_user.or(id_user);
        if user.is_some() {
            break;
        }
    }
    let user = user
        .or_else(|| nonempty(&access_claims["sub"]))
        .or_else(|| nonempty(&id_claims["sub"]))
        .ok_or(FetchError::Login)?;
    let email = nonempty(&id_claims["email"])
        .or_else(|| nonempty(&id_claims["https://api.openai.com/profile"]["email"]))
        .or_else(|| nonempty(&access_claims["email"]))
        .or_else(|| nonempty(&access_claims["https://api.openai.com/profile"]["email"]));
    let plan = nonempty(&access_auth["chatgpt_plan_type"])
        .or_else(|| nonempty(&id_auth["chatgpt_plan_type"]));
    Ok(Credentials {
        identity: Identity {
            key: digest("codex-account", &[workspace, user]),
            email: email.map(str::to_owned),
            plan: plan.map(str::to_owned),
        },
        workspace: workspace.to_owned(),
        access_token: access.to_owned(),
    })
}

fn nonempty(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn jwt_claims(token: &str) -> Option<Value> {
    let parts: Vec<_> = token.splitn(4, '.').collect();
    let [_, payload, _] = parts.as_slice() else {
        return None;
    };
    let data = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&data).ok()
}

fn digest(domain: &str, fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    format!("{:x}", hash.finalize())
}

fn fetch_from(
    path: &Path,
    expected: &str,
    client: &Client,
    base: &str,
) -> Result<Snapshot, FetchError> {
    let credentials = read_credentials(path)?;
    if credentials.identity.key != expected {
        return Err(FetchError::IdentityChanged);
    }
    let request = |endpoint| {
        client
            .get(format!("{base}/wham/{endpoint}"))
            .bearer_auth(&credentials.access_token)
            .header("ChatGPT-Account-Id", &credentials.workspace)
            .header("Accept", "application/json")
            .header("User-Agent", "taskbar-ai-quota")
    };
    let response = request("usage")
        .timeout(Duration::from_secs(10))
        .send()
        .map_err(|error| {
            if error.is_builder() || error.is_redirect() {
                FetchError::Invalid("Could not build Codex usage request".into())
            } else {
                FetchError::Offline
            }
        })?;
    let status = response.status();
    let headers = response.headers().clone();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(FetchError::Login),
        StatusCode::TOO_MANY_REQUESTS => {
            return Err(FetchError::RateLimited(retry_after(&headers)));
        }
        _ if status.is_server_error() => return Err(FetchError::Offline),
        _ if !status.is_success() => {
            return Err(FetchError::Invalid(format!(
                "Codex usage HTTP {}",
                status.as_u16()
            )));
        }
        _ => {}
    }

    let body: Value = response.json().map_err(transport_error)?;
    if !body.is_object()
        || !(body
            .get("rate_limit")
            .is_some_and(|rate| rate.is_object() || rate.is_null())
            || nonempty(&body["plan_type"]).is_some()
            || body["spend_control"].is_object()
            || headers.contains_key("x-codex-primary-used-percent")
            || headers.contains_key("x-codex-secondary-used-percent"))
    {
        return Err(FetchError::Invalid("Expected a Codex usage object".into()));
    }
    if nonempty(&body["account_id"])
        .or_else(|| nonempty(&body["chatgpt_account_id"]))
        .is_some_and(|account| account != credentials.workspace)
    {
        return Err(FetchError::IdentityChanged);
    }
    let mut snapshot = usage_snapshot(&body, &headers);
    snapshot.plan = snapshot.plan.or(credentials.identity.plan.clone());
    snapshot.credits = request("rate-limit-reset-credits")
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "Codex Desktop")
        .timeout(Duration::from_secs(3))
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<Value>())
        .ok()
        .as_ref()
        .and_then(reset_credits);
    Ok(snapshot)
}

fn transport_error(error: reqwest::Error) -> FetchError {
    if error.is_timeout() || error.is_connect() || error.is_body() {
        FetchError::Offline
    } else {
        FetchError::Invalid("Could not read Codex usage response".into())
    }
}

fn retry_after(headers: &HeaderMap) -> u64 {
    let value = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok());
    value
        .and_then(|value| {
            value.trim().parse().ok().or_else(|| {
                let deadline = DateTime::parse_from_rfc2822(value).ok()?.timestamp();
                Some(deadline.saturating_sub(Utc::now().timestamp()).max(0) as u64)
            })
        })
        .unwrap_or(60)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn unsigned(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        let text = value.as_str()?;
        text.parse().ok().or_else(|| {
            DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|t| t.timestamp())
        })
    })
}

fn usage_snapshot(body: &Value, headers: &HeaderMap) -> Snapshot {
    let mut snapshot = Snapshot {
        plan: nonempty(&body["plan_type"]).map(str::to_owned),
        ..Snapshot::default()
    };
    let rate = &body["rate_limit"];
    let secondary_present = rate["secondary_window"].is_object()
        || headers.contains_key("x-codex-secondary-used-percent");
    for (name, header, default_duration) in [
        (
            "primary_window",
            "x-codex-primary-used-percent",
            if secondary_present { 0 } else { DAY },
        ),
        ("secondary_window", "x-codex-secondary-used-percent", DAY),
    ] {
        let window = &rate[name];
        let used = headers
            .get(header)
            .and_then(|header| header.to_str().ok()?.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .or_else(|| number(&window["used_percent"]))
            .or_else(|| number(&window["usage_percent"]));
        let Some(used) = used else { continue };
        let duration = unsigned(&window["limit_window_seconds"]).unwrap_or(default_duration);
        let limit = Limit {
            remaining: 100.0 - used.clamp(0.0, 100.0),
            resets_at: timestamp(&window["reset_at"]),
        };
        if duration < DAY {
            snapshot.short.get_or_insert(limit);
        } else if duration < 20 * DAY {
            snapshot.week.get_or_insert(limit);
        }
    }
    snapshot
}

fn reset_credits(body: &Value) -> Option<Credits> {
    let available: Vec<_> = body["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|credit| {
            nonempty(&credit["status"]).is_some_and(|s| s.eq_ignore_ascii_case("available"))
        })
        .collect();
    let count = unsigned(&body["available_count"])
        .or_else(|| body["credits"].is_array().then_some(available.len() as u64))?;
    let count = u32::try_from(count).ok()?;
    let expires_at = if count > 0 {
        available
            .iter()
            .filter_map(|credit| timestamp(&credit["expires_at"]))
            .min()
    } else {
        None
    };
    Some(Credits { count, expires_at })
}

pub fn log_files(path: &Path) -> Vec<PathBuf> {
    let Ok(home) = path.canonicalize() else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for directory in ["sessions", "archived_sessions"] {
        let root = home.join(directory);
        if !root
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata))
        {
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false)
            .into_iter()
            .filter_entry(plain_entry)
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            {
                files.push(entry.into_path());
            }
        }
    }
    files.sort_unstable();
    files
}

fn plain_entry(entry: &DirEntry) -> bool {
    entry
        .path()
        .symlink_metadata()
        .is_ok_and(|metadata| !is_link(&metadata))
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct ParserState {
    session: Option<String>,
    model: Option<String>,
    fast: bool,
    cumulative: Option<TokenCounts>,
    last_key: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct TokenCounts {
    input: Option<u64>,
    cached: u64,
    output: Option<u64>,
    total: u64,
}

impl TokenCounts {
    fn read(value: &Value) -> Option<Self> {
        let input = unsigned(&value["input_tokens"]);
        let output = unsigned(&value["output_tokens"]);
        let total =
            unsigned(&value["total_tokens"]).or_else(|| Some(input?.checked_add(output?)?))?;
        Some(Self {
            input,
            cached: unsigned(&value["cached_input_tokens"]).unwrap_or(0),
            output,
            total,
        })
    }

    fn since(self, earlier: Self) -> Option<Self> {
        Some(Self {
            input: self
                .input
                .zip(earlier.input)
                .and_then(|(a, b)| a.checked_sub(b)),
            output: self
                .output
                .zip(earlier.output)
                .and_then(|(a, b)| a.checked_sub(b)),
            cached: self.cached.saturating_sub(earlier.cached),
            total: self.total.checked_sub(earlier.total)?,
        })
    }
}

pub fn parse_line(line: &str, state: &mut Value) -> Option<UsageEvent> {
    if ![
        "session_meta",
        "turn_context",
        "thread_settings_applied",
        "token_count",
    ]
    .iter()
    .any(|kind| line.contains(kind))
    {
        return None;
    }
    let record: Value = serde_json::from_str(line.trim_start_matches('\u{feff}')).ok()?;
    let payload = &record["payload"];
    let mut current = ParserState::deserialize(&*state).unwrap_or_default();
    let mut event = None;
    match (record["type"].as_str(), payload["type"].as_str()) {
        (Some("session_meta"), _) => {
            let session =
                log_label(&payload["id"]).or_else(|| log_label(&payload["session_id"]))?;
            if current.session.as_deref() != Some(session) {
                current = ParserState {
                    session: Some(session.to_owned()),
                    ..ParserState::default()
                };
            }
        }
        (Some("turn_context"), _) => {
            if let Some(model) = log_label(&payload["model"]) {
                current.model = Some(model.to_owned());
            }
            if let Some(tier) = payload["service_tier"].as_str() {
                current.fast = matches!(tier, "priority" | "fast");
            }
        }
        (Some("event_msg"), Some("thread_settings_applied")) => {
            let settings = &payload["thread_settings"];
            if settings.is_object() {
                current.fast =
                    matches!(settings["service_tier"].as_str(), Some("priority" | "fast"));
            }
            if let Some(model) = log_label(&settings["model"]) {
                current.model = Some(model.to_owned());
            }
        }
        (Some("event_msg"), Some("token_count")) => {
            event = usage_event(&record, &mut current);
        }
        _ => return None,
    }
    *state = serde_json::to_value(current).ok()?;
    event
}

fn log_label(value: &Value) -> Option<&str> {
    nonempty(value).filter(|text| {
        text.len() <= 128
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
    })
}

fn usage_event(record: &Value, state: &mut ParserState) -> Option<UsageEvent> {
    let stamp = timestamp(&record["timestamp"])?;
    let payload = &record["payload"];
    let info = &payload["info"];
    let session = state
        .session
        .as_deref()
        .or_else(|| log_label(&payload["session_id"]))?;
    let last = &info["last_token_usage"];
    let cumulative = TokenCounts::read(&info["total_token_usage"]);
    let previous = state.cumulative;
    let counts = match (TokenCounts::read(last), cumulative, previous) {
        (Some(last), _, _) => last,
        (None, Some(total), Some(previous)) => total.since(previous)?,
        (None, Some(total), None) => total,
        _ => return None,
    };
    if cumulative
        .zip(previous)
        .is_some_and(|(a, b)| a.total == b.total)
    {
        return None;
    }
    if cumulative.is_some() {
        state.cumulative = cumulative;
    }
    if counts.total == 0 {
        return None;
    }
    let key = if let Some(total) = cumulative {
        digest("codex-usage-total", &[session, &total.total.to_string()])
    } else {
        let stamp = record["timestamp"]
            .as_str()
            .map_or_else(|| stamp.to_string(), str::to_owned);
        let counts = serde_json::to_string(&counts).ok()?;
        digest("codex-usage-event", &[session, &stamp, &counts])
    };
    if state.last_key.as_deref() == Some(&key) {
        return None;
    }
    state.last_key = Some(key.clone());

    if let Some(model) = log_label(&payload["model"]).or_else(|| log_label(&info["model"])) {
        state.model = Some(model.to_owned());
    }
    let fast = payload["service_tier"]
        .as_str()
        .map_or(state.fast, |tier| matches!(tier, "priority" | "fast"));
    let cost_usd = [last, info, payload, record]
        .into_iter()
        .find_map(|source| {
            number(&source["cost_usd"])
                .or_else(|| number(&source["costUSD"]))
                .filter(|amount| *amount >= 0.0)
        })
        .or_else(|| estimate_cost(state.model.as_deref()?, counts, fast));
    let identity = nonempty(&payload["chatgpt_account_id"])
        .or_else(|| nonempty(&payload["account_id"]))
        .zip(nonempty(&payload["chatgpt_user_id"]))
        .map(|(workspace, user)| digest("codex-account", &[workspace, user]));

    Some(UsageEvent {
        key,
        timestamp: stamp,
        tokens: counts.total,
        cost_usd,
        identity,
    })
}

type Rates = (f64, f64, f64);

fn estimate_cost(model: &str, counts: TokenCounts, fast: bool) -> Option<f64> {
    let mut model = model.strip_prefix("openai/").unwrap_or(model);
    let (base, implied_fast) = model
        .strip_suffix("-fast")
        .map_or((model, false), |base| (base, true));
    model = base;
    for suffix in ["-extra-high", "-xhigh", "-high", "-medium", "-low", "-none"] {
        if let Some(base) = model.strip_suffix(suffix) {
            model = base;
            break;
        }
    }
    if let Some((name, date)) = model
        .len()
        .checked_sub(11)
        .and_then(|cut| model.split_at_checked(cut))
    {
        if date.starts_with('-')
            && chrono::NaiveDate::parse_from_str(&date[1..], "%Y-%m-%d").is_ok()
        {
            model = name;
        }
    }
    let input = counts.input?;
    let output = counts.output?;
    let long = input > 272_000;
    // USD per million input, cached input, and output tokens in the pinned pricing catalog.
    let (standard, priority): (Rates, Option<Rates>) = match (model, long) {
        ("gpt-5" | "gpt-5.1", _) => ((1.25, 0.125, 10.0), Some((2.5, 0.25, 20.0))),
        ("gpt-5-codex" | "gpt-5.1-codex" | "gpt-5.1-codex-max", _) => ((1.25, 0.125, 10.0), None),
        ("gpt-5-mini", _) => ((0.25, 0.025, 2.0), Some((0.45, 0.045, 3.6))),
        ("gpt-5.1-codex-mini", _) => ((0.25, 0.025, 2.0), None),
        ("gpt-5-nano", _) => ((0.05, 0.005, 0.4), None),
        ("gpt-5.2" | "gpt-5.2-codex" | "gpt-5.3-codex", _) => {
            ((1.75, 0.175, 14.0), Some((3.5, 0.35, 28.0)))
        }
        ("gpt-5.4", false) => ((2.5, 0.25, 15.0), Some((5.0, 0.5, 30.0))),
        ("gpt-5.4", true) => ((5.0, 0.5, 22.5), Some((10.0, 1.0, 45.0))),
        ("gpt-5.4-mini", _) => ((0.75, 0.075, 4.5), Some((1.5, 0.15, 9.0))),
        ("gpt-5.4-nano", _) => ((0.2, 0.02, 1.25), None),
        ("gpt-5.5", false) => ((5.0, 0.5, 30.0), Some((12.5, 1.25, 75.0))),
        ("gpt-5.5", true) => ((10.0, 1.0, 45.0), Some((25.0, 2.5, 112.5))),
        ("gpt-5.6", false) => ((4.0, 0.4, 20.0), Some((8.0, 0.8, 40.0))),
        ("gpt-5.6", true) => ((8.0, 0.8, 30.0), Some((16.0, 1.6, 60.0))),
        ("gpt-5.6-sol", _) => ((4.0, 0.4, 20.0), Some((8.0, 0.8, 40.0))),
        ("gpt-5.6-terra", _) => ((2.0, 0.2, 12.0), Some((4.0, 0.4, 24.0))),
        ("gpt-5.6-luna", _) => ((0.2, 0.02, 1.2), Some((0.4, 0.04, 2.4))),
        ("gpt-5-pro", _) => ((15.0, 1.5, 120.0), None),
        ("gpt-5.2-pro", _) => ((21.0, 2.1, 168.0), None),
        ("gpt-5.4-pro" | "gpt-5.5-pro", false) => ((30.0, 3.0, 180.0), None),
        ("gpt-5.4-pro" | "gpt-5.5-pro", true) => ((60.0, 3.0, 270.0), None),
        _ => return None,
    };
    let (uncached_rate, cached_rate, output_rate) = if fast || implied_fast {
        priority?
    } else {
        standard
    };
    let cached = counts.cached.min(input);
    Some(
        ((input - cached) as f64 * uncached_rate
            + cached as f64 * cached_rate
            + output as f64 * output_rate)
            / 1_000_000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        thread,
        time::Instant,
    };
    use tempfile::tempdir;

    fn token(user: &str, workspace: &str) -> String {
        let payload = json!({
            "email": "synthetic@example.test",
            (AUTH_CLAIMS): {
                "chatgpt_user_id": user, "chatgpt_account_id": workspace, "chatgpt_plan_type": "plus"
            }
        });
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn write_auth(home: &Path, user: &str, workspace: &str) -> String {
        let token = token(user, workspace);
        let auth = json!({"tokens": {"access_token": token, "account_id": workspace}});
        fs::write(home.join("auth.json"), auth.to_string()).unwrap();
        token
    }

    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    fn accept(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    return stream;
                }
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("No test request: {error}"),
            }
        }
    }

    fn request_headers(stream: &TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                return headers;
            }
            headers.push_str(&line);
        }
    }

    fn reply(stream: &mut TcpStream, status: &str, headers: &str, body: &str) {
        write!(stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len(),
        ).unwrap();
    }

    #[test]
    fn account_mismatch_is_detected_before_any_request() {
        let home = tempdir().unwrap();
        write_auth(home.path(), "user-a", "workspace-a");
        let initial = identity(home.path()).unwrap();
        assert_eq!(initial.email.as_deref(), Some("synthetic@example.test"));
        assert_eq!(initial.plan.as_deref(), Some("plus"));
        write_auth(home.path(), "user-b", "workspace-a");
        let other_user = identity(home.path()).unwrap();
        assert_ne!(initial.key, other_user.key);
        write_auth(home.path(), "user-a", "workspace-b");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        assert!(matches!(
            fetch_from(home.path(), &initial.key, &client(), &url),
            Err(FetchError::IdentityChanged)
        ));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn identity_rejects_missing_or_inconsistent_workspace_and_api_keys() {
        let home = tempdir().unwrap();
        let file = home.path().join("auth.json");
        for body in [
            json!({"OPENAI_API_KEY": "synthetic-key"}),
            json!({"tokens":{"access_token":"opaque", "account_id":"workspace-a"}}),
            json!({"tokens":{"access_token":token("user-a", "workspace-a"), "account_id":"workspace-b"}}),
        ] {
            fs::write(&file, body.to_string()).unwrap();
            assert!(matches!(identity(home.path()), Err(FetchError::Login)));
        }
        let id = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"sub":"user-from-sub"}"#)
        );
        fs::write(
            &file,
            json!({"tokens":{"access_token":"opaque", "id_token":id, "account_id":"workspace-a"}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            identity(home.path()).unwrap().key,
            digest("codex-account", &["workspace-a", "user-from-sub"])
        );
    }

    #[test]
    fn fetch_binds_both_requests_to_one_credential_read() {
        for credit_result in [
            ("200 OK", r#"{"available_count":0}"#),
            ("503 Unavailable", "{}"),
        ] {
            let home = tempdir().unwrap();
            let access = write_auth(home.path(), "user-a", "workspace-a");
            let expected = identity(home.path()).unwrap().key;
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let home_path = home.path().to_owned();
            let server = thread::spawn(move || {
                let mut usage = accept(&listener);
                let first = request_headers(&usage);
                write_auth(&home_path, "user-b", "workspace-b");
                reply(
                    &mut usage,
                    "200 OK",
                    "x-codex-primary-used-percent: 18\r\n",
                    r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":604800,"reset_at":1800000000}},"plan_type":"pro"}"#,
                );
                let mut credits = accept(&listener);
                let second = request_headers(&credits);
                reply(&mut credits, credit_result.0, "", credit_result.1);
                for (headers, path) in [(first, "usage"), (second, "rate-limit-reset-credits")] {
                    assert!(headers.starts_with(&format!("GET /wham/{path} HTTP/1.1\r\n")));
                    assert!(headers.contains(&format!("authorization: Bearer {access}\r\n")));
                    assert!(headers.contains("chatgpt-account-id: workspace-a\r\n"));
                }
            });
            let snapshot = fetch_from(home.path(), &expected, &client(), &url).unwrap();
            server.join().unwrap();
            let week = snapshot.week.unwrap();
            assert_eq!(week.remaining, 82.0);
            assert_eq!(week.resets_at, Some(1_800_000_000));
            assert!(snapshot.short.is_none());
            assert_eq!(snapshot.plan.as_deref(), Some("pro"));
            assert_eq!(
                snapshot.credits.map(|credits| credits.count),
                (credit_result.0 == "200 OK").then_some(0)
            );
        }
    }

    #[test]
    fn windows_and_credit_expiries_use_the_response_metadata() {
        let usage = json!({"rate_limit":{
            "primary_window":{"used_percent":12.5,"limit_window_seconds":18000},
            "secondary_window":{"used_percent":-1,"limit_window_seconds":604800}
        }});
        let snapshot = usage_snapshot(&usage, &HeaderMap::new());
        assert_eq!(snapshot.short.unwrap().remaining, 87.5);
        assert_eq!(snapshot.week.unwrap().remaining, 100.0);
        assert!(snapshot.fable.is_none());
        let monthly = json!({"rate_limit":{"primary_window":{"used_percent":80,"limit_window_seconds":2592000}}});
        let snapshot = usage_snapshot(&monthly, &HeaderMap::new());
        assert!(snapshot.week.is_none() && snapshot.short.is_none());
        let credits = reset_credits(&json!({"credits":[
            {"status":"available","expires_at":1800000001_i64},
            {"status":"used","expires_at":1700000000_i64},
            {"status":"available","expires_at":"2027-01-15T08:00:00Z"}
        ]}))
        .unwrap();
        assert_eq!(credits.count, 2);
        assert_eq!(credits.expires_at, Some(1_800_000_000));
        assert!(reset_credits(&json!({})).is_none());
    }

    #[test]
    fn usage_failures_map_to_the_public_error_contract() {
        let home = tempdir().unwrap();
        write_auth(home.path(), "user-a", "workspace-a");
        let key = identity(home.path()).unwrap().key;
        for (status, headers, body) in [
            ("401 Unauthorized", "", "{}"),
            ("429 Too Many Requests", "retry-after: 17\r\n", "{}"),
            ("429 Too Many Requests", "", "{}"),
            ("502 Bad Gateway", "", "{}"),
            ("200 OK", "", "{}"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let mut request = accept(&listener);
                request_headers(&request);
                reply(&mut request, status, headers, body);
            });
            let result = fetch_from(home.path(), &key, &client(), &url);
            server.join().unwrap();
            assert!(matches!(
                (status, headers, result),
                ("401 Unauthorized", _, Err(FetchError::Login))
                    | (
                        "429 Too Many Requests",
                        "retry-after: 17\r\n",
                        Err(FetchError::RateLimited(17))
                    )
                    | (
                        "429 Too Many Requests",
                        "",
                        Err(FetchError::RateLimited(60))
                    )
                    | ("502 Bad Gateway", _, Err(FetchError::Offline))
                    | ("200 OK", _, Err(FetchError::Invalid(_)))
            ));
        }
    }

    #[test]
    fn log_discovery_excludes_escaped_and_unrelated_paths() {
        let home = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sessions = home.path().join("sessions");
        let archive = home.path().join("archived_sessions");
        fs::create_dir_all(sessions.join("nested")).unwrap();
        fs::create_dir(&archive).unwrap();
        for path in [
            sessions.join("nested/current.JSONL"),
            archive.join("stored.jsonl"),
            sessions.join("skip.json"),
            home.path().join("outside-log-roots.jsonl"),
            outside.path().join("escaped.jsonl"),
        ] {
            fs::write(path, "").unwrap();
        }
        let link = sessions.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command",
                    "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:QUOTA_TEST_JUNCTION -Target $env:QUOTA_TEST_TARGET | Out-Null"])
                .env("QUOTA_TEST_JUNCTION", &link)
                .env("QUOTA_TEST_TARGET", outside.path())
                .status().unwrap();
            assert!(status.success());
        }
        let canonical = home.path().canonicalize().unwrap();
        assert_eq!(
            log_files(home.path()),
            vec![
                canonical.join("archived_sessions/stored.jsonl"),
                canonical.join("sessions/nested/current.JSONL"),
            ]
        );
        fs::remove_dir(&link)
            .or_else(|_| fs::remove_file(&link))
            .unwrap();
    }

    fn record(state: &mut Value, body: Value) -> Option<UsageEvent> {
        parse_line(&body.to_string(), state)
    }

    fn token_count(timestamp: &str, info: Value) -> Value {
        json!({"type":"event_msg", "timestamp":timestamp, "payload":{"type":"token_count", "info":info}})
    }

    #[test]
    fn usage_resumes_and_deduplicates_without_storing_prompt_data() {
        let mut state = Value::Null;
        record(
            &mut state,
            json!({"type":"session_meta","payload":{"id":"session-a","instructions":"sensitive"}}),
        );
        record(
            &mut state,
            json!({"type":"turn_context","payload":{"model":"gpt-5.4","user_instructions":"sensitive"}}),
        );
        record(
            &mut state,
            json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":"priority"}}}),
        );
        let counts = json!({"input_tokens":100,"cached_input_tokens":20,"output_tokens":10,"total_tokens":110});
        let first = token_count(
            "2026-11-26T00:01:00.200Z",
            json!({"last_token_usage":counts,"total_token_usage":counts}),
        );
        let event = record(&mut state, first.clone()).unwrap();
        assert_eq!(event.tokens, 110);
        assert_eq!(event.timestamp, 1_795_651_260);
        assert_eq!(event.identity, None);
        assert!((event.cost_usd.unwrap() - 0.00071).abs() < 1e-10);
        let saved = state.to_string();
        assert!(!saved.contains("sensitive"));
        let mut state = serde_json::from_str(&saved).unwrap();
        assert!(record(&mut state, first).is_none());
        let second = token_count(
            "2026-11-26T00:01:00.500Z",
            json!({"last_token_usage":counts,"total_token_usage":{"total_tokens":220}}),
        );
        let next = record(&mut state, second).unwrap();
        assert_eq!(next.timestamp, event.timestamp);
        assert_ne!(next.key, event.key);
        let old_state = state.clone();
        assert!(parse_line("not-json", &mut state).is_none());
        assert_eq!(state, old_state);
    }

    #[test]
    fn reported_cost_wins_and_unknown_models_are_unpriced() {
        let mut state = Value::Null;
        record(
            &mut state,
            json!({"type":"session_meta","payload":{"id":"session-a"}}),
        );
        record(
            &mut state,
            json!({"type":"turn_context","payload":{"model":"future-model"}}),
        );
        let a = token_count(
            "2026-11-26T01:00:00Z",
            json!({"last_token_usage":{"input_tokens":100,"output_tokens":50},"cost_usd":0.005}),
        );
        assert_eq!(record(&mut state, a).unwrap().cost_usd, Some(0.005));
        let b = token_count(
            "2026-11-26T01:00:01Z",
            json!({"last_token_usage":{"input_tokens":100,"output_tokens":50}}),
        );
        assert_eq!(record(&mut state, b).unwrap().cost_usd, None);
    }
}
