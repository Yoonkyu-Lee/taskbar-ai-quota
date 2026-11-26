use crate::model::{Credits, FetchError, Identity, Limit, Snapshot, UsageEvent};
use chrono::{DateTime, Utc};
use reqwest::{StatusCode, blocking::Client, header::RETRY_AFTER, redirect::Policy};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, Metadata},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use walkdir::WalkDir;

const OAUTH_BASE: &str = "https://api.anthropic.com/api/oauth";
const PROFILE_TTL: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const RECENT_EVENT_KEYS: usize = 256;

struct Credentials {
    access_token: String,
    plan: Option<String>,
}

type ProfileCache = HashMap<[u8; 32], (Instant, Identity)>;
static IDENTITIES: LazyLock<Mutex<ProfileCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PROFILE_CLIENT: LazyLock<Result<Client, reqwest::Error>> =
    LazyLock::new(|| Client::builder().redirect(Policy::none()).build());

pub fn identity(path: &Path) -> Result<Identity, FetchError> {
    let credentials = read_credentials(path)?;
    let client = PROFILE_CLIENT
        .as_ref()
        .map_err(|_| invalid("Could not initialize Claude profile client"))?;
    verified_identity(&credentials, &IDENTITIES, |credentials| {
        get_json(client, "profile", credentials)
    })
}

pub fn fetch(path: &Path, expected: &str, client: &Client) -> Result<Snapshot, FetchError> {
    fetch_with(path, expected, &IDENTITIES, |endpoint, credentials| {
        get_json(client, endpoint, credentials)
    })
}

fn fetch_with(
    path: &Path,
    expected: &str,
    cache: &Mutex<ProfileCache>,
    mut request: impl FnMut(&str, &Credentials) -> Result<Value, FetchError>,
) -> Result<Snapshot, FetchError> {
    let credentials = read_credentials(path)?;
    let identity = verified_identity(&credentials, cache, |credentials| {
        request("profile", credentials)
    })?;
    if identity.key != expected {
        return Err(FetchError::IdentityChanged);
    }

    let usage = request("usage?cedar_ember=1", &credentials);
    if matches!(usage, Err(FetchError::Login))
        && let Ok(mut cache) = cache.lock()
    {
        cache.remove(&token_fingerprint(&credentials));
    }
    parse_snapshot(&usage?, identity.plan, Utc::now().timestamp())
}

fn read_credentials(path: &Path) -> Result<Credentials, FetchError> {
    let path = path.join(".credentials.json");
    let read_error = |error: std::io::Error| match error.kind() {
        ErrorKind::NotFound => FetchError::Login,
        _ => invalid("Could not read Claude credentials file"),
    };
    let metadata = path.symlink_metadata().map_err(read_error)?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(invalid("Expected a regular Claude credentials file"));
    }
    let data = fs::read(path).map_err(read_error)?;
    let root: Value = serde_json::from_slice(data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data))
        .map_err(|_| invalid("Invalid Claude credentials file"))?;
    let oauth = &root["claudeAiOauth"];
    let token = nonempty(&oauth["accessToken"]).ok_or(FetchError::Login)?;
    if oauth["expiresAt"]
        .as_i64()
        .is_some_and(|expires| expires <= Utc::now().timestamp_millis())
    {
        return Err(FetchError::Login);
    }
    Ok(Credentials {
        access_token: token.to_owned(),
        plan: nonempty(&oauth["subscriptionType"]).map(plan_name),
    })
}

fn token_fingerprint(credentials: &Credentials) -> [u8; 32] {
    Sha256::digest(credentials.access_token.as_bytes()).into()
}

fn verified_identity(
    credentials: &Credentials,
    cache: &Mutex<ProfileCache>,
    request: impl FnOnce(&Credentials) -> Result<Value, FetchError>,
) -> Result<Identity, FetchError> {
    let fingerprint = token_fingerprint(credentials);
    if let Ok(mut cache) = cache.lock() {
        cache.retain(|_, (checked, _)| checked.elapsed() < PROFILE_TTL);
        if let Some((_, identity)) = cache.get(&fingerprint) {
            return Ok(with_credential_plan(identity.clone(), credentials));
        }
    }

    let profile = request(credentials)?;
    let identity = parse_profile(&profile)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(fingerprint, (Instant::now(), identity.clone()));
    }
    Ok(with_credential_plan(identity, credentials))
}

fn with_credential_plan(mut identity: Identity, credentials: &Credentials) -> Identity {
    identity.plan = credentials.plan.clone().or(identity.plan);
    identity
}

fn parse_profile(profile: &Value) -> Result<Identity, FetchError> {
    let account = &profile["account"];
    let organization = &profile["organization"];
    let key = account_key(&account["uuid"], &organization["uuid"])
        .ok_or_else(|| invalid("Claude OAuth profile is missing account or organization UUID"))?;
    let plan = nonempty(&account["subscription_type"])
        .or_else(|| nonempty(&organization["organization_type"]))
        .map(plan_name);
    Ok(Identity {
        key,
        email: nonempty(&account["email"]).map(str::to_owned),
        plan,
    })
}

fn account_key(account: &Value, organization: &Value) -> Option<String> {
    let account = uuid(account)?;
    let organization = uuid(organization)?;
    Some(format!("claude:{account}:{organization}").to_ascii_lowercase())
}

fn uuid(value: &Value) -> Option<&str> {
    nonempty(value).filter(|id| {
        id.len() == 36
            && id.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
    })
}

fn plan_name(plan: &str) -> String {
    let normalized = plan.to_ascii_lowercase();
    match normalized.strip_prefix("claude_").unwrap_or(&normalized) {
        "free" => "Free",
        "pro" => "Pro",
        "max" => "Max",
        "team" => "Team",
        "enterprise" => "Enterprise",
        "ultra" => "Ultra",
        _ => plan,
    }
    .to_owned()
}

fn get_json(
    client: &Client,
    endpoint: &str,
    credentials: &Credentials,
) -> Result<Value, FetchError> {
    let response = client
        .get(format!("{OAUTH_BASE}/{endpoint}"))
        .bearer_auth(&credentials.access_token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .header("User-Agent", "claude-cli/2.1.280 (external, cli)")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .map_err(http_error)?;
    let retry = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok());
    check_http_status(response.status(), retry, Utc::now().timestamp())?;
    response.json().map_err(http_error)
}

fn check_http_status(status: StatusCode, retry: Option<&str>, now: i64) -> Result<(), FetchError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(FetchError::Login),
        StatusCode::TOO_MANY_REQUESTS => Err(FetchError::RateLimited(retry_seconds(retry, now))),
        _ if status.is_server_error() => Err(FetchError::Offline),
        _ if status.is_success() => Ok(()),
        _ => Err(invalid(format!("Claude OAuth HTTP {}", status.as_u16()))),
    }
}

fn http_error(error: reqwest::Error) -> FetchError {
    if error.is_timeout() || error.is_connect() || error.is_body() || error.is_request() {
        FetchError::Offline
    } else {
        invalid("Invalid Claude OAuth request or response")
    }
}

fn retry_seconds(header: Option<&str>, now: i64) -> u64 {
    header
        .map(str::trim)
        .and_then(|text| {
            text.parse::<u64>().ok().or_else(|| {
                DateTime::parse_from_rfc2822(text)
                    .ok()
                    .map(|date| date.timestamp().saturating_sub(now).max(0) as u64)
            })
        })
        .unwrap_or(300)
        .max(1)
}

fn invalid(message: impl Into<String>) -> FetchError {
    FetchError::Invalid(message.into())
}

fn nonempty(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn unix_seconds(value: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(nonempty(value)?)
        .ok()
        .map(|date| date.timestamp())
}

fn parse_snapshot(usage: &Value, plan: Option<String>, now: i64) -> Result<Snapshot, FetchError> {
    if !usage.is_object() {
        return Err(invalid("Expected a Claude usage object"));
    }
    let fable = usage["limits"].as_array().and_then(|limits| {
        limits.iter().find_map(|entry| {
            if entry["kind"].as_str() != Some("weekly_scoped")
                || !nonempty(&entry["scope"]["model"]["display_name"])
                    .is_some_and(|name| name.eq_ignore_ascii_case("Fable"))
            {
                return None;
            }
            usage_limit(entry, "percent")
        })
    });
    Ok(Snapshot {
        week: usage_limit(&usage["seven_day"], "utilization")
            .or_else(|| usage_limit(&usage["sevenDay"], "utilization")),
        short: usage_limit(&usage["five_hour"], "utilization")
            .or_else(|| usage_limit(&usage["fiveHour"], "utilization")),
        fable,
        credits: reset_credits(&usage["cedar_ember"], now),
        plan,
    })
}

fn usage_limit(window: &Value, field: &str) -> Option<Limit> {
    let used = window[field].as_f64().filter(|used| *used >= 0.0)?;
    Some(Limit {
        remaining: (100.0 - used).max(0.0),
        resets_at: unix_seconds(&window["resets_at"]),
    })
}

fn reset_credits(block: &Value, now: i64) -> Option<Credits> {
    block.as_object()?;
    let mut count = 0u32;
    let mut expires_at = None;
    if let (Some(true), Some(grants)) = (block["eligible"].as_bool(), block["grants"].as_array()) {
        for grant in grants {
            let Some(left) = grant["resets_left"]
                .as_u64()
                .filter(|n| (1..=100).contains(n))
            else {
                continue;
            };
            let expiry = unix_seconds(&grant["ends_at"]);
            if expiry.is_some_and(|time| time <= now) {
                continue;
            }
            count = count.saturating_add(left as u32);
            expires_at = match (expires_at, expiry) {
                (Some(earliest), Some(end)) => Some(std::cmp::min(earliest, end)),
                (earliest, end) => earliest.or(end),
            };
        }
    }
    Some(Credits { count, expires_at })
}

pub fn log_files(path: &Path) -> Vec<PathBuf> {
    let Ok(home) = path.canonicalize() else {
        return Vec::new();
    };
    let projects = home.join("projects");
    if !projects
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata))
    {
        return Vec::new();
    }
    let Ok(projects) = projects.canonicalize() else {
        return Vec::new();
    };
    if projects == home || !projects.starts_with(&home) {
        return Vec::new();
    }
    let mut files: Vec<_> = WalkDir::new(&projects)
        .follow_links(false)
        .follow_root_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .symlink_metadata()
                .is_ok_and(|metadata| !is_link(&metadata))
        })
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        })
        .filter_map(|entry| entry.path().canonicalize().ok())
        .filter(|path| path.starts_with(&projects))
        .collect();
    files.sort_unstable();
    files.dedup();
    files
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

pub fn parse_line(line: &str, state: &mut Value) -> Option<UsageEvent> {
    let record: Value = serde_json::from_str(line).ok()?;
    if record
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("assistant"))
    {
        return None;
    }
    let timestamp = unix_seconds(&record["timestamp"])?;
    let message = &record["message"];
    let usage = &message["usage"];
    usage.as_object()?;
    let counts = TokenCounts::from_usage(usage);
    let tokens = counts.total()?;
    let model = nonempty(&message["model"]).unwrap_or_default();
    let reported_cost = record["costUSD"].as_f64().filter(|cost| cost.is_finite());
    let key = event_key(&record, message, model, counts, reported_cost);
    if !remember_event(state, &key) {
        return None;
    }
    let cost_usd = reported_cost.or_else(|| {
        rates(model, usage["speed"].as_str() == Some("fast")).map(|prices| counts.estimate(prices))
    });
    let identity = account_key(&record["accountUuid"], &record["organizationUuid"])
        .or_else(|| account_key(&record["account_uuid"], &record["organization_uuid"]));
    Some(UsageEvent {
        key,
        timestamp,
        tokens,
        cost_usd,
        identity,
    })
}

#[derive(Clone, Copy)]
struct TokenCounts {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
}

impl TokenCounts {
    fn from_usage(usage: &Value) -> Self {
        let cache = &usage["cache_creation"];
        let sum = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        let one_hour = cache["ephemeral_1h_input_tokens"]
            .as_u64()
            .or_else(|| usage["ephemeral_1h_input_tokens"].as_u64())
            .unwrap_or(0);
        let five_min = cache["ephemeral_5m_input_tokens"]
            .as_u64()
            .or_else(|| usage["ephemeral_5m_input_tokens"].as_u64())
            .unwrap_or(0);
        Self {
            input: usage["input_tokens"].as_u64().unwrap_or(0),
            output: usage["output_tokens"].as_u64().unwrap_or(0),
            cache_read: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
            cache_write_5m: five_min.max(sum.saturating_sub(one_hour)),
            cache_write_1h: one_hour,
        }
    }

    fn total(self) -> Option<u64> {
        let total = self
            .input
            .checked_add(self.output)?
            .checked_add(self.cache_read)?
            .checked_add(self.cache_write_5m)?
            .checked_add(self.cache_write_1h)?;
        (total > 0 && i64::try_from(total).is_ok()).then_some(total)
    }

    fn estimate(self, rates: Rates) -> f64 {
        (self.input as f64 * rates.input
            + self.output as f64 * rates.output
            + self.cache_read as f64 * rates.cache_read
            + self.cache_write_5m as f64 * rates.cache_write_5m
            + self.cache_write_1h as f64 * rates.input * 2.0)
            * rates.multiplier
            / 1_000_000.0
    }
}

fn event_key(
    record: &Value,
    message: &Value,
    model: &str,
    counts: TokenCounts,
    cost: Option<f64>,
) -> String {
    let message_id = nonempty(&message["id"]);
    let request_id = nonempty(&record["requestId"]);
    let mut hash = Sha256::new();
    if message_id.is_some() || request_id.is_some() {
        hash_field(&mut hash, "request");
        hash_field(&mut hash, message_id.unwrap_or_default());
        hash_field(&mut hash, request_id.unwrap_or_default());
    } else if let Some(record_id) = nonempty(&record["uuid"]) {
        hash_field(&mut hash, "record");
        hash_field(&mut hash, record_id);
    } else {
        hash_field(&mut hash, "usage");
        hash_field(
            &mut hash,
            nonempty(&record["sessionId"]).unwrap_or_default(),
        );
        hash_field(
            &mut hash,
            nonempty(&record["timestamp"]).unwrap_or_default(),
        );
        hash_field(&mut hash, model);
        for total in [
            counts.input,
            counts.output,
            counts.cache_read,
            counts.cache_write_5m,
            counts.cache_write_1h,
        ] {
            hash.update(total.to_be_bytes());
        }
        if let Some(cost) = cost {
            hash.update(cost.to_bits().to_be_bytes());
        }
    }
    format!("claude:{:x}", hash.finalize())
}

fn hash_field(hash: &mut Sha256, field: &str) {
    hash.update((field.len() as u64).to_be_bytes());
    hash.update(field.as_bytes());
}

fn remember_event(state: &mut Value, key: &str) -> bool {
    if !state.is_array() {
        *state = Value::Array(Vec::new());
    }
    let entries = state
        .as_array_mut()
        .expect("initialized parser state array");
    if entries.iter().any(|entry| entry.as_str() == Some(key)) {
        return false;
    }
    let excess = (entries.len() + 1).saturating_sub(RECENT_EVENT_KEYS);
    entries.drain(..excess);
    entries.push(Value::String(key.to_owned()));
    true
}

struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write_5m: f64,
    multiplier: f64,
}

fn rates(model: &str, fast: bool) -> Option<Rates> {
    // USD per million tokens from TaskbarQuota a84432b's bundled pricing catalogs.
    let (input, output, cache_read, cache_write_5m, fast_multiplier) = match model {
        "claude-3-haiku-20240307" => (0.25, 1.25, 0.03, 0.3, 1.0),
        "claude-3-5-haiku" => (0.8, 4.0, 0.08, 1.0, 1.0),
        "claude-3-5-haiku-20241022" | "claude-3-5-haiku-latest" => (0.8, 4.0, 0.08, 0.8, 1.0),
        "claude-3-sonnet-20240229" => (3.0, 15.0, 0.3, 0.3, 1.0),
        "claude-3-5-sonnet-20240620"
        | "claude-3-5-sonnet-20241022"
        | "claude-3-7-sonnet"
        | "claude-sonnet-4-0" => (3.0, 15.0, 0.3, 3.75, 1.0),
        "claude-3-7-sonnet-20250219" | "claude-sonnet-4-20250514" => (3.0, 15.0, 0.3, 3.0, 1.0),
        "claude-3-opus"
        | "claude-3-opus-20240229"
        | "claude-opus-4"
        | "claude-opus-4-0"
        | "claude-opus-4-1" => (15.0, 75.0, 1.5, 18.75, 1.0),
        "claude-opus-4-20250514" | "claude-opus-4-1-20250805" => (15.0, 75.0, 1.5, 15.0, 1.0),
        "claude-fable-5" | "claude-mythos-5" | "claude-mythos-preview" => {
            (10.0, 50.0, 1.0, 12.5, 1.0)
        }
        "claude-fable-5-1" | "claude-mythos-5-1" => (10.0, 50.0, 0.25, 12.5, 1.0),
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => (1.0, 5.0, 0.1, 1.25, 1.0),
        "claude-haiku-5-5" => (0.1, 0.5, 0.01, 0.125, 1.0),
        "claude-opus-4-5"
        | "claude-opus-4-5-20251101"
        | "claude-opus-4-6"
        | "claude-opus-4-6-20260205" => (5.0, 25.0, 0.5, 6.25, 1.0),
        "claude-opus-4-7" | "claude-opus-4-7-20260416" => (5.0, 25.0, 0.5, 6.25, 6.0),
        "claude-opus-4-8" | "claude-opus-5" => (5.0, 25.0, 0.5, 6.25, 2.0),
        "claude-opus-5-5" => (4.0, 20.0, 0.2, 5.0, 2.0),
        "claude-sonnet-4-5"
        | "claude-sonnet-4-5-20250929"
        | "claude-sonnet-4-5-20250929-v1:0"
        | "claude-sonnet-4-6" => (3.0, 15.0, 0.3, 3.75, 1.0),
        "claude-sonnet-5" => (2.0, 10.0, 0.2, 2.5, 1.0),
        "claude-sonnet-5-5" => (2.0, 10.0, 0.1, 2.5, 1.0),
        _ => return None,
    };
    let multiplier = if fast { fast_multiplier } else { 1.0 };
    Some(Rates {
        input,
        output,
        cache_read,
        cache_write_5m,
        multiplier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACCOUNT: &str = "a0000000-0000-4000-8000-000000000001";
    const OTHER_ACCOUNT: &str = "a0000000-0000-4000-8000-000000000002";
    const ORGANIZATION: &str = "b0000000-0000-4000-8000-000000000001";
    const STAMP: &str = "2026-11-26T14:00:00Z";

    fn write_credentials(path: &Path, token: &str) {
        let credentials = json!({"claudeAiOauth": {
            "accessToken": token, "subscriptionType": "max",
        }});
        fs::write(path.join(".credentials.json"), credentials.to_string()).unwrap();
    }

    fn profile(account: &str) -> Value {
        json!({
            "account": {"uuid": account, "email": "fixture@example.test"},
            "organization": {"uuid": ORGANIZATION, "organization_type": "claude_pro"},
        })
    }

    #[test]
    fn fetch_binds_identity_and_usage_to_the_same_token_read() {
        let home = tempfile::tempdir().unwrap();
        let cache = Mutex::new(ProfileCache::new());
        let original = "fixture-original-token";
        let replacement = "fixture-replaced-token";
        write_credentials(home.path(), original);
        let mut steps = [
            ("profile", original, profile(ACCOUNT)),
            (
                "usage?cedar_ember=1",
                original,
                json!({"five_hour": {"utilization": 33}}),
            ),
            ("profile", replacement, profile(OTHER_ACCOUNT)),
        ]
        .into_iter();
        let mut request = |endpoint: &str, credentials: &Credentials| {
            let (target, token, response) = steps.next().expect("unexpected Claude request");
            assert_eq!(endpoint, target);
            assert_eq!(credentials.access_token, token);
            if endpoint == "profile" && token == original {
                write_credentials(home.path(), replacement);
            }
            Ok(response)
        };
        let expected = format!("claude:{ACCOUNT}:{ORGANIZATION}");
        let snapshot = fetch_with(home.path(), &expected, &cache, &mut request).unwrap();
        assert_eq!(snapshot.short.unwrap().remaining, 67.0);
        assert_eq!(snapshot.plan.as_deref(), Some("Max"));
        let changed = fetch_with(home.path(), &expected, &cache, request);
        assert!(
            matches!(changed, Err(FetchError::IdentityChanged)),
            "expected an identity change, got {changed:?}"
        );
        assert!(steps.next().is_none(), "expected all Claude requests");
    }

    #[test]
    fn verified_identity_reuses_only_the_same_tokens_cached_profile() {
        let cache = Mutex::new(ProfileCache::new());
        let credentials = Credentials {
            access_token: "fixture-cached-token".into(),
            plan: None,
        };
        let first = verified_identity(&credentials, &cache, |_| Ok(profile(ACCOUNT))).unwrap();
        let cached =
            verified_identity(&credentials, &cache, |_| panic!("unexpected cache miss")).unwrap();
        assert_eq!(first.key, cached.key);
        assert_eq!(cached.email.as_deref(), Some("fixture@example.test"));
        assert_eq!(cached.plan.as_deref(), Some("Pro"));
        let replacement = Credentials {
            access_token: "fixture-other-cached-token".into(),
            plan: None,
        };
        let other =
            verified_identity(&replacement, &cache, |_| Ok(profile(OTHER_ACCOUNT))).unwrap();
        assert_ne!(other.key, first.key);
    }

    #[test]
    fn maps_login_rate_limit_and_invalid_responses() {
        let now = unix_seconds(&json!(STAMP)).unwrap();
        let check = |status, retry| check_http_status(status, retry, now);
        assert!(check(StatusCode::OK, None).is_ok());
        assert!(matches!(
            check(StatusCode::UNAUTHORIZED, None),
            Err(FetchError::Login)
        ));
        assert!(matches!(
            check(StatusCode::FORBIDDEN, None),
            Err(FetchError::Login)
        ));
        assert!(matches!(
            check(StatusCode::TOO_MANY_REQUESTS, Some("47")),
            Err(FetchError::RateLimited(47))
        ));
        assert!(matches!(
            check(StatusCode::TOO_MANY_REQUESTS, None),
            Err(FetchError::RateLimited(300))
        ));
        assert!(matches!(
            check(
                StatusCode::TOO_MANY_REQUESTS,
                Some("Thu, 26 Nov 2026 14:00:30 GMT")
            ),
            Err(FetchError::RateLimited(30))
        ));
        assert!(matches!(
            check(StatusCode::SERVICE_UNAVAILABLE, None),
            Err(FetchError::Offline)
        ));
        let Err(FetchError::Invalid(error)) = check(StatusCode::BAD_REQUEST, None) else {
            panic!("expected invalid response")
        };
        assert_eq!(error, "Claude OAuth HTTP 400");
    }

    #[test]
    fn missing_and_expired_credentials_are_login_errors() {
        let home = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_credentials(home.path()),
            Err(FetchError::Login)
        ));
        fs::write(
            home.path().join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"fixture-expired","expiresAt":1}}"#,
        )
        .unwrap();
        assert!(matches!(
            read_credentials(home.path()),
            Err(FetchError::Login)
        ));
        fs::write(home.path().join(".credentials.json"), "broken fixture").unwrap();
        assert!(matches!(
            read_credentials(home.path()),
            Err(FetchError::Invalid(_))
        ));
    }

    #[test]
    fn parses_optional_fable_and_unexpired_reset_credits() {
        let now = unix_seconds(&json!(STAMP)).unwrap();
        let usage = json!({
            "five_hour": {"utilization": 101},
            "seven_day": {"utilization": 37.5},
            "seven_day_fable": {"utilization": 91},
            "limits": [
                {"kind":"weekly_scoped", "scope":{"model":{"display_name":"Opus"}}, "percent":80},
                {"kind":"daily_scoped", "scope":{"model":{"display_name":"Fable"}}, "percent":80},
                {"kind":"weekly_scoped", "scope":{"model":{"display_name":"Fable"}}, "percent":18, "resets_at":"2026-11-27T14:00:00Z"},
            ],
            "cedar_ember": {"eligible": true, "grants": [
                {"resets_left": 8, "ends_at": STAMP},
                {"resets_left": 1, "ends_at": "2026-11-28T14:00:00Z"},
                {"resets_left": 2, "ends_at": "2026-11-27T14:00:00Z"},
                {"resets_left": 101},
            ]},
        });
        let snapshot = parse_snapshot(&usage, None, now).unwrap();
        assert_eq!(snapshot.short.unwrap().remaining, 0.0);
        assert_eq!(snapshot.week.unwrap().remaining, 62.5);
        let fable = snapshot.fable.unwrap();
        assert_eq!(fable.remaining, 82.0);
        assert_eq!(fable.resets_at, Some(now + 86_400));
        let credits = snapshot.credits.unwrap();
        assert_eq!((credits.count, credits.expires_at), (3, Some(now + 86_400)));
        let absent =
            parse_snapshot(&json!({"seven_day_fable":{"utilization":91}}), None, now).unwrap();
        assert!(absent.fable.is_none() && absent.credits.is_none());
    }

    #[test]
    fn discovers_sorted_project_transcripts_without_following_links() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let projects = home.path().join("projects");
        let nested = projects.join("b-project/subagents");
        fs::create_dir_all(&nested).unwrap();
        let first = projects.join("a.JSONL");
        let second = nested.join("agent.jsonl");
        for file in [
            &first,
            &second,
            &nested.join("metadata.json"),
            &home.path().join("history.jsonl"),
            &outside.path().join("outside.jsonl"),
        ] {
            fs::write(file, "{}").unwrap();
        }
        let link = projects.join("redirected-project");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(outside.path(), &link).is_err() {
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", "$null = New-Item -ItemType Junction -Path $env:QUOTA_TEST_LINK -Target $env:QUOTA_TEST_TARGET -ErrorAction Stop"])
                .env("QUOTA_TEST_LINK", &link)
                .env("QUOTA_TEST_TARGET", outside.path())
                .output().unwrap();
            assert!(output.status.success(), "could not create fixture junction");
        }
        assert_eq!(
            log_files(home.path()),
            [
                first.canonicalize().unwrap(),
                second.canonicalize().unwrap()
            ]
        );
    }

    fn record(model: &str) -> Value {
        json!({"type":"assistant", "timestamp":STAMP, "requestId":"req_fixture", "sessionId":"synthetic-session",
            "message": {"id":"msg_fixture", "model":model, "content":[{"text":"fixture prompt data"}],
                "usage": {"input_tokens":1000, "output_tokens":100, "cache_read_input_tokens":300,
                    "cache_creation_input_tokens":300, "cache_creation": {"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":200}}}
        })
    }

    #[test]
    fn parses_cost_and_explicit_identity_and_deduplicates_across_json_roundtrip() {
        let mut line = record("claude-sonnet-4-6");
        line["accountUuid"] = json!(ACCOUNT.to_ascii_uppercase());
        line["organizationUuid"] = json!(ORGANIZATION);
        let mut state = Value::Null;
        let event = parse_line(&line.to_string(), &mut state).unwrap();
        assert_eq!(event.timestamp, unix_seconds(&json!(STAMP)).unwrap());
        assert_eq!(event.tokens, 1700);
        assert_eq!(
            event.identity.unwrap(),
            format!("claude:{ACCOUNT}:{ORGANIZATION}")
        );
        assert!((event.cost_usd.unwrap() - 0.006165).abs() < 1e-12);
        let stored = state.to_string();
        assert!(!stored.contains("fixture"));
        let mut restored = serde_json::from_str(&stored).unwrap();
        line["message"]["content"] = json!([{"text":"another fragment"}]);
        assert!(parse_line(&line.to_string(), &mut restored).is_none());
        assert_eq!(
            event.key,
            parse_line(&line.to_string(), &mut Value::Null).unwrap().key
        );
    }

    #[test]
    fn preserves_reported_cost_even_for_unknown_models() {
        let mut line = record("claude-fable-5");
        line["costUSD"] = json!(0.027);
        line["accountUuid"] = json!(ACCOUNT);
        let event = parse_line(&line.to_string(), &mut Value::Null).unwrap();
        assert_eq!(event.cost_usd, Some(0.027));
        assert!(event.identity.is_none());
        line["message"]["model"] = json!("claude-unknown-99");
        assert_eq!(
            parse_line(&line.to_string(), &mut Value::Null)
                .unwrap()
                .cost_usd,
            Some(0.027)
        );
        line["costUSD"] = json!(0.0);
        assert_eq!(
            parse_line(&line.to_string(), &mut Value::Null)
                .unwrap()
                .cost_usd,
            Some(0.0)
        );
        line["costUSD"] = Value::Null;
        assert!(
            parse_line(&line.to_string(), &mut Value::Null)
                .unwrap()
                .cost_usd
                .is_none()
        );
        line["type"] = json!("user");
        assert!(parse_line(&line.to_string(), &mut Value::Null).is_none());
        assert!(parse_line("{broken", &mut Value::Null).is_none());
    }

    #[test]
    fn legacy_usage_without_message_ids_gets_a_content_independent_key() {
        let mut line = record("claude-3-7-sonnet-20250219");
        line["requestId"] = Value::Null;
        line["message"]["id"] = Value::Null;
        let mut state = Value::Null;
        let event = parse_line(&line.to_string(), &mut state).unwrap();
        assert!((event.cost_usd.unwrap() - 0.00609).abs() < 1e-12);
        line["message"]["content"] = json!([{"text":"another fragment"}]);
        assert!(parse_line(&line.to_string(), &mut state).is_none());
        assert_eq!(
            event.key,
            parse_line(&line.to_string(), &mut Value::Null).unwrap().key
        );
    }
}
