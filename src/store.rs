use crate::model::*;
use chrono::{Days, Local, TimeZone};
use rusqlite::{Connection, OptionalExtension, Result, params};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn connect(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(8))?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    Ok(db)
}
pub fn initialize(path: &Path) -> Result<Connection> {
    let db = connect(path)?;
    db.execute_batch("PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS profile(
      id INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT NOT NULL, path TEXT NOT NULL, path_key TEXT UNIQUE NOT NULL,
      identity TEXT NOT NULL, email TEXT, plan TEXT, alias TEXT NOT NULL, visible INTEGER NOT NULL DEFAULT 1,
      dedicated INTEGER NOT NULL, pos INTEGER NOT NULL, snapshot TEXT, success INTEGER, status TEXT NOT NULL DEFAULT '대기 중',
      retry INTEGER NOT NULL DEFAULT 0, locked INTEGER NOT NULL DEFAULT 0, scan_note TEXT NOT NULL DEFAULT '',
      UNIQUE(provider,identity));
    CREATE TABLE IF NOT EXISTS cursor(
      owner INTEGER REFERENCES profile(id) ON DELETE CASCADE, path TEXT, fingerprint BLOB, offset INTEGER, state TEXT,
      PRIMARY KEY(owner,path));
    CREATE TABLE IF NOT EXISTS usage(
      owner INTEGER REFERENCES profile(id) ON DELETE CASCADE,event_key TEXT, stamp INTEGER NOT NULL,
      tokens INTEGER NOT NULL,cost REAL,PRIMARY KEY(owner,event_key));
    CREATE INDEX IF NOT EXISTS usage_date ON usage(owner,stamp);")?;
    Ok(db)
}
pub fn provider_name(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
    }
}
pub fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.trim_start_matches(r"\\?\").to_owned()
    }
}
pub fn registration(
    db: &Connection,
    p: Provider,
    folder: &Path,
    identity: &Identity,
    alias: &str,
    dedicated: bool,
) -> Result<i64, String> {
    if identity.key.trim().is_empty() {
        return Err("로그인 계정을 식별할 수 없습니다.".into());
    }
    let path = fs::canonicalize(folder).map_err(|_| "프로필 폴더를 찾을 수 없습니다.")?;
    if !path.is_dir() {
        return Err("프로필 폴더를 선택하세요.".into());
    }
    let name = alias.trim().chars().take(20).collect::<String>();
    let name = if name.is_empty() {
        provider_name(p)
    } else {
        &name
    };
    let value = path.to_str().ok_or("지원하지 않는 폴더 이름입니다.")?;
    db.execute(
        "INSERT INTO profile(provider,path,path_key,identity,email,plan,alias,dedicated,pos)
      VALUES(?,?,?,?,?,?,?,?,(SELECT coalesce(max(pos),0)+1 FROM profile))",
        params![
            provider_name(p),
            value,
            value.to_lowercase(),
            identity.key,
            identity.email,
            identity.plan,
            name,
            dedicated
        ],
    )
    .map_err(|e| {
        if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
            if let Ok(existing) = db.query_row(
                "SELECT alias FROM profile WHERE path_key=?",
                [value.to_lowercase()], |r| r.get::<_, String>(0)
            ) {
                format!("이 폴더는 이미 ‘{existing}’에 연결되어 있습니다. 다른 계정으로 로그인하거나 해당 계정의 폴더를 선택하세요.")
            } else if let Ok(existing) = db.query_row(
                "SELECT alias FROM profile WHERE provider=? AND identity=?",
                params![provider_name(p), identity.key], |r| r.get::<_, String>(0)
            ) {
                format!("방금 확인한 계정은 이미 ‘{existing}’로 연결되어 있습니다. 로그인 화면에서 다른 이메일의 계정을 선택하세요.")
            } else {
                e.to_string()
            }
        } else {
            e.to_string()
        }
    })?;
    Ok(db.last_insert_rowid())
}
pub fn profiles(db: &Connection) -> Result<Vec<Profile>> {
    let mut stmt=db.prepare("SELECT id,provider,path,identity,email,plan,alias,visible,dedicated FROM profile ORDER BY pos,id")?;
    stmt.query_map([], |r| {
        Ok(Profile {
            id: r.get(0)?,
            provider: if r.get::<_, String>(1)? == "Claude" {
                Provider::Claude
            } else {
                Provider::Codex
            },
            path: PathBuf::from(r.get::<_, String>(2)?),
            identity: Identity {
                key: r.get(3)?,
                email: r.get(4)?,
                plan: r.get(5)?,
            },
            alias: r.get(6)?,
            visible: r.get(7)?,
            dedicated: r.get(8)?,
        })
    })?
    .collect()
}
pub fn move_profile(db: &mut Connection, id: i64, delta: i32) -> Result<()> {
    let ids = profiles(db)?.into_iter().map(|p| p.id).collect::<Vec<_>>();
    let Some(index) = ids.iter().position(|x| *x == id) else {
        return Ok(());
    };
    let Some(&other) = ids.get(index.saturating_add_signed(delta.signum() as isize)) else {
        return Ok(());
    };
    let tx = db.transaction()?;
    let pos: i64 = tx.query_row("SELECT pos FROM profile WHERE id=?", [id], |r| r.get(0))?;
    tx.execute(
        "UPDATE profile SET pos=(SELECT pos FROM profile WHERE id=?) WHERE id=?",
        params![other, id],
    )?;
    tx.execute("UPDATE profile SET pos=? WHERE id=?", params![pos, other])?;
    tx.commit()
}
#[derive(Debug, Serialize)]
pub struct Day {
    pub date: String,
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
}
#[derive(Serialize)]
pub struct AccountView {
    pub id: i64,
    pub provider: Provider,
    pub alias: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub visible: bool,
    pub dedicated: bool,
    pub folder: String,
    pub snapshot: Option<Snapshot>,
    pub success: Option<i64>,
    pub status: String,
    pub locked: bool,
    pub scan_note: String,
    pub days: Vec<Day>,
}
pub fn days(db: &Connection, id: i64) -> Result<Vec<Day>> {
    let today = Local::now().date_naive();
    let start = today.checked_sub_days(Days::new(29)).unwrap();
    let mut days = (0..30)
        .map(|n| Day {
            date: (start + Days::new(n)).to_string(),
            tokens: None,
            cost: None,
        })
        .collect::<Vec<_>>();
    let since = Local
        .from_local_datetime(&start.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| start.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - 86400);
    let mut stmt=db.prepare("SELECT date(stamp,'unixepoch','localtime'),sum(tokens),CASE WHEN count(cost)=count(*) THEN sum(cost) END FROM usage WHERE owner=? AND stamp>=? GROUP BY 1")?;
    for row in stmt.query_map(params![id, since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, Option<f64>>(2)?,
        ))
    })? {
        let (date, tokens, cost) = row?;
        if let Some(d) = days.iter_mut().find(|d| d.date == date) {
            d.tokens = Some(tokens);
            d.cost = cost;
        }
    }
    Ok(days)
}
pub fn views(db: &Connection) -> Result<Vec<AccountView>> {
    profiles(db)?
        .into_iter()
        .map(|p| {
            let (raw, success, status, locked, scan_note): (
                Option<String>,
                Option<i64>,
                String,
                bool,
                String,
            ) = db.query_row(
                "SELECT snapshot,success,status,locked,scan_note FROM profile WHERE id=?",
                [p.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let snapshot: Option<Snapshot> =
                raw.as_deref().and_then(|s| serde_json::from_str(s).ok());
            Ok(AccountView {
                id: p.id,
                provider: p.provider,
                alias: p.alias,
                email: p.identity.email,
                plan: snapshot
                    .as_ref()
                    .and_then(|s| s.plan.clone())
                    .or(p.identity.plan),
                visible: p.visible,
                folder: display_path(&p.path),
                dedicated: p.dedicated,
                snapshot,
                success,
                status,
                locked,
                scan_note,
                days: days(db, p.id)?,
            })
        })
        .collect()
}
pub fn retry_at(db: &Connection, id: i64) -> Result<Option<i64>> {
    db.query_row(
        "SELECT retry FROM profile WHERE id=? AND locked=0",
        [id],
        |r| r.get(0),
    )
    .optional()
}
