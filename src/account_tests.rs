use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use std::{fs, io::Write, path::Path};
use tempfile::tempdir;

fn write_login(folder: &Path, user: &str) {
    let claims = json!({"sub":user,"email":"shared@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"workspace"}});
    let token = format!("header.{}.test", URL_SAFE_NO_PAD.encode(claims.to_string()));
    fs::write(
        folder.join("auth.json"),
        json!({"tokens":{"access_token":token,"account_id":"workspace"}}).to_string(),
    )
    .unwrap();
}
#[test]
fn profile_isolation_and_incremental_history() {
    let base = tempdir().unwrap();
    let dbpath = base.path().join("test.sqlite");
    let mut db = store::initialize(&dbpath).unwrap();
    let timestamp = Utc::now().to_rfc3339();
    let mut ids = vec![];
    for user in ["personal", "business"] {
        let folder = base.path().join(user);
        fs::create_dir_all(folder.join("sessions")).unwrap();
        write_login(&folder, user);
        let ident = codex::identity(&folder).unwrap();
        ids.push(
            store::registration(
                &db,
                Provider::Codex,
                &folder,
                &ident,
                user,
                user == "personal",
            )
            .unwrap(),
        );
        assert!(store::registration(&db, Provider::Codex, &folder, &ident, user, true).is_err());
        let log = folder.join("sessions/test.jsonl");
        let mut f = fs::File::create(&log).unwrap();
        writeln!(f,"{}",json!({"type":"session_meta","payload":{"id":"same-session","instructions":"secret-like-fixture"}})).unwrap();
        writeln!(
            f,
            "{}",
            json!({"type":"turn_context","payload":{"model":"unpriced-test"}})
        )
        .unwrap();
        let event = json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12},"total_token_usage":{"total_tokens":12}}}});
        write!(f, "{event}").unwrap();
        drop(f);
        let p = store::profiles(&db).unwrap().pop().unwrap();
        history::scan(&dbpath, &p).unwrap();
        assert!(
            store::days(&db, p.id)
                .unwrap()
                .iter()
                .all(|d| d.tokens.is_none())
        );
        writeln!(fs::OpenOptions::new().append(true).open(&log).unwrap()).unwrap();
        history::scan(&dbpath, &p).unwrap();
        fs::create_dir(folder.join("archived_sessions")).unwrap();
        fs::copy(&log, folder.join("archived_sessions/copied.jsonl")).unwrap();
        history::scan(&dbpath, &p).unwrap();
        let d = store::days(&db, p.id).unwrap().pop().unwrap();
        assert_eq!(d.tokens, if p.dedicated { Some(12) } else { None });
        assert!(d.cost.is_none());
        let states: String = db
            .query_row(
                "SELECT group_concat(state) FROM cursor WHERE owner=?",
                [p.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!states.contains("secret-like-fixture"));
    }
    store::move_profile(&mut db, ids[1], -1).unwrap();
    assert_eq!(store::profiles(&db).unwrap()[0].id, ids[1]);
    let p = store::profiles(&db).unwrap().pop().unwrap();
    write_login(&p.path, "different");
    assert!(matches!(
        history::verify(&p),
        Err(model::FetchError::IdentityChanged)
    ));
    db.execute("DELETE FROM profile WHERE id=?", [p.id])
        .unwrap();
    assert!(p.path.join("auth.json").is_file());
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage WHERE owner=?", [p.id], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0
    );
}
