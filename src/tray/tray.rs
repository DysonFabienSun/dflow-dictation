use crate::settings;
use dflow_scratchpad::{
    audio::{Session, Update},
    clipboard::{Clipboard, ClipboardFeedback},
    config::{self, Config},
    hotkey::Hotkey,
};
use std::{
    cell::Cell,
    io,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        Controls::EM_REPLACESEL,
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
            SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const TRAY: u32 = WM_APP + 1;
const OPEN_MENU: u32 = WM_APP + 2;
const RESTORE_ICON: u32 = WM_APP + 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const SS_CENTER: u32 = 1;
const SS_CENTERIMAGE: u32 = 0x200;
const TIMER: usize = 1;
const SETTINGS: u32 = 1;
const CANCEL: u32 = 2;
const COPY: u32 = 3;
const EXIT: u32 = 4;

thread_local! {
    static FOCUS_CHANGES: Cell<u64> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
pub struct Binding {
    pub id: i32,
    pub hotkey: Hotkey,
}

impl Binding {
    pub fn register(hwnd: HWND, hotkey: Hotkey, id: i32) -> Result<Self, String> {
        if unsafe { RegisterHotKey(hwnd, id, hotkey.modifiers | MOD_NOREPEAT, hotkey.key) } == 0 {
            return Err(format!(
                "Could not register {hotkey}. Choose another shortcut in Settings."
            ));
        }
        Ok(Self { id, hotkey })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    window: HWND,
    control: HWND,
    changes: u64,
}

impl Target {
    fn focused() -> Option<Self> {
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() || unsafe { GetWindowThreadProcessId(window, null_mut()) } == 0 {
            return None;
        }
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(0, &mut info) } == 0
            || info.hwndFocus.is_null()
            || unsafe { GetAncestor(info.hwndFocus, GA_ROOT) } != window
        {
            return None;
        }
        Some(Self {
            window,
            control: info.hwndFocus,
            changes: FOCUS_CHANGES.get(),
        })
    }
}

unsafe extern "system" fn focus_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND
        || (!hwnd.is_null() && unsafe { GetAncestor(hwnd, GA_ROOT) == GetForegroundWindow() })
    {
        FOCUS_CHANGES.set(FOCUS_CHANGES.get().wrapping_add(1));
    }
}

struct Recording {
    session: Session,
    target: Option<Target>,
    finishing: bool,
    transcript: String,
}

impl Recording {
    fn finish(self, now: Instant) -> Option<Delivery> {
        if self.transcript.trim().is_empty() {
            return None;
        }
        Some(Delivery {
            target: self.target,
            text: self.transcript,
            deadline: now + Duration::from_secs(2),
        })
    }
}

struct Delivery {
    target: Option<Target>,
    text: String,
    deadline: Instant,
}

#[derive(PartialEq, Eq)]
enum DeliveryAction {
    Wait,
    Insert,
    Keep,
}

impl Delivery {
    fn action(&self, focus: Option<Target>, modifiers_down: bool, now: Instant) -> DeliveryAction {
        if self.target.is_none() || self.target != focus || self.text.contains(['\r', '\n', '\0']) {
            DeliveryAction::Keep
        } else if modifiers_down {
            if now < self.deadline {
                DeliveryAction::Wait
            } else {
                DeliveryAction::Keep
            }
        } else {
            DeliveryAction::Insert
        }
    }
}

struct App {
    hwnd: HWND,
    pill: HWND,
    pill_font: Option<(i32, HFONT)>,
    icon: NOTIFYICONDATAW,
    path: PathBuf,
    binding: Option<Binding>,
    recording: Option<Recording>,
    delivery: Option<Delivery>,
    last_transcript: Option<String>,
    status_until: Option<Instant>,
    clipboard: Clipboard,
    copy_pending: bool,
    timer: bool,
    focus_hooks: [HWINEVENTHOOK; 2],
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(null()) }
}

pub fn message(owner: HWND, text: &str) {
    unsafe {
        MessageBoxW(
            owner,
            wide(text).as_ptr(),
            wide("DFlow Tray").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn last_error() -> String {
    io::Error::last_os_error().to_string()
}

pub fn run() -> Result<(), String> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let handle = unsafe { CreateMutexW(null(), 0, wide(r"Local\DFlow.Tray").as_ptr()) };
    let error = unsafe { GetLastError() };
    if handle.is_null() {
        return Err(last_error());
    }
    let _instance = Handle(handle);
    if error == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    let path = config::settings_path().map_err(|e| e.to_string())?;
    let class = wide("DFlowTrayWindow");
    let icon = unsafe { LoadIconW(instance(), 1usize as *const u16) };
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance(),
        lpszClassName: class.as_ptr(),
        hIcon: icon,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(last_error());
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("DFlow Tray").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance(),
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(last_error());
    }
    let pill = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            wide("STATIC").as_ptr(),
            null(),
            WS_POPUP | WS_BORDER | SS_CENTER | SS_CENTERIMAGE,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance(),
            null(),
        )
    };
    if pill.is_null() {
        let error = last_error();
        unsafe {
            DestroyWindow(hwnd);
        }
        return Err(error);
    }
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });
    let hooks = [EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_FOCUS].map(|event| unsafe {
        SetWinEventHook(
            event,
            event,
            null_mut(),
            Some(focus_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    });
    let mut app = App {
        hwnd,
        pill,
        pill_font: None,
        path,
        binding: None,
        recording: None,
        delivery: None,
        last_transcript: None,
        status_until: None,
        clipboard: Clipboard::new(),
        copy_pending: false,
        timer: false,
        focus_hooks: hooks,
        icon: NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY,
            hIcon: icon,
            ..Default::default()
        },
    };
    let config = Config::load(app.path.clone());
    let binding = config
        .tray_hotkey()
        .and_then(|hotkey| Binding::register(hwnd, hotkey, 1));
    match binding {
        Ok(binding) => app.binding = Some(binding),
        Err(error) => app.notice(&error),
    }
    if !app.add_icon() {
        return Err("Could not add the DFlow notification icon.".into());
    }
    if let Some(error) = config.error.as_deref() {
        app.notice(error);
    }
    app.sync_timer();

    let mut msg = MSG::default();
    loop {
        match unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } {
            -1 => return Err(last_error()),
            0 => break,
            _ => {}
        }
        if msg.hwnd == hwnd {
            match msg.message {
                WM_HOTKEY
                    if app
                        .binding
                        .is_some_and(|binding| msg.wParam == binding.id as usize) =>
                {
                    app.toggle()
                }
                WM_TIMER if msg.wParam == TIMER => app.tick(),
                OPEN_MENU => app.menu(),
                RESTORE_ICON => {
                    app.add_icon();
                }
                _ => unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        } else {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        app.sync_timer();
    }
    Ok(())
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        TRAY => {
            // Explorer can send callbacks directly to the window procedure.
            // Queue the action so the main loop owns menu and app state.
            let event = lp as u16 as u32;
            let legacy_right_click = event == WM_RBUTTONUP && lp as usize >> 16 == 0;
            if legacy_right_click
                || matches!(
                    event,
                    WM_LBUTTONUP | WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT
                )
            {
                unsafe {
                    PostMessageW(hwnd, OPEN_MENU, 0, 0);
                }
            }
            0
        }
        event if event != 0 && event == TASKBAR_CREATED.get() => {
            unsafe {
                PostMessageW(hwnd, RESTORE_ICON, 0, 0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl App {
    fn add_icon(&mut self) -> bool {
        self.set_tooltip();
        if unsafe { Shell_NotifyIconW(NIM_ADD, &self.icon) } == 0 {
            return false;
        }
        self.icon.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe {
            Shell_NotifyIconW(NIM_SETVERSION, &self.icon);
        }
        true
    }

    fn set_tooltip(&mut self) {
        let label = if let Some(binding) = self.binding {
            format!("DFlow Tray · {} to start or finish", binding.hotkey)
        } else {
            "DFlow Tray · Shortcut unavailable · Open Settings".into()
        };
        self.icon.szTip.fill(0);
        for (slot, unit) in self.icon.szTip.iter_mut().zip(label.encode_utf16()) {
            *slot = unit;
        }
    }

    fn toggle(&mut self) {
        if let Some(recording) = self.recording.as_mut() {
            if !recording.finishing {
                recording.finishing = true;
                recording.session.stop();
                self.indicator("Transcribing…");
            }
        } else if self.delivery.is_none() {
            let target = Target::focused().filter(|target| supports_insertion(target.control));
            let config = Config::load(self.path.clone());
            if let Some(error) = config.error {
                self.notice(&error);
                return;
            }
            self.recording = Some(Recording {
                session: Session::start(config.settings),
                target,
                finishing: false,
                transcript: String::new(),
            });
            self.indicator("Starting microphone…");
        }
    }

    fn tick(&mut self) {
        while let Some(recording) = &mut self.recording {
            match recording.session.updates.try_recv() {
                Ok(Update::Live) if !recording.finishing => self.indicator("Listening…"),
                Ok(Update::QuietInput(quiet)) if !recording.finishing => self.indicator(if quiet {
                    "Listening… microphone is quiet"
                } else {
                    "Listening…"
                }),
                Ok(Update::Finishing) => {
                    recording.finishing = true;
                    self.indicator("Transcribing…");
                }
                Ok(Update::Live | Update::QuietInput(_)) => {}
                Ok(Update::Transcript(text)) => recording.transcript = text,
                Ok(Update::Done) => {
                    self.delivery = self.recording.take().unwrap().finish(Instant::now());
                    if self.delivery.is_none() {
                        self.notice_for("No speech detected.", Duration::from_secs(1));
                    }
                }
                Ok(Update::Failed(error)) => self.fail(&error),
                Err(TryRecvError::Disconnected) => self.fail("Dictation ended unexpectedly."),
                Err(TryRecvError::Empty) => break,
            }
        }
        if let Some(delivery) = self.delivery.take() {
            match delivery.action(Target::focused(), modifiers_down(), Instant::now()) {
                DeliveryAction::Wait => self.delivery = Some(delivery),
                DeliveryAction::Insert
                    if delivery.target.is_some_and(|target| {
                        Target::focused() == Some(target)
                            && insert_text(target.control, &delivery.text)
                    }) =>
                {
                    self.last_transcript = Some(delivery.text);
                    self.hide()
                }
                _ => {
                    self.last_transcript = Some(delivery.text);
                    self.notice("Text ready to copy from the DFlow tray menu.");
                }
            }
        }
        for feedback in self.clipboard.feedback.try_iter().collect::<Vec<_>>() {
            self.copy_pending = false;
            if self.recording.is_none() && self.delivery.is_none() {
                self.notice(match feedback {
                    ClipboardFeedback::Copied(_) => "Transcript copied.",
                    ClipboardFeedback::Failed => "Could not copy the transcript.",
                });
            }
        }
        if self
            .status_until
            .is_some_and(|until| until <= Instant::now())
        {
            self.hide();
        }
    }

    fn fail(&mut self, error: &str) {
        if let Some(recording) = self
            .recording
            .take()
            .filter(|recording| !recording.transcript.trim().is_empty())
        {
            self.last_transcript = Some(recording.transcript);
            self.notice(&format!(
                "{error}\nPartial transcript available from the tray menu."
            ));
        } else {
            self.notice(error);
        }
    }

    fn menu(&mut self) {
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }
        for (id, label, enabled) in [
            (
                SETTINGS,
                "Settings",
                self.recording.is_none() && self.delivery.is_none(),
            ),
            (
                CANCEL,
                "Cancel recording",
                self.recording.is_some() || self.delivery.is_some(),
            ),
            (
                COPY,
                "Copy last transcript",
                self.last_transcript.is_some() && !self.copy_pending,
            ),
            (EXIT, "Exit", true),
        ] {
            unsafe {
                AppendMenuW(
                    menu,
                    MF_STRING | if enabled { 0 } else { MF_GRAYED },
                    id as usize,
                    wide(label).as_ptr(),
                );
            }
        }
        let mut cursor = POINT::default();
        let command = unsafe {
            GetCursorPos(&mut cursor);
            SetForegroundWindow(self.hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                cursor.x,
                cursor.y,
                0,
                self.hwnd,
                null(),
            );
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            command as u32
        };
        match command {
            SETTINGS => {
                settings::open(self.hwnd, &self.path, &mut self.binding);
                self.set_tooltip();
                unsafe {
                    Shell_NotifyIconW(NIM_MODIFY, &self.icon);
                }
            }
            CANCEL => {
                self.recording.take();
                self.delivery.take();
                self.hide();
            }
            COPY => {
                if let Some(text) = &self.last_transcript {
                    self.copy_pending = self.clipboard.copy(text, None);
                }
            }
            EXIT => unsafe {
                PostQuitMessage(0);
            },
            _ => {}
        }
    }

    fn sync_timer(&mut self) {
        let needed = self.recording.is_some()
            || self.delivery.is_some()
            || self.status_until.is_some()
            || self.copy_pending;
        if needed && !self.timer {
            self.timer = unsafe { SetTimer(self.hwnd, TIMER, 30, None) != 0 };
        } else if !needed && self.timer {
            unsafe {
                KillTimer(self.hwnd, TIMER);
            }
            self.timer = false;
        }
    }

    fn indicator(&mut self, text: &str) {
        self.status_until = None;
        let target = self
            .recording
            .as_ref()
            .and_then(|recording| recording.target)
            .map_or_else(|| unsafe { GetForegroundWindow() }, |target| target.window);
        let monitor = unsafe { MonitorFromWindow(target, MONITOR_DEFAULTTOPRIMARY) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return;
        }
        let work = info.rcWork;
        let dpi = unsafe { GetDpiForWindow(target) }.max(96) as i32;
        self.update_indicator_font(dpi);
        let width = (if text.chars().count() > 31 { 480 } else { 224 }) * dpi / 96;
        let height = (if text.chars().count() > 50 { 68 } else { 36 }) * dpi / 96;
        unsafe {
            SetWindowTextW(self.pill, wide(text).as_ptr());
            SetWindowPos(
                self.pill,
                HWND_TOPMOST,
                (work.left + work.right - width) / 2,
                work.bottom - height - 24 * dpi / 96,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn update_indicator_font(&mut self, dpi: i32) {
        if self.pill_font.is_some_and(|(current, _)| current == dpi) {
            return;
        }
        let font = create_indicator_font(dpi);
        if !font.is_null() {
            unsafe {
                SendMessageW(self.pill, WM_SETFONT, font as WPARAM, 1);
            }
            if let Some((_, previous)) = self.pill_font.replace((dpi, font)) {
                unsafe {
                    DeleteObject(previous);
                }
            }
        }
    }

    fn notice(&mut self, text: &str) {
        self.notice_for(text, Duration::from_secs(2));
    }

    fn notice_for(&mut self, text: &str, duration: Duration) {
        self.indicator(text);
        self.status_until = Some(Instant::now() + duration);
    }

    fn hide(&mut self) {
        self.status_until = None;
        unsafe {
            ShowWindow(self.pill, SW_HIDE);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.recording.take();
        unsafe {
            for hook in self.focus_hooks {
                if !hook.is_null() {
                    UnhookWinEvent(hook);
                }
            }
            if let Some(binding) = self.binding {
                UnregisterHotKey(self.hwnd, binding.id);
            }
            Shell_NotifyIconW(NIM_DELETE, &self.icon);
            DestroyWindow(self.pill);
            if let Some((_, font)) = self.pill_font {
                DeleteObject(font);
            }
            DestroyWindow(self.hwnd);
        }
    }
}

fn create_indicator_font(dpi: i32) -> HFONT {
    let mut font = LOGFONTW {
        lfHeight: -(14 * dpi + 48) / 96,
        lfWeight: FW_NORMAL as i32,
        lfCharSet: ANSI_CHARSET,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (slot, unit) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *slot = unit;
    }
    unsafe { CreateFontIndirectW(&font) }
}

fn modifiers_down() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .into_iter()
        .any(|key| unsafe { GetAsyncKeyState(i32::from(key)) < 0 })
}

fn supports_insertion(control: HWND) -> bool {
    let mut class = [0; 6];
    unsafe {
        if GetClassNameW(control, class.as_mut_ptr(), class.len() as i32) != 4
            || class[..4] != [b'E' as u16, b'd' as u16, b'i' as u16, b't' as u16]
            || IsWindowUnicode(control) == 0
            || IsWindowEnabled(control) == 0
        {
            return false;
        }
        let style = GetWindowLongW(control, GWL_STYLE);
        // Edit controls without automatic scrolling can truncate text at the visible bounds.
        let wraps_and_scrolls = ES_MULTILINE | ES_AUTOVSCROLL;
        style & (ES_READONLY | ES_PASSWORD | ES_NUMBER) == 0
            && (style & ES_AUTOHSCROLL != 0 || style & wraps_and_scrolls == wraps_and_scrolls)
    }
}

fn insert_text(control: HWND, text: &str) -> bool {
    let text = wide(text);
    if !supports_insertion(control) {
        return false;
    }
    // Address the captured control directly so a foreground switch cannot redirect the text.
    unsafe {
        SendMessageTimeoutW(
            control,
            EM_REPLACESEL,
            1,
            text.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            200,
            null_mut(),
        ) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_edit(style: u32) -> HWND {
        unsafe {
            CreateWindowExW(
                0,
                wide("Edit").as_ptr(),
                wide("original").as_ptr(),
                style,
                0,
                0,
                100,
                24,
                null_mut(),
                null_mut(),
                instance(),
                null(),
            )
        }
    }

    fn edit_text(control: HWND) -> String {
        let mut buffer = [0; 32];
        let length = unsafe { GetWindowTextW(control, buffer.as_mut_ptr(), 32) };
        String::from_utf16_lossy(&buffer[..length as usize])
    }

    #[test]
    fn insertion_changes_only_the_addressed_edit_control() {
        let style = WS_POPUP | ES_AUTOHSCROLL as u32;
        let target = create_edit(style);
        let other = create_edit(style);
        let readonly = create_edit(style | ES_READONLY as u32);
        let bounded = create_edit(WS_POPUP);
        let controls = [target, other, readonly, bounded];
        assert!(controls.iter().all(|control| !control.is_null()));
        assert!(insert_text(target, "é界 "));
        assert_eq!(edit_text(target), "é界 original");
        assert_eq!(edit_text(other), "original");
        assert!(!insert_text(readonly, "ignored"));
        assert_eq!(edit_text(readonly), "original");
        assert!(!insert_text(bounded, "ignored"));
        assert_eq!(edit_text(bounded), "original");
        for control in controls {
            unsafe { DestroyWindow(control) };
        }
    }

    #[test]
    fn insertion_preserves_unicode_across_processes() {
        use std::{fs, os::windows::process::CommandExt, process::Command, thread};
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        const HELPER_PATH: &str = "DFLOW_TEST_EDIT_CONTROL_DIRECTORY";
        const CHECK_TEXT: u32 = WM_APP + 22;
        const TRANSCRIPT: &str = "é界🙂 text ";
        if let Some(path) = std::env::var_os(HELPER_PATH) {
            let control = create_edit(WS_POPUP | ES_AUTOHSCROLL as u32);
            assert!(!control.is_null());
            let timer = unsafe { SetTimer(control, 1, 10_000, None) };
            assert_ne!(timer, 0);
            fs::write(
                PathBuf::from(path).join("handle"),
                (control as usize).to_string(),
            )
            .unwrap();
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
                assert_ne!(msg.message, WM_TIMER, "parent did not signal completion");
                if msg.hwnd == control && msg.message == CHECK_TEXT {
                    assert_eq!(edit_text(control), format!("{TRANSCRIPT}original"));
                    unsafe { DestroyWindow(control) };
                    return;
                }
                unsafe { DispatchMessageW(&msg) };
            }
            panic!("edit control message loop ended early");
        }

        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tray::tests::insertion_preserves_unicode_across_processes",
            ])
            .env(HELPER_PATH, root.path())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle_file = root.path().join("handle");
        let control = loop {
            if let Ok(handle) = fs::read_to_string(&handle_file)
                && let Ok(handle) = handle.parse::<usize>()
            {
                break handle as HWND;
            }
            if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("edit control helper did not start");
            }
            thread::sleep(Duration::from_millis(10));
        };
        let inserted = insert_text(control, TRANSCRIPT);
        let signalled = unsafe { PostMessageW(control, CHECK_TEXT, 0, 0) } != 0;
        assert!(child.wait().unwrap().success());
        assert!(inserted && signalled);
    }

    #[test]
    fn indicator_resolves_to_segoe_ui() {
        let font = create_indicator_font(96);
        assert!(!font.is_null());
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        assert!(!dc.is_null());
        let previous = unsafe { SelectObject(dc, font) };
        let mut name = [0; 32];
        let length = unsafe { GetTextFaceW(dc, name.len() as i32, name.as_mut_ptr()) };
        unsafe {
            SelectObject(dc, previous);
            DeleteDC(dc);
            DeleteObject(font);
        }
        assert!(length > 0);
        let end = name.iter().position(|&unit| unit == 0).unwrap();
        assert_eq!(String::from_utf16_lossy(&name[..end]), "Segoe UI");
    }

    #[test]
    fn completed_recording_offers_latest_revision_and_only_once() {
        let (session, send) = Session::test_channel();
        send.send(Update::Transcript("We should meet Wednesday.".into()))
            .unwrap();
        send.send(Update::Transcript("We should meet Tuesday.".into()))
            .unwrap();
        send.send(Update::Done).unwrap();
        let target = Target {
            window: 1usize as HWND,
            control: 2usize as HWND,
            changes: 0,
        };
        let mut recording = Some(Recording {
            session,
            target: Some(target),
            finishing: true,
            transcript: String::new(),
        });
        let now = Instant::now();
        let mut delivery = None;
        while let Some(active) = recording.as_mut() {
            match active.session.updates.try_recv().unwrap() {
                Update::Transcript(text) => active.transcript = text,
                Update::Done => delivery = recording.take().unwrap().finish(now),
                _ => unreachable!(),
            }
        }
        assert!(
            delivery.as_ref().is_some_and(
                |result| result.action(Some(target), true, now) == DeliveryAction::Wait
            )
        );
        let result = delivery.take().unwrap();
        assert_eq!(result.text, "We should meet Tuesday.");
        assert!(result.action(Some(target), false, now) == DeliveryAction::Insert);
        assert!(delivery.take().is_none());
    }

    #[test]
    fn delivery_keeps_the_transcript_after_focus_change_or_for_multiline_text() {
        let now = Instant::now();
        let target = Target {
            window: 1usize as HWND,
            control: 2usize as HWND,
            changes: 0,
        };
        let mut result = Delivery {
            target: Some(target),
            text: "Text".into(),
            deadline: now,
        };
        let changed = Target {
            changes: 1,
            ..target
        };
        assert!(result.action(Some(changed), false, now) == DeliveryAction::Keep);
        assert!(result.action(Some(target), true, now) == DeliveryAction::Keep);
        result.text = "First line\nSecond line".into();
        assert!(result.action(Some(target), false, now) == DeliveryAction::Keep);
    }
}
