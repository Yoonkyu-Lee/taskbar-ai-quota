use crate::model::{HostAction, Provider, Tile};
use std::{
    cell::RefCell,
    f32::consts::{PI, TAU},
    mem::size_of,
    rc::Rc,
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::*,
        },
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::{Accessibility::*, HiDpi::*, Shell::*, WindowsAndMessaging::*},
    },
    core::{Error, PCWSTR, Result as WinResult, w},
};

const WINDOW_CLASS: PCWSTR = w!("Quota.TaskbarOverlay");
const TRAY_MESSAGE: u32 = WM_APP + 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TIMER_ID: usize = 1;
const TRAY_ID: u32 = 1;
const TILE_WIDTH: i32 = 36;
const HEIGHT: i32 = 45;
const GROUP_GAP: i32 = 11;
const OVERFLOW_GAP: i32 = 5;
const TRAY_GAP: i32 = 4;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(350);
const MAX_SAMPLE_AGE: Duration = Duration::from_millis(1100);

/// Owns the overlay and notification icon. Create, update and drop on Tao's GUI thread.
pub struct Host {
    window: HWND,
    instance: HINSTANCE,
    state: Rc<State>,
}

impl Host {
    pub fn new(action: impl Fn(HostAction) + 'static) -> Result<Self, String> {
        Self::create(action).map_err(|error| error.to_string())
    }

    fn create(action: impl Fn(HostAction) + 'static) -> WinResult<Self> {
        let state = Rc::new(State {
            action: Box::new(action),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            icon: TrayIcon::new()?,
            overlay: RefCell::new(Overlay::new()?),
        });
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: WINDOW_CLASS,
            lpfnWndProc: Some(window_proc),
            hCursor: unsafe { LoadCursorW(None, IDC_HAND)? },
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(Error::from_win32());
        }
        let window = {
            let _dpi = DpiContext::per_monitor();
            unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                    WINDOW_CLASS,
                    w!("Quota"),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    Some(instance),
                    None,
                )
            }
        };
        let window = window.inspect_err(|_| unsafe {
            let _ = UnregisterClassW(WINDOW_CLASS, Some(instance));
        })?;
        let host = Self {
            window,
            instance,
            state,
        };
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, Rc::as_ptr(&host.state) as isize);
            if SetTimer(Some(window), TIMER_ID, 100, None) == 0 {
                return Err(Error::from_win32());
            }
        }
        host.state.refresh(window);
        Ok(host)
    }

    pub fn update(&mut self, tiles: Vec<Tile>) {
        {
            let mut overlay = self.state.overlay.borrow_mut();
            overlay.layout = Layout::new(&tiles);
            overlay.tiles = tiles;
            overlay.hover = None;
            overlay.dirty = true;
        }
        self.state.refresh(self.window);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = KillTimer(Some(self.window), TIMER_ID);
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.state.icon.data(self.window));
            let _ = DestroyWindow(self.window);
            let _ = UnregisterClassW(WINDOW_CLASS, Some(self.instance));
        }
    }
}

struct State {
    action: Box<dyn Fn(HostAction)>,
    taskbar_created: u32,
    icon: TrayIcon,
    overlay: RefCell<Overlay>,
}

impl State {
    fn refresh(&self, window: HWND) {
        if let Ok(mut overlay) = self.overlay.try_borrow_mut() {
            overlay.refresh(window, &self.icon);
        }
    }

    fn invoke(&self, action: HostAction) {
        // The application owns the callback. Keep Rust panics inside the native procedure.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.action)(action)));
    }

    fn menu(&self, window: HWND, position: POINT, direct: bool, first: usize) {
        let Ok(overlay) = self.overlay.try_borrow() else {
            return;
        };
        let accounts: Vec<_> = overlay
            .tiles
            .iter()
            .skip(first)
            .map(|tile| {
                let provider = if tile.provider == Provider::Claude {
                    "Claude"
                } else {
                    "GPT"
                };
                let status = if !has_quota(tile) {
                    " · 정보 없음"
                } else if tile.stale {
                    " · 이전 데이터"
                } else {
                    ""
                };
                (tile.id, format!("{provider} · {}{status}", tile.alias))
            })
            .collect();
        drop(overlay);
        match (direct, accounts.as_slice()) {
            (true, [(id, _)]) => self.invoke(HostAction::Open(*id)),
            (true, []) => self.invoke(HostAction::Settings),
            _ => {
                if let Some(action) = show_menu(window, position, &accounts) {
                    self.invoke(action);
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const State;
    if state.is_null() {
        return unsafe { DefWindowProcW(window, message, wp, lp) };
    }
    // Hold an extra reference because a callback may drop Host while the message is active.
    let state = unsafe {
        Rc::increment_strong_count(state);
        Rc::from_raw(state)
    };
    match message {
        WM_TIMER if wp.0 == TIMER_ID => state.refresh(window),
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE | WM_THEMECHANGED => {
            // SetWindowPos/UpdateLayeredWindow can send WM_DPICHANGED during refresh.
            if let Ok(mut overlay) = state.overlay.try_borrow_mut() {
                overlay.sample = None;
                overlay.next_sample = None;
                overlay.dirty = true;
            }
            let _ = unsafe { PostMessageW(Some(window), WM_TIMER, WPARAM(TIMER_ID), LPARAM(0)) };
        }
        msg if msg == state.taskbar_created && msg != 0 => {
            if let Ok(mut overlay) = state.overlay.try_borrow_mut() {
                overlay.tray_shell = None;
                overlay.sample = None;
                overlay.next_sample = None;
            }
            state.refresh(window);
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_LBUTTONUP => {
            let point = message_point(lp.0);
            let selected = state
                .overlay
                .try_borrow()
                .ok()
                .and_then(|overlay| overlay.click_at(point));
            match selected {
                Some(StripClick::Open(id)) => state.invoke(HostAction::Open(id)),
                Some(StripClick::Overflow { first, anchor }) => {
                    state.menu(window, anchor, false, first)
                }
                None => {}
            }
        }
        WM_CONTEXTMENU => {
            let mut point = message_point(lp.0);
            if point.x == -1 && point.y == -1 {
                point = cursor_pos();
            }
            state.menu(window, point, false, 0);
        }
        TRAY_MESSAGE => {
            // NOTIFYICON_VERSION_4: callback event in LOWORD(lParam), position in wParam.
            let event = lp.0 as u32 & 0xffff;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_KEYSELECT {
                let mut point = message_point(wp.0 as isize);
                if event == WM_CONTEXTMENU || (point.x == -1 && point.y == -1) {
                    point = tray_position(window).unwrap_or_else(cursor_pos);
                }
                state.menu(window, point, event != WM_CONTEXTMENU, 0);
            } else if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                // Legacy notifications if the shell rejected NIM_SETVERSION.
                let legacy = state
                    .overlay
                    .try_borrow()
                    .is_ok_and(|overlay| !overlay.tray_v4);
                if legacy {
                    state.menu(window, cursor_pos(), event == WM_LBUTTONUP, 0);
                }
            }
        }
        _ => return unsafe { DefWindowProcW(window, message, wp, lp) },
    }
    LRESULT(0)
}

fn message_point(value: isize) -> POINT {
    POINT {
        x: value as i16 as i32,
        y: (value >> 16) as i16 as i32,
    }
}

fn cursor_pos() -> POINT {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    point
}

fn tray_position(window: HWND) -> Option<POINT> {
    let icon = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: window,
        uID: TRAY_ID,
        ..Default::default()
    };
    let bounds = unsafe { Shell_NotifyIconGetRect(&icon) }.ok()?;
    Some(POINT {
        x: bounds.left + (bounds.right - bounds.left) / 2,
        y: bounds.top,
    })
}

fn show_menu(window: HWND, point: POINT, accounts: &[(i64, String)]) -> Option<HostAction> {
    // Position can fall back to the cursor for older shell notifications.
    let _dpi = DpiContext::per_monitor();
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    let selected = (|| {
        const SETTINGS: usize = 1;
        const EXIT: usize = 2;
        const FIRST_ACCOUNT: usize = 3;
        for (index, (_, label)) in accounts.iter().enumerate() {
            let label = label.replace('&', "&&").replace('\0', "");
            let wide: Vec<u16> = label.encode_utf16().chain([0]).collect();
            unsafe {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    FIRST_ACCOUNT + index,
                    PCWSTR(wide.as_ptr()),
                )
            }
            .ok()?;
        }
        unsafe {
            if !accounts.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok()?;
            }
            AppendMenuW(menu, MF_STRING, SETTINGS, w!("설정")).ok()?;
            AppendMenuW(menu, MF_STRING, EXIT, w!("종료")).ok()?;
            let _ = SetForegroundWindow(window);
            let command = TrackPopupMenuEx(
                menu,
                (TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON).0,
                point.x,
                point.y,
                window,
                None,
            )
            .0 as usize;
            let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
            match command {
                SETTINGS => Some(HostAction::Settings),
                EXIT => Some(HostAction::Exit),
                command => accounts
                    .get(command.checked_sub(FIRST_ACCOUNT)?)
                    .map(|(id, _)| HostAction::Open(*id)),
            }
        }
    })();
    let _ = unsafe { DestroyMenu(menu) };
    selected
}

struct TrayIcon(HICON);

impl TrayIcon {
    fn new() -> WinResult<Self> {
        // Draw a 32-pixel quota ring with an alpha channel for both taskbar themes.
        let mask = [0u8; 128];
        let mut pixels = [0u8; 32 * 32 * 4];
        for y in 0..32 {
            for x in 0..32 {
                let dx = x as f32 + 0.5 - 16.;
                let dy = y as f32 + 0.5 - 16.;
                let radius = dx.hypot(dy);
                let mut angle = dx.atan2(dy);
                if angle < 0. {
                    angle += TAU;
                }
                let fill = angle <= TAU * 0.77;
                let (r, g, b) = if fill { (103, 215, 204) } else { (73, 92, 95) };
                let alpha = (2.5 - (radius - 11.).abs()).clamp(0., 1.);
                let position = (y * 32 + x) * 4;
                pixels[position..position + 4].copy_from_slice(&[
                    (b as f32 * alpha).round() as u8,
                    (g as f32 * alpha).round() as u8,
                    (r as f32 * alpha).round() as u8,
                    (255. * alpha).round() as u8,
                ]);
            }
        }
        unsafe { CreateIcon(None, 32, 32, 1, 32, mask.as_ptr(), pixels.as_ptr()).map(Self) }
    }

    fn data(&self, window: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: window,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: self.0,
            ..Default::default()
        };
        for (to, from) in data.szTip.iter_mut().zip("Quota".encode_utf16()) {
            *to = from;
        }
        data
    }

    fn add(&self, window: HWND) -> Option<bool> {
        let mut data = self.data(window);
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return None;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        Some(unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool())
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct DpiContext(DPI_AWARENESS_CONTEXT);

impl DpiContext {
    fn per_monitor() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for DpiContext {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn dip(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

#[derive(Default)]
struct Layout {
    starts: Vec<i32>,
    dividers: Vec<i32>,
    width: i32,
}

impl Layout {
    fn new(tiles: &[Tile]) -> Self {
        let mut layout = Self::default();
        for (index, tile) in tiles.iter().enumerate() {
            if index != 0 {
                if tiles[index - 1].provider != tile.provider {
                    layout.dividers.push(layout.width + GROUP_GAP / 2);
                    layout.width += GROUP_GAP;
                } else {
                    layout.width += 1;
                }
            }
            layout.starts.push(layout.width);
            layout.width += TILE_WIDTH;
        }
        layout
    }

    fn fit(&self, capacity: i32, dpi: u32) -> Option<Fit> {
        if self.starts.is_empty() || capacity < dip(TILE_WIDTH, dpi) {
            return None;
        }
        if dip(self.width, dpi) <= capacity {
            return Some(Fit {
                visible: self.starts.len(),
                overflow_at: None,
                width: self.width,
            });
        }
        let visible = self.starts.partition_point(|&start| {
            dip(start + TILE_WIDTH + OVERFLOW_GAP + TILE_WIDTH, dpi) <= capacity
        });
        let overflow_at = visible
            .checked_sub(1)
            .map_or(0, |last| self.starts[last] + TILE_WIDTH + OVERFLOW_GAP);
        Some(Fit {
            visible,
            overflow_at: Some(overflow_at),
            width: overflow_at + TILE_WIDTH,
        })
    }

    fn slot_at(&self, x: i32, dpi: u32, fit: Fit) -> Option<Slot> {
        let contains = |start| (dip(start, dpi)..dip(start + TILE_WIDTH, dpi)).contains(&x);
        if fit.overflow_at.is_some_and(contains) {
            return Some(Slot::Overflow);
        }
        self.starts
            .get(..fit.visible)?
            .iter()
            .position(|&start| contains(start))
            .map(Slot::Account)
    }

    fn click_at(&self, point: POINT, placed: Placement, tiles: &[Tile]) -> Option<StripClick> {
        let rect = placed.rect;
        if !(0..rect.bottom - rect.top).contains(&point.y) {
            return None;
        }
        match self.slot_at(point.x, placed.dpi, placed.fit)? {
            Slot::Account(index) => tiles.get(index).map(|tile| StripClick::Open(tile.id)),
            Slot::Overflow => Some(StripClick::Overflow {
                first: placed.fit.visible,
                anchor: POINT {
                    x: rect.left + point.x,
                    y: rect.top + point.y,
                },
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    visible: usize,
    overflow_at: Option<i32>,
    width: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Account(usize),
    Overflow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    rect: RECT,
    dpi: u32,
    fit: Fit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StripClick {
    Open(i64),
    Overflow { first: usize, anchor: POINT },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    shell: usize,
    bar: RECT,
    tray: RECT,
    monitor: RECT,
    dpi: u32,
}

fn window_rect(window: HWND) -> Option<RECT> {
    let mut bounds = RECT::default();
    unsafe { GetWindowRect(window, &mut bounds).ok()? };
    (bounds.right > bounds.left && bounds.bottom > bounds.top).then_some(bounds)
}

fn intersects(a: RECT, b: RECT) -> bool {
    a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
}

fn contains(bounds: RECT, rect: RECT) -> bool {
    rect.left >= bounds.left
        && rect.right <= bounds.right
        && rect.top >= bounds.top
        && rect.bottom <= bounds.bottom
}

fn win_class(window: HWND) -> String {
    let mut name = [0; 96];
    let length = unsafe { GetClassNameW(window, &mut name) }.max(0) as usize;
    String::from_utf16_lossy(&name[..length])
}

fn geometry(shell: HWND) -> Option<Geometry> {
    unsafe {
        if !IsWindowVisible(shell).as_bool() {
            return None;
        }
        let mut bar_info = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut bar_info) == 0
            || ![ABE_TOP, ABE_BOTTOM].contains(&bar_info.uEdge)
        {
            return None;
        }
        let monitor_handle = MonitorFromWindow(shell, MONITOR_DEFAULTTONEAREST);
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor_handle, &mut monitor_info).as_bool()
            || monitor_info.dwFlags & MONITORINFOF_PRIMARY == 0
        {
            return None;
        }
        // ABM_GETTASKBARPOS supplies the edge and nominal rectangle. The actual HWND
        // moves while auto-hide is active. Require the entire strip to be on-screen.
        let monitor = monitor_info.rcMonitor;
        let bar = window_rect(shell)?;
        if !contains(monitor, bar)
            || !intersects(bar_info.rc, bar)
            || bar.right - bar.left <= bar.bottom - bar.top
        {
            return None;
        }
        if is_fullscreen(shell, monitor, bar) {
            return None;
        }

        let tray_window = FindWindowExW(Some(shell), None, w!("TrayNotifyWnd"), None).ok()?;
        let mut tray = window_rect(tray_window)?;
        if !IsWindowVisible(tray_window).as_bool()
            || !intersects(tray, bar)
            || tray.left <= bar.left
            || tray.left >= bar.right
        {
            return None;
        }
        for class in [
            w!("ClockButton"),
            w!("TrayClockWClass"),
            w!("TrayShowDesktopButtonWClass"),
        ] {
            let mut previous = None;
            while let Ok(child) = FindWindowExW(Some(shell), previous, class, None) {
                previous = Some(child);
                if let Some(rect) = window_rect(child)
                    && IsWindowVisible(child).as_bool()
                    && intersects(rect, bar)
                {
                    tray.left = tray.left.min(rect.left);
                    tray.right = tray.right.max(rect.right);
                }
            }
        }
        let dpi = GetDpiForWindow(shell);
        (dpi != 0).then_some(Geometry {
            shell: shell.0 as usize,
            bar,
            tray,
            monitor,
            dpi,
        })
    }
}

fn is_fullscreen(shell: HWND, monitor: RECT, bar: RECT) -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid()
            || foreground == shell
            || foreground == GetShellWindow()
            || !IsWindowVisible(foreground).as_bool()
            || MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST)
                != MonitorFromWindow(shell, MONITOR_DEFAULTTONEAREST)
            || matches!(
                win_class(foreground).as_str(),
                "Progman" | "WorkerW" | "Shell_TrayWnd"
            )
        {
            return false;
        }
        if SHQueryUserNotificationState().is_ok_and(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN) {
            return true;
        }
        if !window_rect(foreground)
            .is_some_and(|rect| contains(rect, monitor) && contains(rect, bar))
        {
            return false;
        }
        let style = WINDOW_STYLE(GetWindowLongPtrW(foreground, GWL_STYLE) as u32);
        // Maximized captioned windows include invisible frame margins.
        !(IsZoomed(foreground).as_bool() && style.contains(WS_CAPTION))
    }
}

fn placement(geometry: Geometry, obstacles: &[RECT], layout: &Layout) -> Option<Placement> {
    let bar = geometry.bar;
    let dpi = geometry.dpi;
    let height = dip(HEIGHT, dpi);
    let left_bound = bar.left.max(geometry.monitor.left);
    let right_bound = geometry
        .tray
        .left
        .min(bar.right)
        .min(geometry.monitor.right);
    let top = bar.top + (bar.bottom - bar.top - height) / 2;
    let band = RECT {
        left: left_bound,
        top,
        right: right_bound,
        bottom: top + height,
    };
    if left_bound >= right_bound || !contains(bar, band) || !contains(geometry.monitor, band) {
        return None;
    }

    let mut occupied: Vec<_> = obstacles
        .iter()
        .filter(|&&rect| intersects(rect, band))
        .map(|rect| (rect.left.max(left_bound), rect.right.min(right_bound)))
        .collect();
    occupied.sort_unstable_by_key(|&(_, right)| std::cmp::Reverse(right));

    // Walk from the tray towards the start of the bar, merging overlaps as we go.
    // A gap closer to the tray wins when it can show the same number of accounts.
    let mut best: Option<Placement> = None;
    let mut end = right_bound;
    for (left, right) in occupied.into_iter().chain([(left_bound, left_bound)]) {
        let anchor = end - dip(TRAY_GAP, dpi);
        let capacity = anchor - right - dip(2, dpi);
        if let Some(fit) = layout.fit(capacity, dpi)
            && best.is_none_or(|best| fit.visible > best.fit.visible)
        {
            let rect = RECT {
                left: anchor - dip(fit.width, dpi),
                right: anchor,
                ..band
            };
            let placed = Placement { rect, dpi, fit };
            if fit.overflow_at.is_none() {
                return Some(placed);
            }
            best = Some(placed);
        }
        end = end.min(left);
    }
    best
}

struct Sample {
    target: Geometry,
    measured: Instant,
    obstacles: Vec<RECT>,
}

struct Scanner {
    request: SyncSender<Geometry>,
    response: Receiver<(Geometry, Option<Sample>)>,
}

impl Scanner {
    fn new() -> WinResult<Self> {
        let (request, pending) = mpsc::sync_channel::<Geometry>(1);
        let (ready, response) = mpsc::channel();
        std::thread::Builder::new()
            .name("quota-taskbar-uia".into())
            .spawn(move || {
                let _dpi = DpiContext::per_monitor();
                if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
                    return;
                }
                {
                    let mut uia = None;
                    while let Ok(target) = pending.recv() {
                        if uia.is_none() {
                            uia = Automation::new().ok();
                        }
                        let measured = Instant::now();
                        let obstacles =
                            uia.as_ref().and_then(|uia| uia.scan(target).ok()).flatten();
                        let sample = obstacles.map(|obstacles| Sample {
                            target,
                            measured,
                            obstacles,
                        });
                        if ready.send((target, sample)).is_err() {
                            break;
                        }
                    }
                }
                unsafe { CoUninitialize() };
            })
            .map_err(|error| Error::new(E_FAIL, error.to_string()))?;
        Ok(Self { request, response })
    }
}

struct Automation {
    client: IUIAutomation2,
    filter: IUIAutomationCondition,
    cache: IUIAutomationCacheRequest,
}

impl Automation {
    fn new() -> WinResult<Self> {
        unsafe {
            let client: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
            client.SetConnectionTimeout(250)?;
            client.SetTransactionTimeout(350)?;
            let cache = client.CreateCacheRequest()?;
            for property in [
                UIA_ControlTypePropertyId,
                UIA_BoundingRectanglePropertyId,
                UIA_IsOffscreenPropertyId,
            ] {
                cache.AddProperty(property)?;
            }
            cache.SetAutomationElementMode(AutomationElementMode_None)?;
            let filter = client.CreateTrueCondition()?;
            Ok(Self {
                client,
                filter,
                cache,
            })
        }
    }

    fn scan(&self, target: Geometry) -> WinResult<Option<Vec<RECT>>> {
        let shell = HWND(target.shell as *mut _);
        unsafe {
            let root = self.client.ElementFromHandle(shell)?;
            let entries =
                root.FindAllBuildCache(TreeScope_Descendants, &self.filter, &self.cache)?;
            let count = entries.Length()?;
            if count > 2048 {
                return Ok(None);
            }
            let mut obstacles = Vec::new();
            let mut saw_left_button = false;
            for index in 0..count {
                let element = entries.GetElement(index)?;
                if element.CachedIsOffscreen()?.as_bool()
                    || ![
                        UIA_ButtonControlTypeId,
                        UIA_SplitButtonControlTypeId,
                        UIA_ListItemControlTypeId,
                        UIA_CheckBoxControlTypeId,
                        UIA_RadioButtonControlTypeId,
                        UIA_EditControlTypeId,
                        UIA_ComboBoxControlTypeId,
                        UIA_MenuItemControlTypeId,
                        UIA_TabItemControlTypeId,
                    ]
                    .contains(&element.CachedControlType()?)
                {
                    continue;
                }
                let rect = element.CachedBoundingRectangle()?;
                if !is_bar_control(rect, target.bar) {
                    continue;
                }
                saw_left_button |= rect.right <= target.tray.left;
                obstacles.push(rect);
            }
            // An empty/incomplete new Explorer tree is insufficient evidence of a gap.
            Ok(saw_left_button.then_some(obstacles))
        }
    }
}

fn is_bar_control(rect: RECT, bar: RECT) -> bool {
    if rect.right <= rect.left || rect.bottom <= rect.top || !intersects(rect, bar) {
        return false;
    }
    let center_y = rect.top + (rect.bottom - rect.top) / 2;
    (bar.top..bar.bottom).contains(&center_y)
        && rect.bottom - rect.top <= 2 * (bar.bottom - bar.top)
}

fn add_native_obstacles(window: HWND, target: Geometry, obstacles: &mut Vec<RECT>) {
    let shell = HWND(target.shell as *mut _);
    unsafe {
        let mut previous = None;
        while let Ok(child) = FindWindowExW(Some(shell), previous, PCWSTR::null(), PCWSTR::null()) {
            previous = Some(child);
            if IsWindowVisible(child).as_bool()
                && let Some(bounds) = window_rect(child)
                && is_bar_control(bounds, target.bar)
                && !is_shell_container(&win_class(child))
            {
                obstacles.push(bounds);
            }
        }

        // A third-party overlay can sit above the taskbar as a top-level window.
        // Restrict this scan to visible topmost windows situated in the taskbar band.
        let mut scan = NativeScan {
            window,
            shell,
            bar: target.bar,
            obstacles,
        };
        let _ = EnumWindows(Some(check_topmost), LPARAM((&raw mut scan) as isize));
    }
}

fn is_shell_container(class: &str) -> bool {
    matches!(
        class,
        "Start"
            | "TrayDummySearchControl"
            | "ReBarWindow32"
            | "WorkerW"
            | "TrayNotifyWnd"
            | "DynamicContent2"
            | "Windows.UI.Core.CoreWindow"
            | "Windows.UI.Composition.DesktopWindowContentBridge"
            | "Windows.UI.Input.InputSite.WindowClass"
            | "#32770"
    )
}

struct NativeScan<'a> {
    window: HWND,
    shell: HWND,
    bar: RECT,
    obstacles: &'a mut Vec<RECT>,
}

unsafe extern "system" fn check_topmost(window: HWND, param: LPARAM) -> windows::core::BOOL {
    // EnumWindows calls synchronously with a pointer to its caller's scan.
    let scan = unsafe { &mut *(param.0 as *mut NativeScan<'_>) };
    unsafe {
        if window != scan.window
            && window != scan.shell
            && IsWindowVisible(window).as_bool()
            && GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0
            && let Some(bounds) = window_rect(window)
            && is_bar_control(bounds, scan.bar)
        {
            scan.obstacles.push(bounds);
        }
    }
    true.into()
}
struct Overlay {
    scanner: Scanner,
    renderer: Renderer,
    tiles: Vec<Tile>,
    layout: Layout,
    sample: Option<Sample>,
    requested: Option<Geometry>,
    next_sample: Option<Instant>,
    placed: Option<Placement>,
    hover: Option<Slot>,
    light: bool,
    dirty: bool,
    tray_shell: Option<usize>,
    tray_v4: bool,
}

impl Overlay {
    fn new() -> WinResult<Self> {
        Ok(Self {
            scanner: Scanner::new()?,
            renderer: Renderer::new()?,
            tiles: Vec::new(),
            layout: Layout::default(),
            sample: None,
            requested: None,
            next_sample: None,
            placed: None,
            hover: None,
            light: false,
            dirty: true,
            tray_shell: None,
            tray_v4: false,
        })
    }

    fn refresh(&mut self, window: HWND, icon: &TrayIcon) {
        let _dpi = DpiContext::per_monitor();
        let shell = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.ok();
        let shell_id = shell.map(|shell| shell.0 as usize);
        if self.tray_shell != shell_id {
            self.tray_shell = None;
            if shell.is_some()
                && let Some(v4) = icon.add(window)
            {
                self.tray_v4 = v4;
                self.tray_shell = shell_id;
            }
        }
        let target = if self.tiles.is_empty() {
            None
        } else {
            shell.and_then(geometry)
        };
        let Some(target) = target else {
            self.sample = None;
            self.next_sample = None;
            self.requested = None;
            self.hide(window);
            return;
        };
        let now = Instant::now();
        while let Ok((measured, result)) = self.scanner.response.try_recv() {
            if measured == target {
                self.sample = result;
            }
        }
        if (self.requested != Some(target) || self.next_sample.is_none_or(|at| now >= at))
            && self.scanner.request.try_send(target).is_ok()
        {
            self.requested = Some(target);
            self.next_sample = Some(now + SAMPLE_INTERVAL);
        }
        let placed = self
            .sample
            .as_ref()
            .filter(|sample| {
                sample.target == target
                    && now.saturating_duration_since(sample.measured) <= MAX_SAMPLE_AGE
            })
            .and_then(|sample| {
                let mut obstacles = sample.obstacles.clone();
                add_native_obstacles(window, target, &mut obstacles);
                placement(target, &obstacles, &self.layout)
            });
        let Some(placed) = placed else {
            self.hide(window);
            return;
        };
        let rect = placed.rect;

        let cursor = cursor_pos();
        let hover = (cursor.x >= rect.left
            && cursor.x < rect.right
            && cursor.y >= rect.top
            && cursor.y < rect.bottom)
            .then(|| {
                self.layout
                    .slot_at(cursor.x - rect.left, placed.dpi, placed.fit)
            })
            .flatten();
        let light = light_theme();
        let old = self.placed;
        let moved = old != Some(placed);
        let redraw = self.dirty
            || self.hover != hover
            || self.light != light
            || old.is_none_or(|old| {
                old.dpi != placed.dpi
                    || old.fit != placed.fit
                    || old.rect.bottom - old.rect.top != rect.bottom - rect.top
            });
        let positioned = if redraw {
            self.renderer
                .present(window, placed, &self.layout, &self.tiles, hover, light)
        } else if moved {
            unsafe {
                SetWindowPos(
                    window,
                    None,
                    rect.left,
                    rect.top,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
                )
            }
        } else {
            Ok(())
        };
        if positioned.is_err() {
            self.hide(window);
            self.dirty = true;
            return;
        }
        if old.is_none()
            && unsafe {
                SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE | SWP_SHOWWINDOW,
                )
            }
            .is_err()
        {
            self.dirty = true;
            return;
        }
        self.placed = Some(placed);
        self.dirty = false;
        self.hover = hover;
        self.light = light;
    }

    fn hide(&mut self, window: HWND) {
        if self.placed.take().is_some() {
            let _ = unsafe { ShowWindow(window, SW_HIDE) };
        }
    }

    fn click_at(&self, point: POINT) -> Option<StripClick> {
        let placed = self.placed?;
        self.layout.click_at(point, placed, &self.tiles)
    }
}

fn light_theme() -> bool {
    let mut light = 0u32;
    let mut kind = 0u32;
    let mut bytes = size_of::<u32>() as u32;
    let status = unsafe {
        SHRegGetUSValueW(
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            Some(&mut kind),
            Some((&mut light as *mut u32).cast()),
            Some(&mut bytes),
            false,
            None,
            0,
        )
    };
    status == ERROR_SUCCESS && kind == 4 && bytes == 4 && light != 0
}

// 64x64 alpha masks rasterized from claude.svg and openai.svg, covered by ICONS-LICENSE.txt.
const CLAUDE_MASK: &[u8; 4096] = include_bytes!("taskbar_claude.alpha");
const GPT_MASK: &[u8; 4096] = include_bytes!("taskbar_openai.alpha");
const RING_WIDTH: f32 = 4.35 / 2.;

fn has_quota(tile: &Tile) -> bool {
    [tile.week, tile.short, tile.fable]
        .into_iter()
        .flatten()
        .any(f64::is_finite)
}

#[derive(Clone, Copy, Debug)]
struct Arc {
    start: f32,
    span: f32,
    filled: f32,
}

fn arcs(radius: f32, divisions: u8, remaining: f64) -> impl Iterator<Item = Arc> {
    let step = TAU / f32::from(divisions);
    let gap = if divisions == 1 { 0. } else { 1.3 / radius };
    let percent = if remaining.is_finite() {
        remaining.clamp(0., 100.) as f32
    } else {
        0.
    };
    (0..divisions).map(move |index| Arc {
        start: step * f32::from(index) + gap / 2.,
        span: step - gap,
        filled: (step - gap)
            * (percent * f32::from(divisions) / 100. - f32::from(index)).clamp(0., 1.),
    })
}

struct Renderer {
    factory: ID2D1Factory,
    target: Option<ID2D1DCRenderTarget>,
    labels: IDWriteTextFormat,
    overflow_label: IDWriteTextFormat,
}

impl Renderer {
    fn new() -> WinResult<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let labels = write.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                9.,
                w!("ko-KR"),
            )?;
            labels.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            labels.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            labels.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let ellipsis = write.CreateEllipsisTrimmingSign(&labels)?;
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                ..Default::default()
            };
            labels.SetTrimming(&trimming, &ellipsis)?;
            let target = Some(Self::create_target(&factory)?);
            let overflow_label = write.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                14.,
                w!("ko-KR"),
            )?;
            overflow_label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            overflow_label.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            overflow_label.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(Self {
                factory,
                target,
                labels,
                overflow_label,
            })
        }
    }

    fn create_target(factory: &ID2D1Factory) -> WinResult<ID2D1DCRenderTarget> {
        unsafe {
            let target = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: pixel_format(),
                dpiX: 96.,
                dpiY: 96.,
                ..Default::default()
            })?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(target)
        }
    }

    fn present(
        &mut self,
        window: HWND,
        position: Placement,
        layout: &Layout,
        tiles: &[Tile],
        hover: Option<Slot>,
        light: bool,
    ) -> WinResult<()> {
        let Placement { rect, dpi, fit } = position;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let buffer = Buffer::new(width, height)?;
        if self.target.is_none() {
            self.target = Some(Self::create_target(&self.factory)?);
        }
        let target = self
            .target
            .as_ref()
            .expect("Direct2D target was just initialized");
        let local = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let palette = Palette::new(light);
        unsafe {
            target.BindDC(buffer.dc, &local)?;
            target.SetDpi(dpi as f32, dpi as f32);
            let brush = target.CreateSolidColorBrush(&palette.track, None)?;
            let claude = logo(target, CLAUDE_MASK, palette.claude)?;
            let gpt = logo(target, GPT_MASK, palette.gpt)?;
            target.BeginDraw();
            target.Clear(Some(&D2D1_COLOR_F::default()));
            for &divider in &layout.dividers {
                // The divider before the first hidden account does not belong to the visible strip.
                if fit.visible == 0 || divider >= layout.starts[fit.visible - 1] {
                    break;
                }
                brush.SetColor(&palette.line);
                target.FillRectangle(
                    &draw_rect(divider as f32, 6., divider as f32 + 1., 39.),
                    &brush,
                );
            }
            let draw_slot = |x: f32, slot| {
                brush.SetColor(if hover == Some(slot) {
                    &palette.hover
                } else {
                    &palette.paper
                });
                target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: draw_rect(x, 0., x + TILE_WIDTH as f32, HEIGHT as f32),
                        radiusX: 6.,
                        radiusY: 6.,
                    },
                    &brush,
                );
            };
            let draw: WinResult<()> = (|| {
                for (index, tile) in tiles.iter().take(fit.visible).enumerate() {
                    let x = layout.starts[index] as f32;
                    draw_slot(x, Slot::Account(index));

                    let center = (x + TILE_WIDTH as f32 / 2., 17.);
                    let known = has_quota(tile);
                    for (limit, radius, divisions, active) in [
                        (tile.week, 14.75, 7, palette.week),
                        (tile.short, 11.5, 5, palette.short),
                        (tile.fable, 8.25, 1, palette.fable),
                    ] {
                        let Some(remaining) = limit.filter(|value| value.is_finite()) else {
                            continue;
                        };
                        for arc in arcs(radius, divisions, remaining) {
                            brush.SetColor(&palette.track);
                            self.arc(target, &brush, center, radius, arc.start, arc.span)?;
                            if arc.filled > 0. {
                                brush.SetColor(&active);
                                self.arc(target, &brush, center, radius, arc.start, arc.filled)?;
                            }
                        }
                    }
                    if !known {
                        brush.SetColor(&palette.track);
                        self.arc(target, &brush, center, 14.75, 0., TAU)?;
                    }
                    let logo_radius = 32. * 0.36 / 2.;
                    let logo_bounds = draw_rect(
                        center.0 - logo_radius,
                        center.1 - logo_radius,
                        center.0 + logo_radius,
                        center.1 + logo_radius,
                    );
                    let mark = if tile.provider == Provider::Claude {
                        &claude
                    } else {
                        &gpt
                    };
                    target.DrawBitmap(
                        mark,
                        Some(&logo_bounds),
                        if known { 1. } else { 0.48 },
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                    brush.SetColor(&palette.label);
                    let alias: Vec<_> = tile.alias.encode_utf16().collect();
                    target.DrawText(
                        &alias,
                        &self.labels,
                        &draw_rect(x + 1., 33., x + 35., 44.),
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                    if tile.stale && known {
                        let mut marker = D2D1_ELLIPSE {
                            radiusX: 1.6,
                            radiusY: 1.6,
                            ..Default::default()
                        };
                        marker.point.X = x + 32.;
                        marker.point.Y = 4.;
                        brush.SetColor(&palette.fable);
                        target.FillEllipse(&marker, &brush);
                    }
                }
                if let Some(x) = fit.overflow_at {
                    let x = x as f32;
                    draw_slot(x, Slot::Overflow);
                    let label: Vec<_> = format!("+{}", tiles.len() - fit.visible)
                        .encode_utf16()
                        .collect();
                    brush.SetColor(&palette.label);
                    target.DrawText(
                        &label,
                        &self.overflow_label,
                        &draw_rect(x, 0., x + TILE_WIDTH as f32, HEIGHT as f32),
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                Ok(())
            })();
            let result = target.EndDraw(None, None);
            if result.is_err() {
                self.target = None;
            }
            draw?;
            result?;
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                BlendFlags: 0,
            };
            UpdateLayeredWindow(
                window,
                None,
                Some(&POINT {
                    x: rect.left,
                    y: rect.top,
                }),
                Some(&SIZE {
                    cx: width,
                    cy: height,
                }),
                Some(buffer.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }

    fn arc(
        &self,
        target: &ID2D1DCRenderTarget,
        brush: &ID2D1SolidColorBrush,
        center: (f32, f32),
        radius: f32,
        start: f32,
        span: f32,
    ) -> WinResult<()> {
        let mut ellipse = D2D1_ELLIPSE {
            radiusX: radius,
            radiusY: radius,
            ..Default::default()
        };
        ellipse.point.X = center.0;
        ellipse.point.Y = center.1;
        unsafe {
            if span >= TAU - f32::EPSILON {
                target.DrawEllipse(&ellipse, brush, RING_WIDTH, None);
                return Ok(());
            }
            let point = |angle: f32| {
                let mut point = ellipse.point;
                point.X += radius * angle.sin();
                point.Y -= radius * angle.cos();
                point
            };
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(point(start), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: point(start + span),
                size: D2D_SIZE_F {
                    width: radius,
                    height: radius,
                },
                rotationAngle: 0.,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if span > PI {
                    D2D1_ARC_SIZE_LARGE
                } else {
                    D2D1_ARC_SIZE_SMALL
                },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            target.DrawGeometry(&path, brush, RING_WIDTH, None);
        }
        Ok(())
    }
}

fn logo(target: &ID2D1DCRenderTarget, mask: &[u8], tint: u32) -> WinResult<ID2D1Bitmap> {
    let mut pixels = Vec::with_capacity(mask.len() * 4);
    let channel = |shift| (tint >> shift) & 0xffu32;
    for &alpha in mask {
        let multiply = |shift| ((channel(shift) * u32::from(alpha) + 127u32) / 255u32) as u8;
        pixels.extend_from_slice(&[multiply(0), multiply(8), multiply(16), alpha]);
    }
    unsafe {
        target.CreateBitmap(
            D2D_SIZE_U {
                width: 64,
                height: 64,
            },
            Some(pixels.as_ptr().cast()),
            64 * 4,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: pixel_format(),
                dpiX: 96.,
                dpiY: 96.,
            },
        )
    }
}

fn pixel_format() -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT {
        format: DXGI_FORMAT_B8G8R8A8_UNORM,
        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
    }
}

fn draw_rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    let channel = |shift| ((rgb >> shift) & 0xffu32) as f32 / 255.;
    D2D1_COLOR_F {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: alpha,
    }
}

struct Palette {
    paper: D2D1_COLOR_F,
    hover: D2D1_COLOR_F,
    track: D2D1_COLOR_F,
    label: D2D1_COLOR_F,
    line: D2D1_COLOR_F,
    week: D2D1_COLOR_F,
    short: D2D1_COLOR_F,
    fable: D2D1_COLOR_F,
    claude: u32,
    gpt: u32,
}

impl Palette {
    fn new(light: bool) -> Self {
        let (paper, hover, track, label, line, week, short, fable, claude, gpt) = if light {
            (
                0xf1f3ef, 0xe0e8e4, 0xcdd8d0, 0x52665c, 0xc1ccc6, 0x6750ac, 0x00766e, 0x9a5717,
                0xab5332, 0x247666,
            )
        } else {
            (
                0x1a2028, 0x333d47, 0x48535d, 0xb0bbc4, 0x434c55, 0xbca8ff, 0x67d7cc, 0xffc078,
                0xef9c79, 0x96d7ca,
            )
        };
        Self {
            paper: color(paper, 0.92),
            hover: color(hover, 1.),
            track: color(track, 1.),
            label: color(label, 1.),
            line: color(line, 1.),
            week: color(week, 1.),
            short: color(short, 1.),
            fable: color(fable, 1.),
            claude,
            gpt,
        }
    }
}

struct Buffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Buffer {
    fn new(width: i32, height: i32) -> WinResult<Self> {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let bitmap = unsafe {
            CreateDIBSection(
                None,
                &BITMAPINFO {
                    bmiHeader: header,
                    ..Default::default()
                },
                DIB_RGB_COLORS,
                &mut std::ptr::null_mut(),
                None,
                0,
            )?
        };
        let dc = unsafe { CreateCompatibleDC(None) };
        let previous = if dc.is_invalid() {
            HGDIOBJ::default()
        } else {
            unsafe { SelectObject(dc, HGDIOBJ(bitmap.0)) }
        };
        if previous.is_invalid() {
            let error = Error::from_win32();
            unsafe {
                if !dc.is_invalid() {
                    let _ = DeleteDC(dc);
                }
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            return Err(error);
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
        })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    fn bar(dpi: u32) -> Geometry {
        // Negative origins are possible in the virtual desktop coordinate space.
        Geometry {
            shell: 1,
            bar: rect(-1600, -50, 0, 0),
            tray: rect(-200, -50, 0, 0),
            monitor: rect(-1600, -900, 0, 0),
            dpi,
        }
    }

    fn tiles(providers: &[Provider]) -> Vec<Tile> {
        providers
            .iter()
            .enumerate()
            .map(|(index, &provider)| Tile {
                stale: false,
                id: 100 + index as i64,
                provider,
                alias: "sample".into(),
                week: None,
                short: None,
                fable: None,
            })
            .collect()
    }

    #[test]
    fn placement_uses_physical_tray_edge_and_scales_gap() {
        let layout = Layout::new(&tiles(&[Provider::Claude; 4]));
        for dpi in [96, 120, 144] {
            let mut geometry = bar(dpi);
            geometry.bar.top = -dip(50, dpi);
            let placed = placement(geometry, &[rect(-700, -40, -450, -5)], &layout).unwrap();
            assert_eq!(placed.fit.visible, 4);
            assert_eq!(placed.rect.right, -200 - dip(4, dpi));
            assert_eq!(placed.rect.right - placed.rect.left, dip(layout.width, dpi));
            assert_eq!(placed.rect.bottom - placed.rect.top, dip(45, dpi));
            assert!(
                (geometry.bar.top - placed.rect.top - (placed.rect.bottom - geometry.bar.bottom))
                    .abs()
                    <= 1
            );
        }
    }

    #[test]
    fn crowded_or_hidden_taskbars_fall_back_to_tray() {
        let mut geometry = bar(96);
        let layout = Layout::new(&tiles(&[Provider::Claude; 4]));
        let crowded = [rect(-1570, -40, -920, -5), rect(-895, -40, -230, -5)];
        assert!(placement(geometry, &crowded, &layout).is_none());
        assert!(placement(geometry, &[], &Layout::default()).is_none());
        geometry.bar.top = -30;
        assert!(placement(geometry, &[], &layout).is_none());
        geometry.bar = rect(-1600, -30, 0, 20);
        assert!(placement(geometry, &[], &layout).is_none());
    }

    #[test]
    fn nearest_fitting_gap_precedes_a_wider_gap() {
        let layout = Layout::new(&tiles(&[Provider::Claude; 4]));
        let obstacles = [
            rect(-1600, -50, -1500, 0),
            rect(-900, -50, -820, 0),
            rect(-650, -50, -600, 0),
            rect(-615, -50, -220, 0),
        ];
        let placed = placement(bar(96), &obstacles, &layout).unwrap();
        assert_eq!(placed.fit.visible, 4);
        assert_eq!(placed.rect.right, -654);
        assert!(
            obstacles
                .iter()
                .all(|&obstacle| !intersects(obstacle, placed.rect))
        );
    }

    #[test]
    fn shorter_gaps_keep_a_prefix_and_route_overflow_click() {
        use Provider::{Claude, Codex};
        let tiles = tiles(&[Claude, Codex, Claude, Codex]);
        let layout = Layout::new(&tiles);
        let obstacles = [
            rect(-1600, -50, -800, 0),
            rect(-660, -50, -540, 0),
            rect(-440, -50, -300, 0),
        ];
        let placed = placement(bar(96), &obstacles, &layout).unwrap();
        assert_eq!(placed.rect.right, -664);
        assert_eq!(placed.fit.visible, 2);
        assert_eq!(placed.fit.overflow_at, Some(88));
        assert_eq!(
            tiles[placed.fit.visible..]
                .iter()
                .map(|tile| tile.id)
                .collect::<Vec<_>>(),
            [102, 103]
        );
        let point = |x| POINT { x, y: 20 };
        assert_eq!(
            layout.click_at(point(5), placed, &tiles),
            Some(StripClick::Open(100))
        );
        assert_eq!(
            layout.click_at(point(55), placed, &tiles),
            Some(StripClick::Open(101))
        );
        assert_eq!(layout.click_at(point(85), placed, &tiles), None);
        assert_eq!(
            layout.click_at(point(100), placed, &tiles),
            Some(StripClick::Overflow {
                first: 2,
                anchor: POINT {
                    x: placed.rect.left + 100,
                    y: placed.rect.top + 20
                },
            })
        );
    }

    #[test]
    fn one_available_slot_shows_all_accounts_in_overflow() {
        let obstacles = [rect(-1600, -50, -250, 0)];
        let tiles = tiles(&[Provider::Claude; 4]);
        let layout = Layout::new(&tiles);
        let placed = placement(bar(96), &obstacles, &layout).unwrap();
        assert_eq!(
            placed.fit,
            Fit {
                visible: 0,
                overflow_at: Some(0),
                width: 36
            }
        );
        assert!(matches!(
            layout.click_at(POINT { x: 18, y: 20 }, placed, &tiles),
            Some(StripClick::Overflow { first: 0, .. })
        ));
        assert_eq!(
            placement(bar(96), &obstacles, &Layout::new(&tiles[..1]))
                .unwrap()
                .fit
                .overflow_at,
            None
        );
    }

    #[test]
    fn unrelated_flyout_bounds_are_not_taskbar_controls() {
        let bar = bar(96).bar;
        assert!(is_bar_control(rect(-300, -42, -260, -2), bar));
        assert!(!is_bar_control(rect(-300, -550, -22, -8), bar));
        assert!(!is_bar_control(rect(-300, -42, -300, -2), bar));
    }

    #[test]
    fn segmented_ring_fills_exactly_one_fractional_segment() {
        let segments: Vec<_> = arcs(14.75, 7, 48.).collect();
        assert_eq!(segments.len(), 7);
        assert!(segments[0].start > 0.);
        assert!(segments[0].span < TAU / 7.);
        assert!(segments[..3].iter().all(|arc| arc.filled == arc.span));
        assert!((segments[3].filled / segments[3].span - 0.36).abs() < 0.00001);
        assert!(segments[4..].iter().all(|arc| arc.filled == 0.));
        for (input, expected) in [(-5., 0.), (0., 0.), (100., 1.), (104., 1.), (f64::NAN, 0.)] {
            assert!(
                arcs(11.5, 5, input).all(|arc| (arc.filled / arc.span - expected).abs() < 0.00001)
            );
        }
    }

    #[test]
    fn continuous_fable_ring_starts_at_noon() {
        for remaining in [0., 31.5, 100.] {
            let arc = arcs(8.25, 1, remaining).next().unwrap();
            assert_eq!(arc.start, 0.);
            assert_eq!(arc.span, TAU);
            assert!((arc.filled / TAU - remaining as f32 / 100.).abs() < 0.00001);
        }
    }

    #[test]
    fn the_scaled_group_gap_never_opens_an_account() {
        let tiles = tiles(&[
            Provider::Claude,
            Provider::Claude,
            Provider::Codex,
            Provider::Codex,
        ]);
        let layout = Layout::new(&tiles);
        for dpi in [96, 120, 144] {
            let fit = layout.fit(dip(layout.width, dpi), dpi).unwrap();
            assert_eq!(
                layout.slot_at(dip(37, dpi), dpi, fit),
                Some(Slot::Account(1))
            );
            assert_eq!(layout.slot_at(dip(layout.dividers[0], dpi), dpi, fit), None);
            assert_eq!(
                layout.slot_at(dip(84, dpi), dpi, fit),
                Some(Slot::Account(2))
            );
            assert_eq!(layout.slot_at(dip(layout.width, dpi), dpi, fit), None);
        }
    }

    #[test]
    fn layout_preserves_interleaved_provider_order() {
        let tiles = tiles(&[Provider::Codex, Provider::Claude, Provider::Codex]);
        let layout = Layout::new(&tiles);
        assert_eq!(layout.starts, [0, 47, 94]);
        assert_eq!(layout.dividers, [41, 88]);
        assert_eq!(
            layout.slot_at(60, 96, layout.fit(layout.width, 96).unwrap()),
            Some(Slot::Account(1))
        );
    }
}
