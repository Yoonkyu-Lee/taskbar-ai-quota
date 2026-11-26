use crate::{claude, codex, model::*, store};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

pub fn identity(p: Provider, path: &Path) -> Result<Identity, FetchError> {
    match p {
        Provider::Claude => claude::identity(path),
        Provider::Codex => codex::identity(path),
    }
}
pub fn verify(p: &Profile) -> Result<Identity, FetchError> {
    let identity = identity(p.provider, &p.path)?;
    if identity.key != p.identity.key {
        return Err(FetchError::IdentityChanged);
    }
    Ok(identity)
}
type Error = Box<dyn std::error::Error + Send + Sync>;
pub fn scan(dbfile: &Path, p: &Profile) -> Result<(), Error> {
    let mut db = store::connect(dbfile)?;
    let files = match p.provider {
        Provider::Claude => claude::log_files(&p.path),
        Provider::Codex => codex::log_files(&p.path),
    };
    let cutoff = Utc::now().timestamp() - 32 * 86400;
    for path in files {
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        if !path.starts_with(&p.path) {
            continue;
        }
        let mut file = File::open(&path)?;
        let meta = file.metadata()?;
        if meta
            .modified()?
            .elapsed()
            .is_ok_and(|d| d.as_secs() > 33 * 86400)
        {
            continue;
        }
        let path_key = path.to_string_lossy().to_lowercase();
        let mut head = [0u8; 256];
        let len = file.read(&mut head)?;
        let fingerprint = Sha256::digest(&head[..len]).to_vec();
        let previous: Option<(Vec<u8>, u64, String)> = db
            .query_row(
                "SELECT fingerprint,offset,state FROM cursor WHERE owner=? AND path=?",
                params![p.id, path_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (mut offset, mut state) = match previous {
            Some((fp, pos, json)) if pos <= meta.len() && fp == fingerprint => {
                (pos, serde_json::from_str(&json)?)
            }
            _ => (0, Value::Null),
        };
        if offset == meta.len() {
            continue;
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file.take(meta.len() - offset));
        let mut line = Vec::new();
        loop {
            let start = offset;
            let mut events = Vec::new();
            while offset - start < 4 * 1024 * 1024 {
                line.clear();
                let n = reader.read_until(b'\n', &mut line)?;
                // A running CLI may still be appending this record.
                if n == 0 || line.last() != Some(&b'\n') {
                    break;
                }
                offset += n as u64;
                let Ok(s) = std::str::from_utf8(&line) else {
                    continue;
                };
                let event = match p.provider {
                    Provider::Claude => claude::parse_line(s, &mut state),
                    Provider::Codex => codex::parse_line(s, &mut state),
                };
                if let Some(event) = event
                    && event.timestamp >= cutoff
                    && attributed(&event, p)
                {
                    events.push(event)
                }
            }
            if offset == start {
                break;
            }
            if verify(p).is_err() {
                return Err("이력 수집 중 프로필 신원이 변경되었거나 확인되지 않습니다.".into());
            }
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let eligible: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM profile WHERE id=? AND identity=? AND dedicated=? AND locked=0)",params![p.id,p.identity.key,p.dedicated],|row|row.get(0))?;
            if !eligible {
                return Ok(());
            }
            for e in events {
                tx.execute("INSERT OR IGNORE INTO usage(owner,event_key,stamp,tokens,cost) VALUES(?,?,?,?,?)",params![p.id,e.key,e.timestamp,e.tokens,e.cost_usd])?;
            }
            tx.execute("INSERT OR REPLACE INTO cursor(owner,path,fingerprint,offset,state) VALUES(?,?,?,?,?)",params![p.id,path_key,fingerprint,offset,serde_json::to_string(&state)?])?;
            tx.commit()?;
        }
    }
    db.execute(
        "DELETE FROM usage WHERE owner=? AND stamp<?",
        params![p.id, cutoff],
    )?;
    Ok(())
}
fn attributed(e: &UsageEvent, p: &Profile) -> bool {
    match &e.identity {
        Some(key) => *key == p.identity.key,
        None => p.dedicated,
    }
}
