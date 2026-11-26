use crate::{claude, codex, history, model::*, store};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Ready,
    Refresh {
        id: i64,
    },
    Reconnect {
        id: i64,
    },
    Add {
        provider: Provider,
        folder: PathBuf,
        alias: String,
        dedicated: bool,
    },
    Login {
        provider: Provider,
        alias: String,
    },
    CancelLogin,
    Rename {
        id: i64,
        alias: String,
    },
    Show {
        id: i64,
        visible: bool,
    },
    Move {
        id: i64,
        delta: i32,
    },
    Remove {
        id: i64,
    },
    Browse {
        provider: Provider,
    },
    Hide,
    Exit,
}
pub enum Message {
    User(Command),
    Tick,
    Rechecked(Profile, Result<Identity, FetchError>),
    Quota(Profile, Result<Snapshot, FetchError>),
    Scanned(i64, Result<(), String>),
    Connected(
        Provider,
        PathBuf,
        String,
        bool,
        Result<Identity, FetchError>,
    ),
}
pub enum Update {
    Accounts(Vec<store::AccountView>),
    Notice(String),
    Registered(Result<i64, String>),
}
pub fn start(
    path: PathBuf,
    db: Connection,
    notify: impl Fn(Update) + Send + 'static,
) -> Sender<Message> {
    let (tx, rx) = mpsc::channel();
    let mailbox = tx.clone();
    thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(18))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("TLS client");
        let mut service = Service {
            db,
            path,
            client,
            mailbox,
            inflight: HashSet::new(),
            scanning: HashSet::new(),
        };
        let _ = service.db.execute(
            "UPDATE profile SET status='갱신 대기',retry=0 WHERE locked=0 AND status='정상'",
            [],
        );
        let _ = service.refresh(None, false);
        let mut next_tick = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let wait = next_tick.saturating_duration_since(std::time::Instant::now());
            let msg = if wait.is_zero() {
                Message::Tick
            } else {
                match rx.recv_timeout(wait) {
                    Ok(msg) => msg,
                    Err(mpsc::RecvTimeoutError::Timeout) => Message::Tick,
                    Err(_) => break,
                }
            };
            if matches!(msg, Message::Tick) {
                next_tick = std::time::Instant::now() + Duration::from_secs(5);
            }
            match service.handle(msg, &notify) {
                Ok(false) => continue,
                Ok(true) => (),
                Err(error) => {
                    notify(Update::Notice(error));
                    continue;
                }
            }
            match store::views(&service.db) {
                Ok(items) => notify(Update::Accounts(items)),
                Err(_) => notify(Update::Notice("저장된 계정을 읽지 못했습니다.".into())),
            }
        }
    });
    tx
}
struct Service {
    db: Connection,
    path: PathBuf,
    client: reqwest::blocking::Client,
    mailbox: Sender<Message>,
    inflight: HashSet<i64>,
    scanning: HashSet<i64>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn status(e: &FetchError) -> String {
    match e {
        FetchError::Login => "CLI 로그인이 필요합니다.".into(),
        FetchError::IdentityChanged => "로그인 계정이 바뀌었습니다. 계정을 다시 연결하세요.".into(),
        FetchError::Offline => "연결할 수 없습니다.".into(),
        FetchError::RateLimited(_) => "조회 제한 · 잠시 후 다시 시도합니다.".into(),
        FetchError::Invalid(s) => s.clone(),
    }
}
impl Service {
    fn refresh(&mut self, id: Option<i64>, manual: bool) -> Result<bool, String> {
        let now = Utc::now().timestamp();
        let mut started = false;
        for p in store::profiles(&self.db).map_err(err)? {
            if id.is_some_and(|id| id != p.id) || self.inflight.contains(&p.id) {
                continue;
            }
            let Some(retry) = store::retry_at(&self.db, p.id).map_err(err)? else {
                continue;
            };
            if retry > now {
                let limited: bool = self
                    .db
                    .query_row(
                        "SELECT status LIKE '조회 제한%' FROM profile WHERE id=?",
                        [p.id],
                        |r| r.get(0),
                    )
                    .map_err(err)?;
                if manual && limited {
                    return Err(format!("조회 제한 · {}초 후 다시 시도하세요.", retry - now));
                }
                if !manual {
                    continue;
                }
            }
            self.inflight.insert(p.id);
            started = true;
            let tx = self.mailbox.clone();
            let client = self.client.clone();
            thread::spawn(move || {
                let result = history::verify(&p)
                    .and_then(|_| match p.provider {
                        Provider::Claude => claude::fetch(&p.path, &p.identity.key, &client),
                        Provider::Codex => codex::fetch(&p.path, &p.identity.key, &client),
                    })
                    .and_then(|snapshot| history::verify(&p).map(|_| snapshot));
                let _ = tx.send(Message::Quota(p, result));
            });
        }
        Ok(started)
    }
    fn handle(&mut self, msg: Message, notify: &impl Fn(Update)) -> Result<bool, String> {
        match msg {
            Message::Tick => return self.refresh(None, false),
            Message::Rechecked(p, result) => {
                let checked = result.map_err(|e| status(&e))?;
                if checked.key != p.identity.key {
                    return Err("이 폴더에 원래 계정으로 로그인한 후 다시 확인하세요.".into());
                }
                self.db.execute("UPDATE profile SET locked=0,retry=0,status='갱신 대기' WHERE id=? AND identity=?",params![p.id,checked.key]).map_err(err)?;
                self.refresh(Some(p.id), true)?;
            }
            Message::Connected(provider, folder, alias, dedicated, result) => {
                let registration = result.map_err(|e| status(&e)).and_then(|identity| {
                    store::registration(&self.db, provider, &folder, &identity, &alias, dedicated)
                });
                let id = registration.as_ref().ok().copied();
                notify(Update::Registered(registration));
                if let Some(id) = id {
                    self.refresh(Some(id), true)?;
                }
            }
            Message::Quota(p, result) => {
                self.inflight.remove(&p.id);
                if store::retry_at(&self.db, p.id).map_err(err)?.is_none() {
                    return Ok(false);
                }
                let now = Utc::now().timestamp();
                let can_scan =
                    !matches!(result, Err(FetchError::IdentityChanged | FetchError::Login));
                match result {
                    Ok(s) => {
                        self.db.execute("UPDATE profile SET snapshot=?,success=?,status='정상',retry=? WHERE id=? AND identity=?",params![serde_json::to_string(&s).map_err(err)?,now,now+300,p.id,p.identity.key]).map_err(err)?;
                    }
                    Err(e) => {
                        let changed = matches!(e, FetchError::IdentityChanged);
                        let after = match e {
                            FetchError::RateLimited(s) => s.clamp(1, 7 * 86400) as i64,
                            _ => 300,
                        };
                        self.db.execute("UPDATE profile SET status=?,retry=?,locked=?,dedicated=CASE WHEN ? THEN 0 ELSE dedicated END,snapshot=CASE WHEN ? THEN NULL ELSE snapshot END WHERE id=? AND identity=?",params![status(&e),now+after,changed,changed,changed,p.id,p.identity.key]).map_err(err)?;
                    }
                }
                if can_scan && self.scanning.insert(p.id) {
                    let path = self.path.clone();
                    let tx = self.mailbox.clone();
                    thread::spawn(move || {
                        let result = history::verify(&p).map_err(|e| status(&e)).and_then(|_| {
                            history::scan(&path, &p)
                                .map_err(|_| "일부 로그를 읽지 못했습니다.".into())
                        });
                        let _ = tx.send(Message::Scanned(p.id, result));
                    });
                }
            }
            Message::Scanned(id, r) => {
                self.scanning.remove(&id);
                self.db
                    .execute(
                        "UPDATE profile SET scan_note=? WHERE id=?",
                        params![r.err().unwrap_or_default(), id],
                    )
                    .map_err(err)?;
            }
            Message::User(command) => match command {
                Command::Ready => (),
                Command::Add {
                    provider,
                    folder,
                    alias,
                    dedicated,
                } => {
                    let tx = self.mailbox.clone();
                    thread::spawn(move || {
                        let result = history::identity(provider, &folder);
                        let _ = tx.send(Message::Connected(
                            provider, folder, alias, dedicated, result,
                        ));
                    });
                    return Ok(false);
                }
                Command::Reconnect { id } => {
                    let p = store::profiles(&self.db)
                        .map_err(err)?
                        .into_iter()
                        .find(|p| p.id == id)
                        .ok_or("등록된 계정이 없습니다.")?;
                    let tx = self.mailbox.clone();
                    thread::spawn(move || {
                        let result = history::verify(&p);
                        let _ = tx.send(Message::Rechecked(p, result));
                    });
                    return Ok(false);
                }
                Command::Refresh { id } => {
                    if self.inflight.contains(&id) {
                        return Err("이미 조회 중입니다.".into());
                    }
                    self.refresh(Some(id), true)?;
                    return Ok(false);
                }
                Command::Move { id, delta } => {
                    store::move_profile(&mut self.db, id, delta).map_err(err)?
                }
                Command::Show { id, visible } => {
                    self.db
                        .execute(
                            "UPDATE profile SET visible=? WHERE id=?",
                            params![visible, id],
                        )
                        .map_err(err)?;
                }
                Command::Rename { id, alias } => {
                    let name = alias.trim().chars().take(20).collect::<String>();
                    if name.is_empty() {
                        return Err("별칭을 입력하세요.".into());
                    }
                    self.db
                        .execute("UPDATE profile SET alias=? WHERE id=?", params![name, id])
                        .map_err(err)?;
                }
                Command::Remove { id } => {
                    self.db
                        .execute("DELETE FROM profile WHERE id=?", [id])
                        .map_err(err)?;
                }
                _ => return Ok(false),
            },
        }
        Ok(true)
    }
}
