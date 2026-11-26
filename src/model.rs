use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub key: String,
    pub email: Option<String>,
    pub plan: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limit {
    pub remaining: f64,
    pub resets_at: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credits {
    pub count: u32,
    pub expires_at: Option<i64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub week: Option<Limit>,
    pub short: Option<Limit>,
    pub fable: Option<Limit>,
    pub credits: Option<Credits>,
    pub plan: Option<String>,
}
#[derive(Debug, Clone)]
pub enum FetchError {
    Login,
    IdentityChanged,
    Offline,
    RateLimited(u64),
    Invalid(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: i64,
    pub provider: Provider,
    pub path: PathBuf,
    pub identity: Identity,
    pub alias: String,
    pub visible: bool,
    pub dedicated: bool,
}
#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub key: String,
    pub timestamp: i64,
    pub tokens: u64,
    pub cost_usd: Option<f64>,
    pub identity: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Tile {
    pub stale: bool,
    pub id: i64,
    pub provider: Provider,
    pub alias: String,
    pub week: Option<f64>,
    pub short: Option<f64>,
    pub fable: Option<f64>,
}
#[derive(Clone, Debug)]
pub enum HostAction {
    Open(i64),
    Settings,
    Exit,
}
