use crate::model::Provider;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HANDLE,
    System::{JobObjects::*, Threading::CREATE_NO_WINDOW},
};

const CANCELLED: &str = "로그인을 취소했습니다.";
const FAILED: &str = "로그인을 마치지 못했습니다. 새 링크로 다시 시도하세요.";
const NO_LINK: &str =
    "CLI에서 로그인 링크를 받지 못했습니다. CLI를 업데이트하거나 로그인된 폴더를 선택하세요.";

fn cli_path(cli: &str) -> Result<PathBuf, String> {
    let path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .chain(env::var_os("APPDATA").map(|p| PathBuf::from(p).join("npm")))
        .chain(env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".local/bin")))
        .flat_map(|dir| {
            [
                dir.join(format!("{cli}.exe")),
                dir.join(format!("{cli}.cmd")),
            ]
        })
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{cli} CLI를 찾을 수 없습니다. CLI 설치 후 다시 시도하세요."))
}

// npm launchers have child processes. Closing the job releases the CLI's
// loopback listener on cancel, timeout, normal completion and Quota exit.
fn login_job(child: &Child) -> windows::core::Result<OwnedHandle> {
    unsafe {
        let handle = CreateJobObjectW(None, None)?;
        let job = OwnedHandle::from_raw_handle(handle.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of_val(&limits) as u32,
        )?;
        AssignProcessToJobObject(handle, HANDLE(child.as_raw_handle()))?;
        Ok(job)
    }
}

fn send(child: &mut Child, msg: Value) -> Result<(), String> {
    let pipe = child.stdin.as_mut().ok_or(FAILED)?;
    writeln!(pipe, "{msg}")
        .and_then(|_| pipe.flush())
        .map_err(|_| FAILED.into())
}
fn claude_request(id: &str, request: Value) -> Value {
    json!({"type":"control_request","request_id":id,"request":request})
}
fn check_link(provider: Provider, value: &Value) -> Result<&str, String> {
    let raw = value.as_str().ok_or(NO_LINK)?;
    let url = reqwest::Url::parse(raw).map_err(|_| NO_LINK)?;
    let trusted = match provider {
        Provider::Claude => matches!(
            url.host_str(),
            Some("claude.ai" | "claude.com" | "platform.claude.com")
        ),
        Provider::Codex => url.host_str() == Some("auth.openai.com"),
    };
    if trusted && url.scheme() == "https" && url.username().is_empty() && url.password().is_none() {
        Ok(raw)
    } else {
        Err(NO_LINK.into())
    }
}

fn new_folder(parent: &Path, cli: &str) -> Result<tempfile::TempDir, String> {
    fs::create_dir_all(parent).map_err(|_| "프로필 폴더를 만들 수 없습니다.")?;
    tempfile::Builder::new()
        .prefix(&format!("{cli}-"))
        .tempdir_in(parent)
        .map_err(|_| "새 프로필 폴더를 만들 수 없습니다.".into())
}

pub fn sign_in(
    provider: Provider,
    parent: &Path,
    cancel: &AtomicBool,
    offer: impl Fn(&str),
) -> Result<tempfile::TempDir, String> {
    let (cli, variable, args): (_, _, &[&str]) = match provider {
        Provider::Codex => (
            "codex",
            "CODEX_HOME",
            &["app-server", "-c", "cli_auth_credentials_store=file"],
        ),
        Provider::Claude => (
            "claude",
            "CLAUDE_CONFIG_DIR",
            &[
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--tools",
                "",
            ],
        ),
    };
    let program = cli_path(cli)?;
    let folder = new_folder(parent, cli)?;
    let mut child = Command::new(program)
        .args(args)
        .env(variable, folder.path())
        .current_dir(folder.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map_err(|_| format!("{cli} CLI를 시작할 수 없습니다."))?;
    let job = match login_job(&child) {
        Ok(job) => job,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("로그인 프로세스를 관리할 수 없습니다.".into());
        }
    };
    let result = exchange(provider, &mut child, cancel, offer);
    drop(child.stdin.take());
    drop(job);
    let _ = child.wait();
    result.map(|()| folder)
}

fn exchange(
    provider: Provider,
    child: &mut Child,
    cancel: &AtomicBool,
    offer: impl Fn(&str),
) -> Result<(), String> {
    let stdout = child.stdout.take().ok_or(NO_LINK)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && tx.send(message).is_err()
            {
                break;
            }
        }
    });
    let is_codex = provider == Provider::Codex;
    let init = if is_codex {
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"quota","version":env!("CARGO_PKG_VERSION")}}})
    } else {
        claude_request("init", json!({"subtype":"initialize"}))
    };
    send(child, init)?;
    let mut login_id = None;
    let mut offered = false;
    let mut deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        if Instant::now() >= deadline {
            return Err(if offered {
                "로그인 시간이 만료됐습니다. 새 링크를 받으세요."
            } else {
                NO_LINK
            }
            .into());
        }
        let msg = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(msg) => msg,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(if offered { FAILED } else { NO_LINK }.into());
            }
        };
        let url = if is_codex {
            match msg["id"].as_i64() {
                Some(1) if msg.get("result").is_some() => {
                    send(child, json!({"method":"initialized"}))?;
                    send(
                        child,
                        json!({"id":2,"method":"account/login/start","params":{"type":"chatgpt"}}),
                    )?;
                    continue;
                }
                Some(2) if msg["result"]["type"] == "chatgpt" => {
                    login_id = Some(msg["result"]["loginId"].as_str().ok_or(NO_LINK)?.to_owned());
                    &msg["result"]["authUrl"]
                }
                Some(1 | 2) => return Err(NO_LINK.into()),
                _ if offered
                    && msg["method"] == "account/login/completed"
                    && msg["params"]["loginId"].as_str() == login_id.as_deref() =>
                {
                    return if msg["params"]["success"] == true {
                        Ok(())
                    } else {
                        Err(FAILED.into())
                    };
                }
                _ => continue,
            }
        } else {
            if msg["type"] != "control_response" {
                continue;
            }
            let reply = &msg["response"];
            match reply["request_id"].as_str() {
                Some("init") if reply["subtype"] == "success" => {
                    send(
                        child,
                        claude_request(
                            "login",
                            json!({"subtype":"claude_authenticate","loginWithClaudeAi":true}),
                        ),
                    )?;
                    continue;
                }
                Some("login") if reply["subtype"] == "success" => {
                    send(
                        child,
                        claude_request(
                            "finish",
                            json!({"subtype":"claude_oauth_wait_for_completion"}),
                        ),
                    )?;
                    &reply["response"]["automaticUrl"]
                }
                Some("finish") if offered && reply["subtype"] == "success" => return Ok(()),
                Some("init" | "login") => return Err(NO_LINK.into()),
                Some("finish") => return Err(FAILED.into()),
                _ => continue,
            }
        };
        offer(check_link(provider, url)?);
        offered = true;
        deadline = Instant::now() + Duration::from_secs(10 * 60);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_uses_a_new_folder_and_cancel_preserves_registered_profiles() {
        let root = tempfile::tempdir().unwrap();
        let first = new_folder(root.path(), "codex").unwrap().keep();
        fs::write(first.join("auth.json"), b"first").unwrap();
        let retry = new_folder(root.path(), "codex").unwrap();
        let retry_path = retry.path().to_owned();
        assert_ne!(first, retry_path);
        drop(retry);
        assert!(!retry_path.exists());
        assert_eq!(fs::read(first.join("auth.json")).unwrap(), b"first");
    }

    #[test]
    fn links_keep_query_parameters_and_match_the_provider() {
        let url = json!("https://auth.openai.com/oauth/authorize?state=a%2Fb");
        assert_eq!(
            check_link(Provider::Codex, &url).unwrap(),
            url.as_str().unwrap()
        );
        assert!(check_link(Provider::Claude, &url).is_err());
        assert!(check_link(Provider::Codex, &json!("https://auth.openai.com.invalid/")).is_err());
        assert!(
            check_link(
                Provider::Claude,
                &json!("https://claude.com/cai/oauth/authorize")
            )
            .is_ok()
        );
    }
}
