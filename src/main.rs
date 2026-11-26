#![windows_subsystem = "windows"]
#[cfg(test)]
mod account_tests;
mod claude;
mod codex;
mod history;
mod login;
mod model;
mod service;
mod store;
mod taskbar;

use model::{HostAction, Provider, Tile};
use serde_json::{Value, json};
use service::{Command, Message, Update};
use std::{
    env,
    error::Error,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tao::{
    dpi::{LogicalSize, PhysicalPosition},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::WindowBuilderExtWindows,
    window::WindowBuilder,
};
use windows::{
    Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*},
    core::{HSTRING, w},
};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

enum Ui {
    Host(HostAction),
    Command(Command),
    Update(Update),
    LoginLink(String),
    LoginFinished(Provider, String, Result<tempfile::TempDir, String>),
}
static EXIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn post(proxy: &EventLoopProxy<Ui>, e: Ui) {
    let _ = proxy.send_event(e);
}
fn instance_names(root: &std::path::Path) -> (HSTRING, HSTRING) {
    use sha2::{Digest, Sha256};
    let key = format!(
        "{:x}",
        Sha256::digest(root.to_string_lossy().to_lowercase().as_bytes())
    );
    let open = HSTRING::from(format!("Local\\Quota.open.{key}"));
    let quit = HSTRING::from(format!("Local\\Quota.quit.{key}"));
    (open, quit)
}
fn signal_instance(
    proxy: EventLoopProxy<Ui>,
    open: HSTRING,
    quit: HSTRING,
) -> Result<bool, Box<dyn Error>> {
    unsafe {
        let activate = CreateEventW(None, false, false, &open)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            SetEvent(activate)?;
            CloseHandle(activate)?;
            return Ok(false);
        }
        let exit = CreateEventW(None, false, false, &quit)?;
        let raw = [activate.0 as usize, exit.0 as usize];
        std::thread::spawn(move || {
            let handles = raw.map(|h| HANDLE(h as *mut _));
            loop {
                let event = WaitForMultipleObjects(&handles, false, INFINITE);
                if event == WAIT_OBJECT_0 {
                    post(&proxy, Ui::Host(HostAction::Settings));
                } else {
                    if event.0 == WAIT_OBJECT_0.0 + 1 {
                        EXIT_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
                        post(&proxy, Ui::Host(HostAction::Exit));
                    }
                    break;
                }
            }
            for h in handles {
                let _ = CloseHandle(h);
            }
        });
    }
    Ok(true)
}
fn html() -> String {
    let script = include_str!("app.js")
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
        .replace(
            "\"__CLAUDE__\"",
            &json!(include_str!("../claude.svg")).to_string(),
        )
        .replace(
            "\"__CODEX__\"",
            &json!(include_str!("../openai.svg")).to_string(),
        );
    include_str!("app.html").replace("/*APP_JS*/", &script)
}
fn js(web: &WebView, method: &str, arg: Value) {
    let _ = web.evaluate_script(&format!("window.quota?.{method}({arg})"));
}
fn default_folder(provider: Provider) -> PathBuf {
    let (var, dir) = match provider {
        Provider::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
        Provider::Codex => ("CODEX_HOME", ".codex"),
    };
    let folder = env::var_os(var)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("USERPROFILE").unwrap_or_default()).join(dir));
    selected_path(folder)
}
fn selected_path(path: PathBuf) -> PathBuf {
    match std::fs::canonicalize(&path) {
        Ok(p) => PathBuf::from(store::display_path(&p)),
        Err(_) => path,
    }
}
fn position_panel(window: &tao::window::Window) {
    let mut area = RECT::default();
    let r = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&raw mut area).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if r.is_ok() {
        let size = window.outer_size();
        window.set_outer_position(PhysicalPosition::new(
            (area.right - size.width as i32 - 8).max(area.left),
            (area.bottom - size.height as i32 - 8).max(area.top),
        ));
    }
}
struct PanelOwner(HWND);

impl PanelOwner {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
            .map(Self)
        }
    }
}

impl Drop for PanelOwner {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let root = env::var_os("QUOTA_DATA_DIR").map(PathBuf::from).unwrap_or(
        PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA missing")?)
            .join("TaskbarAiQuota"),
    );
    let (activate, quit) = instance_names(&root);
    if env::args().any(|s| s == "--shutdown") {
        unsafe {
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &quit) {
                SetEvent(event)?;
                CloseHandle(event)?;
            }
        }
        return Ok(());
    }
    let events = EventLoopBuilder::<Ui>::with_user_event().build();
    if !signal_instance(events.create_proxy(), activate, quit)? {
        return Ok(());
    }
    std::fs::create_dir_all(&root)?;
    let path = root.join("quota.sqlite3");
    let db = store::initialize(&path)?;
    // A hidden owner keeps the panel out of the taskbar when Explorer sees it appear.
    // Tao's skip flag removes the shell tab but leaves the native window styles alone.
    let panel_owner = PanelOwner::new()?;
    let window = WindowBuilder::new()
        .with_title("Quota · AI 할당량")
        .with_inner_size(LogicalSize::new(425, 505))
        .with_min_inner_size(LogicalSize::new(340, 345))
        .with_visible(false)
        .with_owner_window(panel_owner.0.0 as isize)
        .with_skip_taskbar(true)
        .build(&events)?;
    let p = events.create_proxy();
    let mut host = taskbar::Host::new(move |a| post(&p, Ui::Host(a)))?;
    let p = events.create_proxy();
    let service = service::start(path, db, move |m| post(&p, Ui::Update(m)));
    let mut context = WebContext::new(Some(root.join("WebView2")));
    let p = events.create_proxy();
    let web = WebViewBuilder::new_with_web_context(&mut context)
        .with_custom_protocol("quota".into(), |_, request| {
            let is_root = request.uri().path() == "/";
            let body = if is_root { html() } else { String::new() };
            wry::http::Response::builder()
                .status(if is_root { 200 } else { 404 })
                .header("Content-Type", "text/html; charset=utf-8")
                .header("Cache-Control", "no-store")
                .body(std::borrow::Cow::Owned(body.into_bytes()))
                .unwrap()
        })
        .with_url("quota://localhost/")
        .with_devtools(false)
        .with_navigation_handler(|url| {
            matches!(
                url.as_str(),
                "http://quota.localhost/" | "quota://localhost/"
            )
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_ipc_handler(move |req| {
            if let Ok(command) = serde_json::from_str(req.body()) {
                post(&p, Ui::Command(command));
            }
        })
        .build(&window)?;
    let defaults =
        json!({"Claude":default_folder(Provider::Claude),"Codex":default_folder(Provider::Codex)});
    let mut selected_folders = std::collections::HashMap::new();
    let login_proxy = events.create_proxy();
    let profiles_dir = root.join("profiles");
    let mut login_cancel: Option<Arc<AtomicBool>> = None;
    let mut pending_open: Option<Value> = None;
    let mut presented = false;
    let mut ready = false;
    events.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Ui::Host(HostAction::Exit) | Ui::Command(Command::Exit)) => {
                if let Some(cancel) = &login_cancel {
                    cancel.store(true, Ordering::Release);
                }
                *flow = ControlFlow::Exit
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(Ui::Command(Command::Hide)) => window.set_visible(false),
            Event::UserEvent(Ui::Host(action)) => {
                let id = match action {
                    HostAction::Open(id) => json!(id),
                    _ => Value::Null,
                };
                if ready {
                    js(&web, "open", id)
                } else {
                    pending_open = Some(id)
                }
                position_panel(&window);
                window.set_visible(true);
                window.set_minimized(false);
                window.set_focus();
            }
            Event::UserEvent(Ui::Command(Command::Ready)) => {
                ready = true;
                js(&web, "defaults", defaults.clone());
                let _ = service.send(Message::User(Command::Ready));
            }
            Event::UserEvent(Ui::Command(Command::Browse { provider })) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("로그인된 CLI 프로필 폴더")
                    .pick_folder()
                {
                    let path = selected_path(path);
                    let _ = web.evaluate_script(&format!(
                        "quota.folder({},{})",
                        json!(provider),
                        json!(path)
                    ));
                    selected_folders.insert(store::provider_name(provider), path);
                }
            }
            Event::UserEvent(Ui::Command(Command::Login { provider, alias })) => {
                if login_cancel.is_none() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    login_cancel = Some(cancel.clone());
                    let parent = profiles_dir.clone();
                    let proxy = login_proxy.clone();
                    std::thread::spawn(move || {
                        let result = login::sign_in(provider, &parent, &cancel, |link| {
                            post(&proxy, Ui::LoginLink(link.to_owned()));
                        });
                        post(&proxy, Ui::LoginFinished(provider, alias, result));
                    });
                }
            }
            Event::UserEvent(Ui::LoginLink(link)) => {
                if login_cancel
                    .as_ref()
                    .is_some_and(|c| !c.load(Ordering::Acquire))
                {
                    js(&web, "loginLink", json!(link));
                }
            }
            Event::UserEvent(Ui::Command(Command::CancelLogin)) => {
                if let Some(cancel) = &login_cancel {
                    cancel.store(true, Ordering::Release);
                }
            }
            Event::UserEvent(Ui::LoginFinished(provider, alias, result)) => {
                if login_cancel
                    .take()
                    .is_some_and(|c| c.load(Ordering::Acquire))
                {
                    js(&web, "registrationError", json!("로그인을 취소했습니다."));
                    return;
                }
                js(&web, "clearLogin", Value::Null);
                match result {
                    Ok(folder) => {
                        let folder = selected_path(folder.keep());
                        selected_folders.insert(store::provider_name(provider), folder.clone());
                        let _ = web.evaluate_script(&format!(
                            "quota.folder({},{},true)",
                            json!(provider),
                            json!(folder)
                        ));
                        let _ = service.send(Message::User(Command::Add {
                            provider,
                            folder,
                            alias,
                            dedicated: true,
                        }));
                    }
                    Err(text) => js(&web, "registrationError", json!(text)),
                }
            }
            Event::UserEvent(Ui::Command(cmd)) => {
                if let Command::Add {
                    provider, folder, ..
                } = &cmd
                    && folder != &default_folder(*provider)
                    && selected_folders.get(store::provider_name(*provider)) != Some(folder)
                {
                    js(
                        &web,
                        "registrationError",
                        json!("프로필 폴더를 다시 선택하세요."),
                    );
                    return;
                }
                let _ = service.send(Message::User(cmd));
            }
            Event::UserEvent(Ui::Update(Update::Accounts(items))) => {
                host.update(
                    items
                        .iter()
                        .filter(|a| a.visible)
                        .map(|a| {
                            let s = a.snapshot.as_ref();
                            Tile {
                                stale: a.status != "정상",
                                id: a.id,
                                provider: a.provider,
                                alias: a.alias.clone(),
                                week: s.and_then(|x| x.week.clone()),
                                short: s.and_then(|x| x.short.clone()),
                                fable: s.and_then(|x| x.fable.clone()),
                            }
                        })
                        .collect(),
                );
                if ready {
                    js(&web, "state", json!(items));
                    if let Some(id) = pending_open.take() {
                        js(&web, "open", id)
                    }
                    if !presented {
                        presented = true;
                        position_panel(&window);
                        window.set_visible(true);
                        window.set_focus();
                    }
                }
            }
            Event::UserEvent(Ui::Update(Update::Notice(text))) => js(&web, "notice", json!(text)),
            Event::UserEvent(Ui::Update(Update::Registered(Ok(id)))) => {
                js(&web, "registered", json!(id))
            }
            Event::UserEvent(Ui::Update(Update::Registered(Err(text)))) => {
                js(&web, "registrationError", json!(text))
            }
            _ => (),
        }
    })
}
fn main() {
    if let Err(e) = run() {
        if env::args().any(|s| s == "--shutdown") {
            std::process::exit(1)
        }
        let message = HSTRING::from(e.to_string());
        let dialog = std::thread::spawn(move || unsafe {
            MessageBoxW(None, &message, w!("Quota 시작 오류"), MB_OK | MB_ICONERROR);
        });
        while !dialog.is_finished() && !EXIT_REQUESTED.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::process::exit(1);
    }
}
