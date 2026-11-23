use crate::settings;
use dflow_scratchpad::{
    audio::{Session, Update},
    clipboard::{Clipboard, ClipboardFeedback},
    config::Config,
    hotkey::Hotkey,
    locale::Locale,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    io,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
            GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const TRAY: u32 = WM_APP + 1;
const OPEN_MENU: u32 = WM_APP + 2;
const RESTORE_ICON: u32 = WM_APP + 3;
const PREVIEW_ACTION: u32 = WM_APP + 4;
const PREVIEW_HOLD: u32 = WM_APP + 5;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const CANCEL_WINDOW: Duration = Duration::from_secs(2);
const ABORT_HOLD: Duration = Duration::from_millis(750);
const COMPLETED_PREVIEW_TIME: Duration = Duration::from_secs(3);
const LAUNCH_PREVIEW_TIME: Duration = Duration::from_secs(7);
const PREVIEW_LINES: usize = 3;
const PREVIEW_SIDE_PADDING: i32 = 12;
const PREVIEW_VERTICAL_PADDING: i32 = 10;
const PREVIEW_LINE_GAP: i32 = 3;
const PREVIEW_ACCENT_WIDTH: i32 = 3;
const SHORTCUT_HEIGHT: i32 = 23;
const SHORTCUT_GAP: i32 = 4;
const PROGRESS_CYCLE_MS: isize = 1100;
const TICK_MS: u32 = 30;
const PILL_WELCOME_SLOT: i32 = size_of::<isize>() as i32;
const PILL_BADGE_SLOT: i32 = PILL_WELCOME_SLOT + size_of::<isize>() as i32;
const SHORTCUT_COPY_READY_SLOT: i32 = size_of::<isize>() as i32;
const TIMER: usize = 1;
const SETTINGS: u32 = 1;
const CANCEL: u32 = 2;
const COPY: u32 = 3;
const EXIT: u32 = 4;
const SETUP: u32 = 5;

thread_local! {
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

#[derive(Clone, Copy)]
pub struct Binding {
    pub id: i32,
    pub hotkey: Hotkey,
}

impl Binding {
    pub fn register(hwnd: HWND, hotkey: Hotkey, id: i32, locale: Locale) -> Result<Self, String> {
        if unsafe { RegisterHotKey(hwnd, id, hotkey.modifiers | MOD_NOREPEAT, hotkey.key) } == 0 {
            return Err(match locale {
                Locale::English => {
                    format!("Could not register {hotkey}. Choose another shortcut in Settings.")
                }
                Locale::SimplifiedChinese => format!("无法注册 {hotkey}，请在设置中选择其他快捷键"),
            });
        }
        Ok(Self { id, hotkey })
    }
}

struct Recording {
    session: Session,
    started_at: Instant,
    can_stop: bool,
    listening: bool,
    quiet: bool,
    finishing: bool,
    cleaning: bool,
    cleanup_error: Option<String>,
    transcript: String,
}

#[derive(Clone, Copy)]
enum HoldInput {
    Hotkey(u32),
    Button,
}

#[derive(Clone, Copy)]
struct DictationHold {
    started_at: Instant,
    input: HoldInput,
}

enum CopyOrigin {
    Manual,
    BeforeCleanup,
    Final(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
enum PreviewBadge {
    Cancel = 0,
    Stop = 1,
    SkipCleanup,
    Dismiss,
    Transcribing,
    Copying,
    Copied,
    CopyFailed,
    Listening,
    CopyLast,
    Holding,
}

impl PreviewBadge {
    fn from_id(id: isize) -> Option<Self> {
        const BADGES: [PreviewBadge; 11] = [
            PreviewBadge::Cancel,
            PreviewBadge::Stop,
            PreviewBadge::SkipCleanup,
            PreviewBadge::Dismiss,
            PreviewBadge::Transcribing,
            PreviewBadge::Copying,
            PreviewBadge::Copied,
            PreviewBadge::CopyFailed,
            PreviewBadge::Listening,
            PreviewBadge::CopyLast,
            PreviewBadge::Holding,
        ];
        BADGES.get(id as usize).copied()
    }

    fn working(self) -> bool {
        matches!(self, Self::Transcribing | Self::SkipCleanup | Self::Copying)
    }

    fn clickable(self) -> bool {
        matches!(
            self,
            Self::Cancel
                | Self::Stop
                | Self::SkipCleanup
                | Self::Dismiss
                | Self::CopyLast
                | Self::Holding
        )
    }

    fn accent(self) -> Option<COLORREF> {
        match self {
            Self::Listening | Self::Cancel | Self::Stop | Self::CopyLast => Some(rgb(0, 129, 146)),
            Self::Transcribing => Some(rgb(27, 104, 227)),
            Self::SkipCleanup => Some(rgb(126, 69, 211)),
            Self::Copying => Some(rgb(0, 119, 193)),
            Self::Copied => Some(rgb(0, 137, 77)),
            Self::CopyFailed => Some(rgb(184, 70, 24)),
            Self::Holding => Some(rgb(207, 66, 39)),
            Self::Dismiss => None,
        }
    }

    fn countdown(action: isize) -> Option<Duration> {
        match action {
            value if value == Self::Dismiss as isize => Some(LAUNCH_PREVIEW_TIME),
            value if value == Self::Copied as isize => Some(COMPLETED_PREVIEW_TIME),
            _ => None,
        }
    }

    fn label(self, hotkey: Option<Hotkey>, locale: Locale) -> String {
        let label = match self {
            Self::Cancel => locale.text("CANCEL", "取消"),
            Self::Stop => locale.text("STOP & COPY", "停止并复制"),
            Self::SkipCleanup => locale.text("Skip cleanup", "跳过整理"),
            Self::Dismiss => locale.text("DISMISS", "关闭提示"),
            Self::Transcribing => locale.text("Finishing…", "正在完成转写…"),
            Self::Copying => locale.text("Copying…", "正在复制…"),
            Self::Copied => locale.text("COPIED", "已复制"),
            Self::CopyFailed => locale.text("Copy failed", "复制失败"),
            Self::Listening => locale.text("Listening…", "正在录音…"),
            Self::CopyLast => locale.text("Copy last transcript", "复制上次转写结果"),
            Self::Holding => locale.text("Hold to abort", "长按取消听写"),
        };
        match hotkey {
            Some(hotkey) if matches!(self, Self::Cancel | Self::Stop | Self::SkipCleanup) => {
                format!("{label} · {}", hotkey.to_string().to_ascii_uppercase())
            }
            _ => label.into(),
        }
    }
}

impl Recording {
    fn update_cancel_window(&mut self, now: Instant) -> bool {
        if !self.finishing
            && !self.can_stop
            && now.saturating_duration_since(self.started_at) >= CANCEL_WINDOW
        {
            self.can_stop = true;
            return true;
        }
        false
    }

    fn finish_requested(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.started_at) < CANCEL_WINDOW {
            return false;
        }
        self.can_stop = true;
        self.finishing = true;
        self.session.stop();
        true
    }

    fn previewing(&self) -> bool {
        !self.transcript.trim().is_empty()
    }

    fn preview_badge(&self) -> PreviewBadge {
        if self.cleaning {
            PreviewBadge::SkipCleanup
        } else if self.finishing {
            PreviewBadge::Transcribing
        } else if self.can_stop {
            PreviewBadge::Stop
        } else {
            PreviewBadge::Cancel
        }
    }

    fn status(&self, locale: Locale) -> String {
        if self.previewing() {
            return self.transcript.clone();
        }
        let activity = if self.finishing {
            locale.text("Transcribing…", "正在转写…")
        } else if self.quiet {
            locale.text(
                "Listening… microphone is quiet",
                "正在录音… · 麦克风输入音量较低",
            )
        } else if self.listening {
            locale.text("Listening…", "正在录音…")
        } else {
            locale.text("Starting microphone…", "正在启动麦克风…")
        };
        if !self.can_stop {
            format!(
                "{activity} · {}",
                locale.text("Stop now to discard", "此时停止将丢弃录音")
            )
        } else if !self.finishing && self.listening && !self.quiet {
            locale
                .text(
                    "Listening… · Waiting for transcript",
                    "正在录音… · 等待转写结果",
                )
                .into()
        } else {
            activity.into()
        }
    }
}

fn transcript_preview(text: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rows = VecDeque::with_capacity(PREVIEW_LINES);
    let mut push = |row| {
        if rows.len() == PREVIEW_LINES {
            rows.pop_front();
        }
        rows.push_back(row);
    };
    let mut start = 0;
    let mut end = 0;
    for word in line.split(' ') {
        let word_start = end + usize::from(end != 0);
        let word_end = word_start + word.len();
        if !fits(&line[start..word_end]) {
            if start < word_start {
                push(&line[start..end]);
                start = word_start;
            }
            if !fits(&line[start..word_end]) {
                for (offset, grapheme) in word.grapheme_indices(true) {
                    let at = word_start + offset;
                    if start < at && !fits(&line[start..at + grapheme.len()]) {
                        push(&line[start..at]);
                        start = at;
                    }
                }
            }
        }
        end = word_end;
    }
    push(&line[start..end]);
    rows.make_contiguous().join("\r\n")
}

struct App {
    locale: Locale,
    hwnd: HWND,
    pill: HWND,
    preview_shortcut: HWND,
    copy_shortcut: HWND,
    pill_font: Option<(i32, HFONT)>,
    icon: NOTIFYICONDATAW,
    path: PathBuf,
    binding: Option<Binding>,
    recording: Option<Recording>,
    hold: Option<DictationHold>,
    last_transcript: Option<String>,
    recording_id: u64,
    completed_preview: Option<PreviewBadge>,
    status_until: Option<Instant>,
    clipboard: Clipboard,
    copies_pending: VecDeque<CopyOrigin>,
    timer: bool,
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(null()) }
}

pub fn message(owner: HWND, text: &str, locale: Locale) {
    unsafe {
        MessageBoxW(
            owner,
            wide(text).as_ptr(),
            wide(locale.dictation_name()).as_ptr(),
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

struct SmallIcon(HICON);

impl SmallIcon {
    fn load(resource: *const u16) -> Self {
        unsafe {
            let taskbar = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
            let dpi = match GetDpiForWindow(taskbar) {
                0 => GetDpiForSystem(),
                dpi => dpi,
            };
            let width = GetSystemMetricsForDpi(SM_CXSMICON, dpi);
            let height = GetSystemMetricsForDpi(SM_CYSMICON, dpi);
            Self(LoadImageW(
                instance(),
                resource,
                IMAGE_ICON,
                width,
                height,
                0,
            ))
        }
    }
}

impl Drop for SmallIcon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyIcon(self.0) };
        }
    }
}

fn last_error() -> String {
    io::Error::last_os_error().to_string()
}

pub fn run(mut config: Config) -> Result<(), String> {
    let locale = config.locale;
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
    let class = wide("DFlowTrayWindow");
    let icon_resource = 1usize as *const u16;
    let icon = unsafe { LoadIconW(instance(), icon_resource) };
    let tray_icon = SmallIcon::load(icon_resource);
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
            wide(locale.dictation_name()).as_ptr(),
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
    let pill = create_pill(hwnd);
    if pill.is_null() {
        let error = last_error();
        unsafe {
            DestroyWindow(hwnd);
        }
        return Err(error);
    }
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });
    let mut app = App {
        locale,
        hwnd,
        pill,
        preview_shortcut: create_preview_shortcut(pill),
        copy_shortcut: create_preview_shortcut(pill),
        pill_font: None,
        path: config.path.clone(),
        binding: None,
        recording: None,
        hold: None,
        last_transcript: None,
        recording_id: 0,
        completed_preview: None,
        status_until: None,
        clipboard: Clipboard::new(),
        copies_pending: VecDeque::new(),
        timer: false,
        icon: NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: TRAY,
            hIcon: if tray_icon.0.is_null() {
                icon
            } else {
                tray_icon.0
            },
            ..Default::default()
        },
    };
    let binding = config
        .tray_hotkey()
        .and_then(|hotkey| Binding::register(hwnd, hotkey, 1, locale));
    match binding {
        Ok(binding) => app.binding = Some(binding),
        Err(error) => app.notice(&error),
    }
    if !app.add_icon() {
        return Err(locale
            .text(
                "Could not add the DFlow notification icon.",
                "无法添加 DFlow 托盘图标",
            )
            .into());
    }
    if config.settings.cloud_dictation.is_none() {
        match settings::ask_consent(hwnd, config.settings.proxy.clone(), locale) {
            Ok(Some((enabled, proxy))) => {
                let mut choice = config.settings.clone();
                choice.cloud_dictation = Some(enabled);
                choice.proxy = proxy;
                match config.save(choice) {
                    Ok(()) if enabled => {
                        settings::open(hwnd, &config.path, &mut app.binding, locale, true)
                    }
                    Ok(()) => {}
                    Err(error) => message(hwnd, &error.restart_message(&config), locale),
                }
            }
            Ok(None) => {}
            Err(error) => message(hwnd, &error, locale),
        }
        config = Config::load_for(config.path.clone(), Some(locale));
    }
    if let Some(error) = config.error.as_deref() {
        app.notice(error);
    } else if config.settings.cloud_dictation == Some(true)
        && let Some(binding) = app.binding
    {
        match config.tray_launch_preview() {
            Ok(true) => app.show_launch_preview(binding.hotkey),
            Ok(false) => {}
            Err(error) => app.notice(&error),
        }
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
                    app.dictation_key_pressed()
                }
                PREVIEW_HOLD if msg.lParam as HWND == app.preview_shortcut => {
                    app.begin_button_hold();
                }
                PREVIEW_ACTION
                    if msg.lParam as HWND == app.preview_shortcut
                        || msg.lParam as HWND == app.copy_shortcut =>
                {
                    app.activate_preview_shortcut(msg.wParam as isize);
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

fn blend_color(base: COLORREF, ink: COLORREF, ink_parts: u32) -> COLORREF {
    let mut color = 0;
    for shift in [0, 8, 16] {
        let base_channel = (base >> shift) & 0xff;
        let ink_channel = (ink >> shift) & 0xff;
        color |= (((12 - ink_parts) * base_channel + ink_parts * ink_channel) / 12) << shift;
    }
    color
}

const fn rgb(red: u32, green: u32, blue: u32) -> COLORREF {
    red | (green << 8) | (blue << 16)
}

fn key_down(key: u32) -> bool {
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}

fn high_contrast() -> bool {
    let mut settings = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            settings.cbSize,
            (&mut settings as *mut HIGHCONTRASTW).cast(),
            0,
        ) != 0
            && settings.dwFlags & HCF_HIGHCONTRASTON != 0
    }
}

fn scale_preview(px: i32, dpi: i32) -> i32 {
    (px * dpi + 48) / 96
}

fn preview_text_height(rows: i32, line_height: i32, dpi: i32) -> i32 {
    rows * line_height + (rows - 1) * scale_preview(PREVIEW_LINE_GAP, dpi)
}

fn line_height(dc: HDC, dpi: i32) -> i32 {
    let mut metrics = TEXTMETRICW::default();
    if unsafe { GetTextMetricsW(dc, &mut metrics) } != 0 {
        metrics.tmHeight
    } else {
        scale_preview(18, dpi)
    }
}

fn create_pill(owner: HWND) -> HWND {
    let class = wide("DFlowPreview");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(pill_proc),
        cbWndExtra: PILL_BADGE_SLOT + size_of::<isize>() as i32,
        hInstance: instance(),
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    unsafe {
        if RegisterClassW(&window_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return null_mut();
        }
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            class.as_ptr(),
            null(),
            WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
            0,
            0,
            0,
            0,
            owner,
            null_mut(),
            instance(),
            null(),
        )
    }
}

unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                paint_preview_window(hwnd, draw_pill);
                0
            }
            WM_PRINTCLIENT => {
                draw_pill(hwnd, wp as HDC);
                0
            }
            WM_NCHITTEST => HTTRANSPARENT as LRESULT,
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn paint_preview_window(hwnd: HWND, draw: fn(HWND, HDC)) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let mut area = RECT::default();
        GetClientRect(hwnd, &mut area);
        let buffer = CreateCompatibleDC(dc);
        let bitmap = CreateCompatibleBitmap(dc, area.right, area.bottom);
        if buffer.is_null() || bitmap.is_null() {
            draw(hwnd, dc);
        } else {
            let previous = SelectObject(buffer, bitmap);
            draw(hwnd, buffer);
            BitBlt(dc, 0, 0, area.right, area.bottom, buffer, 0, 0, SRCCOPY);
            SelectObject(buffer, previous);
        }
        if !bitmap.is_null() {
            DeleteObject(bitmap);
        }
        if !buffer.is_null() {
            DeleteDC(buffer);
        }
        EndPaint(hwnd, &paint);
    }
}

fn set_preview_text(hwnd: HWND, text: &[u16]) -> bool {
    unsafe {
        if GetWindowTextLengthW(hwnd) as usize + 1 == text.len() {
            let mut current = vec![0; text.len()];
            GetWindowTextW(hwnd, current.as_mut_ptr(), current.len() as i32);
            if current == text {
                return false;
            }
        }
        SetWindowTextW(hwnd, text.as_ptr()) != 0
    }
}

fn place_preview_window(hwnd: HWND, left: i32, top: i32, width: i32, height: i32) {
    unsafe {
        let mut current = RECT::default();
        if IsWindowVisible(hwnd) == 0
            || GetWindowRect(hwnd, &mut current) == 0
            || (current.left, current.top, current.right, current.bottom)
                != (left, top, left + width, top + height)
        {
            SetWindowPos(
                hwnd,
                null_mut(),
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOZORDER,
            );
            InvalidateRect(hwnd, null(), 0);
        }
    }
}

fn draw_pill(hwnd: HWND, dc: HDC) {
    unsafe {
        let mut area = RECT::default();
        GetClientRect(hwnd, &mut area);
        let layout_dpi = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as i32;
        let preview = layout_dpi > 0;
        let dpi = layout_dpi.abs().max(96);
        let face = GetSysColor(COLOR_BTNFACE);
        let badge = PreviewBadge::from_id(GetWindowLongPtrW(hwnd, PILL_BADGE_SLOT) - 1);
        let accent = badge.and_then(PreviewBadge::accent);
        let contrast = high_contrast();
        let white_welcome = badge == Some(PreviewBadge::Dismiss) && !contrast;
        let ink = if white_welcome {
            rgb(23, 23, 23)
        } else {
            GetSysColor(COLOR_BTNTEXT)
        };
        let background = if white_welcome {
            rgb(255, 255, 255)
        } else {
            match accent {
                Some(accent) if !contrast => blend_color(face, accent, 2),
                _ if preview && !contrast => blend_color(face, ink, 1),
                _ => face,
            }
        };
        SetDCBrushColor(dc, background);
        FillRect(dc, &area, GetStockObject(DC_BRUSH));
        if let Some(accent) = accent {
            let edge = RECT {
                right: scale_preview(PREVIEW_ACCENT_WIDTH, dpi),
                ..area
            };
            SetDCBrushColor(
                dc,
                if contrast {
                    GetSysColor(COLOR_HIGHLIGHT)
                } else {
                    accent
                },
            );
            FillRect(dc, &edge, GetStockObject(DC_BRUSH));
        }
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, ink);
        let font = GetWindowLongPtrW(hwnd, 0) as HFONT;
        let previous = if font.is_null() {
            null_mut()
        } else {
            SelectObject(dc, font)
        };
        let mut text = vec![0; GetWindowTextLengthW(hwnd) as usize + 1];
        let length = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
        let inset = scale_preview(PREVIEW_SIDE_PADDING, dpi);
        area.left += inset;
        area.right -= inset;
        if preview {
            let rows = text[..length as usize].split(|&unit| unit == b'\n' as u16);
            let height = line_height(dc, dpi);
            let block = preview_text_height(rows.clone().count() as i32, height, dpi);
            area.top += (area.bottom - area.top - block).max(0) / 2;
            let prefix = GetWindowLongPtrW(hwnd, PILL_WELCOME_SLOT) as usize;
            for (index, row) in rows.enumerate() {
                let row = row.strip_suffix(&[b'\r' as u16]).unwrap_or(row);
                area.bottom = area.top + height;
                if !(prefix > 0 && index == 0 && draw_welcome_title(dc, row, area, dpi, prefix)) {
                    DrawTextW(
                        dc,
                        row.as_ptr(),
                        row.len() as i32,
                        &mut area,
                        DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                    );
                }
                area.top += height + scale_preview(PREVIEW_LINE_GAP, dpi);
            }
        } else {
            let flags = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
            let mut measured = area;
            DrawTextW(
                dc,
                text.as_ptr(),
                length,
                &mut measured,
                flags | DT_CALCRECT,
            );
            area.top += (area.bottom - measured.bottom).max(0) / 2;
            DrawTextW(dc, text.as_ptr(), length, &mut area, flags);
        }
        if !previous.is_null() {
            SelectObject(dc, previous);
        }
    }
}

fn welcome_prefix(locale: Locale) -> &'static str {
    locale.text("Welcome to ", "欢迎使用 ")
}

fn draw_welcome_title(dc: HDC, row: &[u16], area: RECT, dpi: i32, prefix: usize) -> bool {
    if row.len() <= prefix {
        return false;
    }
    let (welcome, name) = row.split_at(prefix);
    let bold = create_indicator_font_with_weight(dpi, 14, FW_BOLD);
    if bold.is_null() {
        return false;
    }
    unsafe {
        let mut regular_size = SIZE::default();
        let mut bold_size = SIZE::default();
        GetTextExtentPoint32W(
            dc,
            welcome.as_ptr(),
            welcome.len() as i32,
            &mut regular_size,
        );
        let regular = SelectObject(dc, bold);
        GetTextExtentPoint32W(dc, name.as_ptr(), name.len() as i32, &mut bold_size);
        let start = (area.left + area.right - regular_size.cx - bold_size.cx) / 2;
        let top = area.top + (area.bottom - area.top - bold_size.cy) / 2;
        TextOutW(
            dc,
            start + regular_size.cx,
            top,
            name.as_ptr(),
            name.len() as i32,
        );
        SelectObject(dc, regular);
        TextOutW(dc, start, top, welcome.as_ptr(), welcome.len() as i32);
        DeleteObject(bold);
    }
    true
}

fn create_preview_shortcut(pill: HWND) -> HWND {
    let class = wide("DFlowPreviewShortcut");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(preview_shortcut_proc),
        cbWndExtra: 2 * size_of::<isize>() as i32,
        hInstance: instance(),
        lpszClassName: class.as_ptr(),
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_HAND) },
        ..Default::default()
    };
    unsafe {
        if RegisterClassW(&window_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return null_mut();
        }
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class.as_ptr(),
            null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            pill,
            null_mut(),
            instance(),
            null(),
        )
    }
}

unsafe extern "system" fn preview_shortcut_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_ERASEBKGND => 1,
            WM_SETCURSOR
                if !PreviewBadge::from_id(GetWindowLongPtrW(hwnd, GWLP_USERDATA))
                    .is_some_and(PreviewBadge::clickable) =>
            {
                SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
                1
            }
            WM_LBUTTONDOWN
                if PreviewBadge::from_id(GetWindowLongPtrW(hwnd, GWLP_USERDATA))
                    .is_some_and(PreviewBadge::clickable) =>
            {
                SetCapture(hwnd);
                InvalidateRect(hwnd, null(), 0);
                let action = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if action == PreviewBadge::Stop as isize || action == PreviewBadge::Cancel as isize
                {
                    PostMessageW(
                        GetAncestor(hwnd, GA_ROOTOWNER),
                        PREVIEW_HOLD,
                        0,
                        hwnd as LPARAM,
                    );
                }
                0
            }
            WM_CAPTURECHANGED => {
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONUP if GetCapture() == hwnd => {
                ReleaseCapture();
                let point = POINT {
                    x: i32::from(lp as i16),
                    y: i32::from((lp >> 16) as i16),
                };
                let mut area = RECT::default();
                let action = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if PreviewBadge::from_id(action).is_some_and(PreviewBadge::clickable)
                    && GetClientRect(hwnd, &mut area) != 0
                    && PtInRect(&area, point) != 0
                {
                    let owner = GetAncestor(hwnd, GA_ROOTOWNER);
                    PostMessageW(owner, PREVIEW_ACTION, action as WPARAM, hwnd as LPARAM);
                }
                0
            }
            WM_PAINT => {
                paint_preview_window(hwnd, draw_preview_shortcut);
                0
            }
            WM_PRINTCLIENT => {
                draw_preview_shortcut(hwnd, wp as HDC);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn draw_preview_shortcut(hwnd: HWND, dc: HDC) {
    let mut area = RECT::default();
    let mut text = [0; 64];
    unsafe {
        GetClientRect(hwnd, &mut area);
        let action = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        let badge = PreviewBadge::from_id(action).unwrap_or(PreviewBadge::Cancel);
        let copy_button = badge == PreviewBadge::CopyLast;
        let face = GetSysColor(COLOR_BTNFACE);
        let ink = GetSysColor(COLOR_BTNTEXT);
        let highlight = GetSysColor(COLOR_HIGHLIGHT);
        let contrast = high_contrast();
        let accent = if contrast {
            highlight
        } else if copy_button && GetWindowLongPtrW(hwnd, SHORTCUT_COPY_READY_SLOT) != 0 {
            rgb(54, 98, 172)
        } else {
            badge.accent().unwrap_or(highlight)
        };
        let pressed = GetCapture() == hwnd && badge != PreviewBadge::Holding;
        let background = if pressed {
            highlight
        } else if contrast {
            face
        } else {
            match badge {
                PreviewBadge::Cancel | PreviewBadge::Stop => blend_color(face, ink, 1),
                PreviewBadge::CopyLast => blend_color(face, accent, 2),
                _ => blend_color(face, accent, 3),
            }
        };
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        SetDCBrushColor(dc, background);
        FillRect(dc, &area, GetStockObject(DC_BRUSH));
        if badge.clickable() && badge != PreviewBadge::Holding {
            SetDCBrushColor(
                dc,
                if pressed {
                    GetSysColor(COLOR_HIGHLIGHTTEXT)
                } else if contrast {
                    ink
                } else if copy_button {
                    GetSysColor(COLOR_WINDOWFRAME)
                } else {
                    accent
                },
            );
            FrameRect(dc, &area, GetStockObject(DC_BRUSH));
        }
        if copy_button {
            let owner = GetWindow(hwnd, GW_OWNER);
            let preview_dpi = (GetWindowLongPtrW(owner, GWLP_USERDATA) as i32)
                .abs()
                .max(96);
            let inset = GetSystemMetricsForDpi(SM_CXBORDER, preview_dpi as u32);
            let edge = RECT {
                left: inset,
                top: 1,
                right: inset + scale_preview(PREVIEW_ACCENT_WIDTH, preview_dpi),
                bottom: area.bottom - 1,
            };
            SetDCBrushColor(
                dc,
                if pressed {
                    GetSysColor(COLOR_HIGHLIGHTTEXT)
                } else {
                    accent
                },
            );
            FillRect(dc, &edge, GetStockObject(DC_BRUSH));
            area.left = edge.right;
        }
        let progress = GetWindowLongPtrW(hwnd, 0);
        let width = area.right;
        let mut track = area;
        if badge.working() {
            let segment = width / 3;
            let start =
                ((width + segment) as isize * progress / PROGRESS_CYCLE_MS) as i32 - segment;
            track.left = start.max(0);
            track.right = (start + segment).min(width);
        } else if let Some(duration) = PreviewBadge::countdown(action) {
            track.right = (width as isize * progress / duration.as_millis() as isize) as i32;
        } else if badge == PreviewBadge::Holding {
            track.right = (width as isize * progress / ABORT_HOLD.as_millis() as isize) as i32;
        } else {
            track.right = 0;
        }
        if track.right > track.left {
            track.top = track.bottom - scale_preview(3, dpi);
            SetDCBrushColor(
                dc,
                if pressed && contrast {
                    GetSysColor(COLOR_HIGHLIGHTTEXT)
                } else {
                    accent
                },
            );
            FillRect(dc, &track, GetStockObject(DC_BRUSH));
        }
        area.bottom -= scale_preview(2, dpi);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(
            dc,
            if pressed {
                GetSysColor(COLOR_HIGHLIGHTTEXT)
            } else {
                ink
            },
        );
        let alignment = if matches!(badge, PreviewBadge::Cancel | PreviewBadge::Stop) {
            area.right -= scale_preview(9, dpi);
            DT_RIGHT
        } else {
            DT_CENTER
        };
        let font = create_indicator_font(dpi, 11);
        if !font.is_null() {
            let previous = SelectObject(dc, font);
            let length = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
            DrawTextW(
                dc,
                text.as_ptr(),
                length,
                &mut area,
                alignment | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(dc, previous);
            DeleteObject(font);
        }
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
            match self.locale {
                Locale::English => {
                    format!("DFlow Dictation · {} to start or finish", binding.hotkey)
                }
                Locale::SimplifiedChinese => {
                    format!("DFlow 听写 · {} 开始或结束听写", binding.hotkey)
                }
            }
        } else {
            self.locale
                .text(
                    "DFlow Dictation · Shortcut unavailable · Open Settings",
                    "DFlow 听写 · 快捷键不可用 · 请打开设置",
                )
                .into()
        };
        self.icon.szTip.fill(0);
        for (slot, unit) in self.icon.szTip.iter_mut().zip(label.encode_utf16()) {
            *slot = unit;
        }
    }

    fn dictation_key_pressed(&mut self) {
        let now = Instant::now();
        let Some(recording) = &self.recording else {
            self.toggle(now);
            return;
        };
        if recording.cleaning {
            self.finish_recording();
            return;
        }
        if let Some(hold) = self.hold {
            // MOD_NOREPEAT means another hotkey message follows a release.
            if matches!(hold.input, HoldInput::Hotkey(_)) {
                self.finish_hold(hold, now);
            }
            return;
        }
        let Some(binding) = self.binding else { return };
        if key_down(binding.hotkey.key) {
            self.begin_hold(HoldInput::Hotkey(binding.hotkey.key), now);
        } else {
            self.toggle(now);
        }
    }

    fn begin_button_hold(&mut self) {
        if unsafe { GetCapture() == self.preview_shortcut } {
            self.begin_hold(HoldInput::Button, Instant::now());
        }
    }

    fn begin_hold(&mut self, input: HoldInput, now: Instant) {
        if self.hold.is_none() && self.recording.as_ref().is_some_and(|r| !r.finishing) {
            self.hold = Some(DictationHold {
                started_at: now,
                input,
            });
            self.recording_status();
        }
    }

    fn finish_hold(&mut self, hold: DictationHold, now: Instant) {
        self.hold = None;
        if now.saturating_duration_since(hold.started_at) >= ABORT_HOLD {
            self.cancel_recording();
        } else {
            self.toggle(hold.started_at);
        }
    }

    fn tick_hold(&mut self, now: Instant) {
        let Some(hold) = self.hold else { return };
        let expired = now.saturating_duration_since(hold.started_at) >= ABORT_HOLD;
        match hold.input {
            HoldInput::Hotkey(key) if expired || !key_down(key) => self.finish_hold(hold, now),
            HoldInput::Button => unsafe {
                if GetCapture() != self.preview_shortcut {
                    self.hold = None;
                    self.recording_status();
                } else if expired {
                    let mut cursor = POINT::default();
                    let mut button = RECT::default();
                    if GetCursorPos(&mut cursor) != 0
                        && GetWindowRect(self.preview_shortcut, &mut button) != 0
                        && PtInRect(&button, cursor) != 0
                    {
                        self.finish_hold(hold, now);
                    } else {
                        ReleaseCapture();
                        self.hold = None;
                        self.recording_status();
                    }
                }
            },
            HoldInput::Hotkey(_) => {}
        }
    }

    fn toggle(&mut self, at: Instant) {
        if let Some(recording) = self.recording.as_mut() {
            if !recording.finishing {
                if recording.finish_requested(at) {
                    self.recording_status();
                } else {
                    self.cancel_recording();
                }
            }
        } else {
            self.hide();
            self.recording_id += 1;
            self.recording = Some(Recording {
                session: Session::start_from_config(self.path.clone(), self.locale),
                started_at: at,
                can_stop: false,
                listening: false,
                quiet: false,
                finishing: false,
                cleaning: false,
                cleanup_error: None,
                transcript: String::new(),
            });
            self.recording_status();
        }
    }

    fn show_launch_preview(&mut self, hotkey: Hotkey) {
        self.indicator(
            &format!(
                "{}{}{}\n{}",
                welcome_prefix(self.locale),
                self.locale.dictation_name(),
                self.locale.text("!", ""),
                match self.locale {
                    Locale::English =>
                        format!("Press {hotkey} to dictate. Press again to copy your transcript."),
                    Locale::SimplifiedChinese =>
                        format!("按 {hotkey} 开始听写，再按一次停止并复制"),
                },
            ),
            Some(PreviewBadge::Dismiss),
        );
        self.status_until = Some(Instant::now() + LAUNCH_PREVIEW_TIME);
    }

    fn activate_preview_shortcut(&mut self, action: isize) {
        if action == PreviewBadge::CopyLast as isize {
            self.copy_last_transcript(CopyOrigin::Manual);
            return;
        }
        if action == PreviewBadge::SkipCleanup as isize {
            if self
                .recording
                .as_ref()
                .is_some_and(|recording| recording.cleaning)
            {
                self.finish_recording();
            }
            return;
        }
        if action == PreviewBadge::Dismiss as isize {
            if self.recording.is_none()
                && !self.preview_shortcut.is_null()
                && unsafe {
                    IsWindowVisible(self.preview_shortcut) != 0
                        && GetWindowLongPtrW(self.preview_shortcut, GWLP_USERDATA) == action
                }
            {
                self.hide();
            }
            return;
        }
        if self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.previewing() && !recording.finishing)
        {
            if let Some(hold) = self.hold {
                if matches!(hold.input, HoldInput::Button) {
                    self.finish_hold(hold, Instant::now());
                }
            } else if action == PreviewBadge::Stop as isize {
                self.toggle(Instant::now());
            } else if action == PreviewBadge::Cancel as isize {
                self.cancel_recording();
            }
        }
    }

    fn cancel_recording(&mut self) {
        self.recording.take();
        self.hide();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.tick_hold(now);
        let mut redraw = self
            .recording
            .as_mut()
            .is_some_and(|recording| recording.update_cancel_window(now));
        while let Some(recording) = &mut self.recording {
            match recording.session.updates.try_recv() {
                Ok(Update::Live) if !recording.finishing => {
                    recording.listening = true;
                    redraw = true;
                }
                Ok(Update::QuietInput(quiet)) if !recording.finishing => {
                    recording.quiet = quiet;
                    redraw = true;
                }
                Ok(Update::Finishing) => {
                    self.hold = None;
                    recording.finishing = true;
                    redraw = true;
                }
                Ok(Update::Live | Update::QuietInput(_)) => {}
                Ok(Update::Transcript(text)) => {
                    if recording.transcript != text {
                        recording.transcript = text;
                        redraw = true;
                    }
                }
                Ok(Update::Cleaning) => {
                    self.hold = None;
                    recording.cleaning = true;
                    recording.finishing = true;
                    if !recording.transcript.trim().is_empty()
                        && self.clipboard.copy(&recording.transcript, None)
                    {
                        self.copies_pending.push_back(CopyOrigin::BeforeCleanup);
                    }
                    redraw = true;
                    break;
                }
                Ok(Update::Cleaned(text)) => {
                    recording.transcript = text;
                    self.finish_recording();
                }
                Ok(Update::CleanupFailed(error)) => {
                    recording.cleanup_error = Some(error);
                    self.finish_recording();
                }
                Ok(Update::Done) => self.finish_recording(),
                Ok(Update::Failed(error)) => self.fail(&error),
                Err(TryRecvError::Disconnected) => self.fail(
                    self.locale
                        .text("Dictation ended unexpectedly.", "听写意外中断"),
                ),
                Err(TryRecvError::Empty) => break,
            }
        }
        if redraw {
            self.recording_status();
        }
        if self
            .status_until
            .is_some_and(|until| until <= Instant::now())
        {
            self.hide();
        }
        while let Ok(feedback) = self.clipboard.feedback.try_recv() {
            if let Some(origin) = self.copies_pending.pop_front() {
                self.copy_feedback(origin, feedback);
            }
        }
        self.update_preview_progress(Instant::now());
    }

    fn update_preview_progress(&self, now: Instant) {
        let hwnd = self.preview_shortcut;
        if hwnd.is_null() || unsafe { IsWindowVisible(hwnd) } == 0 {
            return;
        }
        unsafe {
            let action = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            let Some(badge) = PreviewBadge::from_id(action) else {
                return;
            };
            let progress = if badge == PreviewBadge::Holding {
                self.hold.map(|hold| {
                    now.saturating_duration_since(hold.started_at)
                        .min(ABORT_HOLD)
                        .as_millis() as isize
                })
            } else if badge.working() {
                Some((GetWindowLongPtrW(hwnd, 0) + TICK_MS as isize) % PROGRESS_CYCLE_MS)
            } else {
                self.status_until
                    .zip(PreviewBadge::countdown(action))
                    .map(|(until, duration)| {
                        until
                            .saturating_duration_since(now)
                            .min(duration)
                            .as_millis() as isize
                    })
            };
            if let Some(progress) = progress {
                SetWindowLongPtrW(hwnd, 0, progress);
                InvalidateRect(hwnd, null(), 0);
            }
        }
    }

    fn finish_recording(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        self.hold = None;
        drop(recording.session);
        if recording.transcript.trim().is_empty() {
            self.notice_for(
                self.locale.text("No speech detected.", "未检测到语音"),
                Duration::from_secs(1),
            );
            return;
        }
        self.completed_preview = Some(PreviewBadge::Copying);
        let preview = recording.cleanup_error.map_or_else(
            || recording.transcript.clone(),
            |error| format!("{}\n{error}", recording.transcript),
        );
        self.last_transcript = Some(recording.transcript);
        self.indicator(&preview, self.completed_preview);
        for hwnd in [self.pill, self.preview_shortcut, self.copy_shortcut] {
            if !hwnd.is_null() {
                unsafe { UpdateWindow(hwnd) };
            }
        }
        self.status_until = Some(Instant::now() + COMPLETED_PREVIEW_TIME);
        self.copy_last_transcript(CopyOrigin::Final(self.recording_id));
    }

    fn copy_feedback(&mut self, origin: CopyOrigin, feedback: ClipboardFeedback) {
        if matches!(origin, CopyOrigin::BeforeCleanup) || self.recording.is_some() {
            return;
        }
        let copied = matches!(feedback, ClipboardFeedback::Copied(_));
        if let CopyOrigin::Final(id) = origin {
            if id != self.recording_id {
                return;
            }
            if self.completed_preview.is_some() {
                let badge = if copied {
                    PreviewBadge::Copied
                } else {
                    PreviewBadge::CopyFailed
                };
                self.completed_preview = Some(badge);
                self.status_until = Some(Instant::now() + COMPLETED_PREVIEW_TIME);
                let mut area = RECT::default();
                if !self.pill.is_null() && unsafe { GetWindowRect(self.pill, &mut area) } != 0 {
                    let dpi = unsafe { GetDpiForWindow(self.pill) }.max(96) as i32;
                    self.set_pill_badge(Some(badge));
                    self.show_preview_shortcut(Some(badge), area.right, area.top, dpi);
                }
                return;
            }
            if copied || self.status_until.is_some() {
                return;
            }
        } else if self.completed_preview.is_some() {
            return;
        }
        self.notice(if copied {
            self.locale.text("Transcript copied.", "已复制转写结果")
        } else {
            self.locale
                .text("Could not copy the transcript.", "无法复制转写结果")
        });
    }

    fn fail(&mut self, error: &str) {
        if let Some(recording) = self
            .recording
            .take()
            .filter(|recording| !recording.transcript.trim().is_empty())
        {
            self.last_transcript = Some(recording.transcript);
            let hint = self.locale.text(
                "Partial transcript available from the tray menu.",
                "已转写的内容可从托盘菜单复制",
            );
            self.notice(&format!("{error}\n{hint}"));
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
                self.locale.text("Settings", "设置"),
                self.recording.is_none(),
            ),
            (
                SETUP,
                self.locale.text("Dictation setup", "听写设置"),
                self.recording.is_none(),
            ),
            (
                CANCEL,
                if self
                    .recording
                    .as_ref()
                    .is_some_and(|recording| recording.cleaning)
                {
                    self.locale.text("Skip cleanup", "跳过整理")
                } else {
                    self.locale.text("Cancel recording", "取消录音")
                },
                self.recording.is_some(),
            ),
            (
                COPY,
                self.locale.text("Copy last transcript", "复制上次转写结果"),
                self.last_transcript.is_some() && self.copies_pending.is_empty(),
            ),
            (EXIT, self.locale.text("Exit", "退出"), true),
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
            SETTINGS | SETUP => {
                if !self.preview_shortcut.is_null()
                    && unsafe {
                        GetWindowLongPtrW(self.preview_shortcut, GWLP_USERDATA)
                            == PreviewBadge::Dismiss as isize
                            && IsWindowVisible(self.preview_shortcut) != 0
                    }
                {
                    self.hide();
                }
                settings::open(
                    self.hwnd,
                    &self.path,
                    &mut self.binding,
                    self.locale,
                    command == SETUP,
                );
                self.set_tooltip();
                unsafe {
                    Shell_NotifyIconW(NIM_MODIFY, &self.icon);
                }
            }
            CANCEL
                if self
                    .recording
                    .as_ref()
                    .is_some_and(|recording| recording.cleaning) =>
            {
                self.finish_recording()
            }
            CANCEL => self.cancel_recording(),
            COPY => self.copy_last_transcript(CopyOrigin::Manual),
            EXIT => unsafe {
                PostQuitMessage(0);
            },
            _ => {}
        }
    }

    fn copy_last_transcript(&mut self, origin: CopyOrigin) {
        if let Some(text) = &self.last_transcript {
            if self.clipboard.copy(text, None) {
                self.copies_pending.push_back(origin);
            } else {
                self.copy_feedback(origin, ClipboardFeedback::Failed);
            }
        }
    }

    fn sync_timer(&mut self) {
        let needed = self.recording.is_some()
            || self.status_until.is_some()
            || !self.copies_pending.is_empty();
        if needed && !self.timer {
            self.timer = unsafe { SetTimer(self.hwnd, TIMER, TICK_MS, None) != 0 };
        } else if !needed && self.timer {
            unsafe {
                KillTimer(self.hwnd, TIMER);
            }
            self.timer = false;
        }
    }

    fn set_pill_badge(&self, badge: Option<PreviewBadge>) {
        unsafe {
            SetWindowLongPtrW(
                self.pill,
                PILL_BADGE_SLOT,
                badge.map_or(0, |badge| badge as isize + 1),
            );
            InvalidateRect(self.pill, null(), 0);
        }
    }

    fn indicator(&mut self, text: &str, badge: Option<PreviewBadge>) {
        if self.pill.is_null() {
            return;
        }
        let shortcut = badge.filter(|&badge| badge != PreviewBadge::Listening);
        let preview = self.recording.is_some() || shortcut.is_some();
        let target = unsafe { GetForegroundWindow() };
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
        let width = (if preview {
            560
        } else if text.lines().any(|line| line.width() > 26) {
            480
        } else {
            224
        }) * dpi
            / 96;
        let width = width.min(work.right - work.left);
        let border_x = unsafe { GetSystemMetricsForDpi(SM_CXBORDER, dpi as u32) };
        let border_y = unsafe { GetSystemMetricsForDpi(SM_CYBORDER, dpi as u32) };
        let rendered;
        let (text, height) = if preview {
            let content_width =
                (width - 2 * (border_x + scale_preview(PREVIEW_SIDE_PADDING, dpi))).max(1);
            let (wrapped, line_height) = if badge == Some(PreviewBadge::Dismiss)
                && let Some((title, instructions)) = text.split_once('\n')
            {
                let (title, line_height) = self.fit_preview(title, content_width, dpi);
                let (instructions, _) = self.fit_preview(instructions, content_width, dpi);
                (format!("{title}\r\n{instructions}"), line_height)
            } else {
                self.fit_preview(text, content_width, dpi)
            };
            rendered = wrapped;
            let rows = rendered.lines().count().max(1) as i32;
            let height = preview_text_height(rows, line_height, dpi)
                + 2 * (border_y + scale_preview(PREVIEW_VERTICAL_PADDING, dpi));
            (rendered.as_str(), height)
        } else {
            (
                text,
                scale_preview(if text.contains('\n') { 68 } else { 36 }, dpi),
            )
        };
        let left = (work.left + work.right - width) / 2;
        let top = work.bottom - height - 24 * dpi / 96;
        self.set_pill_badge(badge);
        unsafe {
            SetWindowLongPtrW(
                self.pill,
                GWLP_USERDATA,
                if preview { dpi } else { -dpi } as isize,
            );
            SetWindowLongPtrW(
                self.pill,
                PILL_WELCOME_SLOT,
                if badge == Some(PreviewBadge::Dismiss) {
                    welcome_prefix(self.locale).encode_utf16().count() as isize
                } else {
                    0
                },
            );
        }
        set_preview_text(self.pill, &wide(text));
        place_preview_window(self.pill, left, top, width, height);
        self.show_preview_shortcut(shortcut, left + width, top, dpi);
        if !self.copy_shortcut.is_null() {
            if self.last_transcript.is_some() {
                let ready = self.completed_preview.is_some() as isize;
                unsafe {
                    if SetWindowLongPtrW(self.copy_shortcut, SHORTCUT_COPY_READY_SLOT, ready)
                        != ready
                    {
                        InvalidateRect(self.copy_shortcut, null(), 0);
                    }
                }
                self.position_preview_shortcut(
                    self.copy_shortcut,
                    PreviewBadge::CopyLast,
                    left,
                    top,
                    dpi,
                );
            } else {
                self.hide_preview_shortcut(self.copy_shortcut);
            }
        }
    }

    fn show_preview_shortcut(&self, badge: Option<PreviewBadge>, right: i32, top: i32, dpi: i32) {
        if self.preview_shortcut.is_null() {
            return;
        }
        let Some(badge) = badge else {
            self.hide_preview_shortcut(self.preview_shortcut);
            return;
        };
        self.position_preview_shortcut(self.preview_shortcut, badge, right, top, dpi);
    }

    fn position_preview_shortcut(
        &self,
        hwnd: HWND,
        badge: PreviewBadge,
        anchor: i32,
        top: i32,
        dpi: i32,
    ) {
        let copy_button = badge == PreviewBadge::CopyLast;
        let hotkey = self.binding.map(|binding| binding.hotkey);
        let label = wide(&badge.label(hotkey, self.locale));
        let stop_label = if matches!(badge, PreviewBadge::Dismiss | PreviewBadge::CopyLast) {
            None
        } else {
            Some(wide(&PreviewBadge::Stop.label(hotkey, self.locale)))
        };
        let height = scale_preview(SHORTCUT_HEIGHT, dpi);
        unsafe {
            let mut width = scale_preview(130, dpi);
            let dc = GetDC(self.pill);
            if !dc.is_null() {
                let font = create_indicator_font(dpi, 11);
                if !font.is_null() {
                    let previous = SelectObject(dc, font);
                    let mut length = SIZE::default();
                    GetTextExtentPoint32W(
                        dc,
                        label.as_ptr(),
                        (label.len() - 1) as i32,
                        &mut length,
                    );
                    width = length.cx;
                    if let Some(stop_label) = stop_label {
                        GetTextExtentPoint32W(
                            dc,
                            stop_label.as_ptr(),
                            (stop_label.len() - 1) as i32,
                            &mut length,
                        );
                        width = width.max(length.cx);
                    }
                    SelectObject(dc, previous);
                    DeleteObject(font);
                }
                ReleaseDC(self.pill, dc);
            }
            let padding = if badge == PreviewBadge::Dismiss {
                42
            } else {
                18
            };
            let width = width + scale_preview(padding, dpi);
            let visible = IsWindowVisible(hwnd) != 0;
            let badge_changed = GetWindowLongPtrW(hwnd, GWLP_USERDATA) != badge as isize;
            if badge_changed || !visible {
                if badge != PreviewBadge::Holding && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                let progress = if let Some(duration) = PreviewBadge::countdown(badge as isize) {
                    duration.as_millis() as isize
                } else if badge.working() {
                    PROGRESS_CYCLE_MS / 8
                } else {
                    0
                };
                SetWindowLongPtrW(hwnd, 0, progress);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, badge as isize);
            }
            let label_changed = set_preview_text(hwnd, &label);
            let left = if copy_button { anchor } else { anchor - width };
            let top = top - height - scale_preview(SHORTCUT_GAP, dpi);
            let holding_button = visible && badge == PreviewBadge::Holding && GetCapture() == hwnd;
            if !holding_button {
                place_preview_window(hwnd, left, top, width, height);
            }
            if badge_changed || label_changed {
                InvalidateRect(hwnd, null(), 0);
            }
        }
    }

    fn recording_status(&mut self) {
        if let Some(recording) = &self.recording {
            let badge = if self.hold.is_some() {
                Some(PreviewBadge::Holding)
            } else if recording.previewing() || recording.finishing {
                Some(recording.preview_badge())
            } else {
                (recording.listening && !recording.quiet).then_some(PreviewBadge::Listening)
            };
            self.indicator(&recording.status(self.locale), badge);
        }
    }

    fn fit_preview(&self, text: &str, width: i32, dpi: i32) -> (String, i32) {
        let dc = unsafe { GetDC(self.pill) };
        if dc.is_null() {
            return (
                transcript_preview(text, |line| line.width() as i32 * 14 * dpi / 96 <= width),
                scale_preview(18, dpi),
            );
        }
        let previous = self
            .pill_font
            .map(|(_, font)| unsafe { SelectObject(dc, font) });
        let mut units = Vec::new();
        let rendered = transcript_preview(text, |line| {
            units.clear();
            units.extend(line.encode_utf16());
            let mut size = SIZE::default();
            (unsafe {
                GetTextExtentPoint32W(dc, units.as_ptr(), units.len() as i32, &mut size) != 0
            }) && size.cx <= width
        });
        let height = line_height(dc, dpi);
        unsafe {
            if let Some(previous) = previous {
                SelectObject(dc, previous);
            }
            ReleaseDC(self.pill, dc);
        }
        (rendered, height)
    }

    fn update_indicator_font(&mut self, dpi: i32) {
        if self.pill_font.is_some_and(|(current, _)| current == dpi) {
            return;
        }
        let font = create_indicator_font(dpi, 14);
        if !font.is_null() {
            unsafe {
                SetWindowLongPtrW(self.pill, 0, font as isize);
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
        self.completed_preview = None;
        self.indicator(text, None);
        self.status_until = Some(Instant::now() + duration);
    }

    fn hide(&mut self) {
        self.hold = None;
        self.status_until = None;
        self.completed_preview = None;
        self.hide_preview_shortcut(self.preview_shortcut);
        self.hide_preview_shortcut(self.copy_shortcut);
        unsafe {
            ShowWindow(self.pill, SW_HIDE);
        }
    }

    fn hide_preview_shortcut(&self, hwnd: HWND) {
        if !hwnd.is_null() {
            unsafe {
                if GetCapture() == hwnd {
                    ReleaseCapture();
                }
                ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.recording.take();
        unsafe {
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

fn create_indicator_font(dpi: i32, size: i32) -> HFONT {
    create_indicator_font_with_weight(dpi, size, FW_NORMAL)
}

fn create_indicator_font_with_weight(dpi: i32, size: i32, weight: u32) -> HFONT {
    let mut font = LOGFONTW {
        lfHeight: -(size * dpi + 48) / 96,
        lfWeight: weight as i32,
        lfCharSet: ANSI_CHARSET,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (slot, unit) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *slot = unit;
    }
    unsafe { CreateFontIndirectW(&font) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_preview_test_owner() -> HWND {
        unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
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

    #[test]
    fn indicator_resolves_to_segoe_ui() {
        let font = create_indicator_font(96, 14);
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
    fn preview_paints_centered_rows_at_normal_and_high_dpi() {
        unsafe {
            let pill = create_pill(null_mut());
            assert!(!pill.is_null());
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            let face = GetSysColor(COLOR_BTNFACE);
            let background = if high_contrast() {
                face
            } else {
                blend_color(face, GetSysColor(COLOR_BTNTEXT), 1)
            };
            for dpi in [96, 144] {
                let font = create_indicator_font(dpi, 14);
                assert!(!font.is_null());
                let previous_font = SelectObject(dc, font);
                SetWindowLongPtrW(pill, 0, font as isize);
                SetWindowLongPtrW(pill, GWLP_USERDATA, dpi as isize);
                let line = line_height(dc, dpi);
                let padding = scale_preview(PREVIEW_VERTICAL_PADDING, dpi);
                let gap = scale_preview(PREVIEW_LINE_GAP, dpi);
                let width = scale_preview(300, dpi);
                for rows in 1..=3 {
                    let height = preview_text_height(rows, line, dpi) + 2 * padding;
                    let bitmap = CreateCompatibleBitmap(screen, width, height);
                    assert!(!bitmap.is_null());
                    let previous_bitmap = SelectObject(dc, bitmap);
                    let sample = vec!["Agjp & Agjp"; rows as usize].join("\r\n");
                    SetWindowTextW(pill, wide(&sample).as_ptr());
                    let mut area = RECT {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    };
                    AdjustWindowRect(&mut area, WS_POPUP | WS_BORDER | WS_CLIPCHILDREN, 0);
                    SetWindowPos(
                        pill,
                        null_mut(),
                        0,
                        0,
                        area.right - area.left,
                        area.bottom - area.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    SendMessageW(pill, WM_PRINTCLIENT, dc as WPARAM, 0);
                    let ink = |y| {
                        ((width / 4)..(3 * width / 4)).any(|x| GetPixel(dc, x, y) != background)
                    };
                    assert!(!(0..padding).any(ink));
                    assert!(!(height - padding..height).any(ink));
                    for row in 0..rows {
                        let top = padding + row * (line + gap);
                        assert!((top..top + line).any(ink));
                        if row + 1 < rows {
                            assert!(!(top + line..top + line + gap).any(ink));
                        }
                    }
                    SetWindowLongPtrW(pill, PILL_BADGE_SLOT, PreviewBadge::Dismiss as isize + 1);
                    SendMessageW(pill, WM_PRINTCLIENT, dc as WPARAM, 0);
                    let welcome = if high_contrast() {
                        face
                    } else {
                        rgb(255, 255, 255)
                    };
                    assert_eq!(GetPixel(dc, width / 2, 0), welcome);
                    SetWindowLongPtrW(pill, PILL_BADGE_SLOT, 0);
                    SelectObject(dc, previous_bitmap);
                    DeleteObject(bitmap);
                }
                SetWindowLongPtrW(pill, 0, 0);
                SelectObject(dc, previous_font);
                DeleteObject(font);
            }
            DeleteDC(dc);
            ReleaseDC(null_mut(), screen);
            DestroyWindow(pill);
        }
    }

    #[test]
    fn preview_shortcut_paints_at_the_right_edge_and_accepts_clicks() {
        unsafe {
            let parent = create_preview_test_owner();
            let shortcut = create_preview_shortcut(parent);
            assert!(!shortcut.is_null());
            assert_eq!(GetWindow(shortcut, GW_OWNER), parent);
            assert_eq!(GetWindowLongW(shortcut, GWL_STYLE) as u32 & WS_CHILD, 0);
            let width = 320;
            SetWindowTextW(shortcut, wide("STOP & COPY · CTRL+SHIFT+/").as_ptr());
            SetWindowPos(
                shortcut,
                null_mut(),
                0,
                0,
                width,
                SHORTCUT_HEIGHT,
                SWP_NOZORDER,
            );
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, SHORTCUT_HEIGHT);
            let previous = SelectObject(dc, bitmap);
            SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
            let face = GetSysColor(COLOR_BTNFACE);
            let background = if high_contrast() {
                face
            } else {
                blend_color(face, GetSysColor(COLOR_BTNTEXT), 1)
            };
            let left = GetPixel(dc, 10, 8);
            let right_has_text = (0..SHORTCUT_HEIGHT)
                .any(|y| ((width - 120)..(width - 9)).any(|x| GetPixel(dc, x, y) != background));
            for badge in [PreviewBadge::Transcribing, PreviewBadge::Copying] {
                SetWindowLongPtrW(shortcut, GWLP_USERDATA, badge as isize);
                SetWindowTextW(shortcut, wide(&badge.label(None, Locale::English)).as_ptr());
                SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                let background = GetPixel(dc, 0, 0);
                let mut ink = (0..width)
                    .filter(|&x| (0..SHORTCUT_HEIGHT).any(|y| GetPixel(dc, x, y) != background));
                let first = ink.next().unwrap();
                let last = ink.last().unwrap();
                assert!((first + last - width).abs() <= 4);
            }
            SetWindowLongPtrW(shortcut, GWLP_USERDATA, PreviewBadge::CopyLast as isize);
            SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
            let copy_accent = if high_contrast() {
                GetSysColor(COLOR_HIGHLIGHT)
            } else {
                PreviewBadge::CopyLast.accent().unwrap()
            };
            let y = SHORTCUT_HEIGHT / 2;
            for dpi in [96, 144] {
                SetWindowLongPtrW(parent, GWLP_USERDATA, dpi as isize);
                SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                let inset = GetSystemMetricsForDpi(SM_CXBORDER, dpi as u32);
                let edge = inset + scale_preview(PREVIEW_ACCENT_WIDTH, dpi);
                assert!((inset..edge).all(|x| GetPixel(dc, x, y) == copy_accent));
                assert_ne!(GetPixel(dc, edge, y), copy_accent);
            }
            SetWindowLongPtrW(shortcut, SHORTCUT_COPY_READY_SLOT, 1);
            SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
            let ready_accent = GetPixel(dc, GetSystemMetricsForDpi(SM_CXBORDER, 144), y);
            if high_contrast() {
                assert_eq!(ready_accent, copy_accent);
            } else {
                assert_ne!(ready_accent, copy_accent);
                assert_ne!(Some(ready_accent), PreviewBadge::Copied.accent());
                assert_eq!(GetPixel(dc, 20, y), blend_color(face, ready_accent, 2));
            }
            SetWindowLongPtrW(shortcut, SHORTCUT_COPY_READY_SLOT, 0);
            for badge in [
                PreviewBadge::Transcribing,
                PreviewBadge::SkipCleanup,
                PreviewBadge::Copying,
                PreviewBadge::Holding,
            ] {
                SetWindowLongPtrW(shortcut, GWLP_USERDATA, badge as isize);
                let accent = if high_contrast() {
                    GetSysColor(COLOR_HIGHLIGHT)
                } else {
                    badge.accent().unwrap()
                };
                let y = SHORTCUT_HEIGHT - 2;
                let halfway = ABORT_HOLD.as_millis() as isize / 2;
                let (early, late) = if badge.working() {
                    (PROGRESS_CYCLE_MS / 4, PROGRESS_CYCLE_MS * 3 / 4)
                } else {
                    (halfway, halfway * 2)
                };
                SetWindowLongPtrW(shortcut, 0, early);
                SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                assert_eq!(GetPixel(dc, width / 4, y), accent);
                assert_ne!(GetPixel(dc, width * 3 / 4, y), accent);
                SetWindowLongPtrW(shortcut, 0, late);
                SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                assert_eq!(GetPixel(dc, width * 3 / 4, y), accent);
            }
            SetWindowLongPtrW(shortcut, GWLP_USERDATA, PreviewBadge::Cancel as isize);
            assert_eq!(
                SendMessageW(shortcut, WM_MOUSEACTIVATE, 0, 0),
                MA_NOACTIVATE as LRESULT
            );
            let mut message = MSG::default();
            SendMessageW(shortcut, WM_LBUTTONDOWN, 0, 0);
            SendMessageW(shortcut, WM_LBUTTONUP, 0, width as LPARAM);
            assert_eq!(
                PeekMessageW(
                    &mut message,
                    parent,
                    PREVIEW_ACTION,
                    PREVIEW_ACTION,
                    PM_REMOVE
                ),
                0
            );
            for action in [
                PreviewBadge::Cancel,
                PreviewBadge::Stop,
                PreviewBadge::Dismiss,
                PreviewBadge::SkipCleanup,
                PreviewBadge::CopyLast,
            ] {
                SetWindowLongPtrW(shortcut, GWLP_USERDATA, action as isize);
                SendMessageW(shortcut, WM_LBUTTONDOWN, 0, 0);
                SendMessageW(shortcut, WM_LBUTTONUP, 0, 10);
                assert_ne!(
                    PeekMessageW(
                        &mut message,
                        parent,
                        PREVIEW_ACTION,
                        PREVIEW_ACTION,
                        PM_REMOVE
                    ),
                    0
                );
                assert_eq!(message.wParam, action as WPARAM);
                assert_eq!(message.lParam, shortcut as LPARAM);
            }
            for (badge, duration) in [
                (PreviewBadge::Dismiss, LAUNCH_PREVIEW_TIME),
                (PreviewBadge::Copied, COMPLETED_PREVIEW_TIME),
            ] {
                SetWindowLongPtrW(shortcut, GWLP_USERDATA, badge as isize);
                let accent = if high_contrast() {
                    GetSysColor(COLOR_HIGHLIGHT)
                } else {
                    badge
                        .accent()
                        .unwrap_or_else(|| GetSysColor(COLOR_HIGHLIGHT))
                };
                let background = if high_contrast() {
                    GetSysColor(COLOR_BTNFACE)
                } else {
                    blend_color(GetSysColor(COLOR_BTNFACE), accent, 3)
                };
                let total_ms = duration.as_millis() as isize;
                let y = SHORTCUT_HEIGHT - 2;
                for (remaining_ms, expected_right) in [
                    (total_ms, accent),
                    (total_ms / 2, background),
                    (0, background),
                ] {
                    SetWindowLongPtrW(shortcut, 0, remaining_ms);
                    SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                    assert_eq!(GetPixel(dc, width * 3 / 4, y), expected_right);
                    assert_eq!(
                        GetPixel(dc, width / 4, y),
                        if remaining_ms > 0 { accent } else { background }
                    );
                }
            }
            SendMessageW(shortcut, WM_LBUTTONDOWN, 0, 0);
            SendMessageW(shortcut, WM_LBUTTONUP, 0, 10);
            assert_eq!(
                PeekMessageW(
                    &mut message,
                    parent,
                    PREVIEW_ACTION,
                    PREVIEW_ACTION,
                    PM_REMOVE
                ),
                0
            );
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
            ReleaseDC(null_mut(), screen);
            assert_eq!(left, background);
            assert!(right_has_text);

            let pill = create_pill(parent);
            assert!(!pill.is_null());
            let (clipboard, _writes, feedback) = Clipboard::test_sink();
            let hotkey = "Ctrl+Shift+F2".parse().unwrap();
            let mut app = App {
                locale: Locale::English,
                hwnd: parent,
                pill,
                preview_shortcut: shortcut,
                copy_shortcut: create_preview_shortcut(pill),
                pill_font: None,
                icon: NOTIFYICONDATAW::default(),
                path: PathBuf::new(),
                binding: Some(Binding { id: 1, hotkey }),
                recording: None,
                hold: None,
                last_transcript: None,
                recording_id: 0,
                completed_preview: None,
                status_until: None,
                clipboard,
                copies_pending: VecDeque::new(),
                timer: false,
            };
            let launch = Instant::now();
            app.show_launch_preview(hotkey);
            assert!(app.status_until.unwrap() >= launch + LAUNCH_PREVIEW_TIME);
            assert!(app.status_until.unwrap() <= Instant::now() + LAUNCH_PREVIEW_TIME);
            assert_eq!(
                GetWindowLongPtrW(shortcut, GWLP_USERDATA),
                PreviewBadge::Dismiss as isize
            );
            assert_ne!(GetWindowLongPtrW(pill, PILL_WELCOME_SLOT), 0);
            let mut title = [0; 256];
            let length = GetWindowTextW(pill, title.as_mut_ptr(), title.len() as i32);
            assert_eq!(
                String::from_utf16_lossy(&title[..length as usize]),
                "Welcome to DFlow Dictation!\r\nPress Ctrl+Shift+F2 to dictate. Press again to copy your transcript."
            );
            assert_eq!(
                GetWindowLongPtrW(shortcut, 0),
                LAUNCH_PREVIEW_TIME.as_millis() as isize
            );
            app.status_until = Some(Instant::now() + Duration::from_secs(2));
            app.tick();
            assert!((1..=2000).contains(&GetWindowLongPtrW(shortcut, 0)));
            app.activate_preview_shortcut(PreviewBadge::Dismiss as isize);
            assert!(app.status_until.is_none());
            assert_eq!(IsWindowVisible(pill), 0);
            assert_eq!(IsWindowVisible(shortcut), 0);
            app.show_launch_preview(hotkey);
            app.status_until = Some(Instant::now());
            app.tick();
            assert_eq!(IsWindowVisible(pill), 0);
            assert_eq!(IsWindowVisible(shortcut), 0);

            assert_eq!(IsWindowVisible(app.copy_shortcut), 0);
            app.last_transcript = Some("Previous transcript.".into());
            let (session, updates) = Session::test_channel();
            app.recording = Some(Recording {
                session,
                started_at: Instant::now(),
                can_stop: false,
                listening: false,
                quiet: false,
                finishing: false,
                cleaning: false,
                cleanup_error: None,
                transcript: String::new(),
            });
            let bounds = |hwnd| {
                let mut area = RECT::default();
                assert_ne!(GetWindowRect(hwnd, &mut area), 0);
                (area.left, area.top, area.right, area.bottom)
            };
            app.recording_status();
            let initial_body = bounds(pill);
            let initial_copy = bounds(app.copy_shortcut);
            let recording = app.recording.as_mut().unwrap();
            recording.listening = true;
            app.recording_status();
            assert_eq!(bounds(pill), initial_body);
            for transcript in ["First words.", "The current transcript."] {
                app.recording.as_mut().unwrap().transcript = transcript.into();
                app.recording_status();
                assert_eq!(bounds(pill), initial_body);
                assert_eq!(bounds(app.copy_shortcut), initial_copy);
            }
            let (mut copy, mut stop) = (RECT::default(), RECT::default());
            assert_ne!(GetWindowRect(app.copy_shortcut, &mut copy), 0);
            assert_ne!(GetWindowRect(shortcut, &mut stop), 0);
            let dpi = GetWindowLongPtrW(pill, GWLP_USERDATA) as i32;
            let (preview_left, preview_top, preview_right, _) = initial_body;
            assert_eq!((copy.left, stop.right), (preview_left, preview_right));
            assert_eq!((copy.top, copy.bottom), (stop.top, stop.bottom));
            assert_eq!(preview_top - copy.bottom, scale_preview(SHORTCUT_GAP, dpi));
            assert!(copy.right < stop.left);
            assert_eq!(IsWindowVisible(app.copy_shortcut), 1);
            for update in [Update::Finishing, Update::Cleaning] {
                updates.send(update).unwrap();
                app.tick();
                assert_eq!(
                    GetWindowLongPtrW(app.copy_shortcut, SHORTCUT_COPY_READY_SLOT),
                    0
                );
            }
            updates
                .send(Update::Cleaned("Cleaned transcript.".into()))
                .unwrap();
            app.tick();
            assert_eq!(app.completed_preview, Some(PreviewBadge::Copying));
            assert_eq!(
                GetWindowLongPtrW(app.copy_shortcut, SHORTCUT_COPY_READY_SLOT),
                1
            );
            assert_eq!(
                GetWindowLongPtrW(app.copy_shortcut, GWLP_USERDATA),
                PreviewBadge::CopyLast as isize
            );
            for _ in 0..2 {
                feedback.send(ClipboardFeedback::Copied(1)).unwrap();
            }
            app.tick();
            assert_eq!(app.completed_preview, Some(PreviewBadge::Copied));
            let length = GetWindowTextW(shortcut, title.as_mut_ptr(), title.len() as i32);
            assert_eq!(
                String::from_utf16_lossy(&title[..length as usize]),
                "COPIED"
            );
            assert_eq!(
                GetWindowLongPtrW(app.copy_shortcut, SHORTCUT_COPY_READY_SLOT),
                1
            );
            let previous_id = app.recording_id;
            app.path = tempfile::tempdir()
                .unwrap()
                .path()
                .join("config/dflow.toml");
            app.dictation_key_pressed();
            assert!(app.recording.is_some());
            assert_eq!(app.recording_id, previous_id + 1);
            assert!(app.completed_preview.is_none());
            assert!(app.status_until.is_none());
            app.cancel_recording();
            app.hide();
            assert_eq!(IsWindowVisible(app.copy_shortcut), 0);
            app.indicator("Listening…", Some(PreviewBadge::Listening));
            assert_eq!(
                GetWindowLongPtrW(app.copy_shortcut, SHORTCUT_COPY_READY_SLOT),
                0
            );
            app.hide();

            let mut area = RECT::default();
            app.show_preview_shortcut(Some(PreviewBadge::Stop), 560, 100, 96);
            assert_ne!(GetWindowRect(shortcut, &mut area), 0);
            for badge in [
                PreviewBadge::Transcribing,
                PreviewBadge::Copying,
                PreviewBadge::Copied,
            ] {
                app.show_preview_shortcut(Some(badge), 560, 100, 96);
                let mut updated = RECT::default();
                assert_ne!(GetWindowRect(shortcut, &mut updated), 0);
                assert_eq!(
                    (area.left, area.top, area.right, area.bottom),
                    (updated.left, updated.top, updated.right, updated.bottom),
                );
            }
            for badge in [PreviewBadge::Transcribing, PreviewBadge::Copying] {
                app.indicator("A short transcript.", Some(badge));
                assert_eq!(GetWindowLongPtrW(shortcut, GWLP_USERDATA), badge as isize);
                assert_eq!(GetWindowLongPtrW(pill, PILL_WELCOME_SLOT), 0);
                let long_text = "This longer transcript should wrap in the preview. ".repeat(20);
                app.indicator(&long_text, Some(badge));
                assert_eq!(GetWindowLongPtrW(shortcut, GWLP_USERDATA), badge as isize);
            }

            app.locale = Locale::SimplifiedChinese;
            app.show_launch_preview(hotkey);
            let length = GetWindowTextW(pill, title.as_mut_ptr(), title.len() as i32);
            assert_eq!(
                String::from_utf16_lossy(&title[..length as usize]),
                "欢迎使用 DFlow 听写\r\n按 Ctrl+Shift+F2 开始听写，再按一次停止并复制"
            );
            assert_eq!(
                GetWindowLongPtrW(pill, PILL_WELCOME_SLOT),
                "欢迎使用 ".encode_utf16().count() as isize
            );
            let mut widths = Vec::new();
            for dpi in [96, 144] {
                app.show_preview_shortcut(Some(PreviewBadge::Stop), 560, 100, dpi);
                let length = GetWindowTextW(shortcut, title.as_mut_ptr(), title.len() as i32);
                assert_eq!(
                    String::from_utf16_lossy(&title[..length as usize]),
                    "停止并复制 · CTRL+SHIFT+F2"
                );
                assert_ne!(GetWindowRect(shortcut, &mut area), 0);
                widths.push(area.right - area.left);
            }
            assert!(widths[0] > 0 && widths[1] > widths[0]);
        }
    }

    #[test]
    fn preview_rolls_forward_by_wrapped_lines() {
        let preview = |text| transcript_preview(text, |line| line.width() <= 8);
        assert_eq!(preview("one two three"), "one two\r\nthree");
        assert_eq!(
            preview("one two three four five six seven"),
            "four\r\nfive six\r\nseven"
        );
        assert_eq!(preview("界界界界界界界界界"), "界界界界\r\n界界界界\r\n界");
    }

    #[test]
    fn dictation_aborts_early_and_copies_the_final_revision_once() {
        let (session, send) = Session::test_channel();
        let now = Instant::now();
        let mut recording = Recording {
            session,
            started_at: now,
            can_stop: false,
            listening: true,
            quiet: false,
            finishing: false,
            cleaning: false,
            cleanup_error: None,
            transcript: String::new(),
        };
        assert_eq!(
            recording.status(Locale::English),
            "Listening… · Stop now to discard"
        );
        recording.transcript = "Meet Wednesday.".into();
        assert!(recording.previewing());
        assert_eq!(recording.status(Locale::English), "Meet Wednesday.");
        assert_eq!(
            recording
                .preview_badge()
                .label(Some(Hotkey::default()), Locale::English),
            "CANCEL · CTRL+SHIFT+/"
        );
        assert!(!recording.finish_requested(now + CANCEL_WINDOW - Duration::from_millis(1)));
        assert!(!recording.finishing);
        recording.transcript.clear();
        assert!(recording.update_cancel_window(now + CANCEL_WINDOW));
        assert_eq!(
            recording.status(Locale::English),
            "Listening… · Waiting for transcript"
        );
        recording.transcript = "Meet Wednesday.".into();
        assert_eq!(recording.status(Locale::English), "Meet Wednesday.");
        assert_eq!(
            recording
                .preview_badge()
                .label(Some(Hotkey::default()), Locale::English),
            "STOP & COPY · CTRL+SHIFT+/"
        );
        recording.started_at = now - CANCEL_WINDOW;
        let (clipboard, writes, feedback) = Clipboard::test_sink();
        let mut app = App {
            locale: Locale::English,
            hwnd: null_mut(),
            pill: null_mut(),
            preview_shortcut: null_mut(),
            copy_shortcut: null_mut(),
            pill_font: None,
            icon: NOTIFYICONDATAW::default(),
            path: PathBuf::new(),
            binding: None,
            recording: Some(recording),
            hold: None,
            last_transcript: Some("Prior transcript.".into()),
            recording_id: 1,
            completed_preview: None,
            status_until: None,
            clipboard,
            copies_pending: VecDeque::new(),
            timer: false,
        };
        app.activate_preview_shortcut(PreviewBadge::CopyLast as isize);
        assert_eq!(writes.try_recv().unwrap().text, "Prior transcript.");
        feedback.send(ClipboardFeedback::Copied(17)).unwrap();

        let press = Instant::now();
        app.begin_hold(HoldInput::Button, press);
        assert!(!app.recording.as_ref().unwrap().finishing);
        app.activate_preview_shortcut(PreviewBadge::Holding as isize);
        assert_eq!(
            app.recording.as_ref().unwrap().preview_badge(),
            PreviewBadge::Transcribing
        );
        send.send(Update::Transcript("Meet Tuesday.".into()))
            .unwrap();
        app.tick();
        assert_eq!(app.recording.as_ref().unwrap().transcript, "Meet Tuesday.");
        assert!(app.status_until.is_none());
        assert!(matches!(writes.try_recv(), Err(TryRecvError::Empty)));

        let final_transcript = "Meet Tuesday.\nBring the notes.";
        send.send(Update::Transcript(final_transcript.into()))
            .unwrap();
        send.send(Update::Cleaning).unwrap();
        send.send(Update::Cleaned("Late cleanup output.".into()))
            .unwrap();
        app.tick();
        assert_eq!(
            app.recording
                .as_ref()
                .unwrap()
                .preview_badge()
                .label(Some(Hotkey::default()), Locale::English),
            "Skip cleanup · CTRL+SHIFT+/"
        );
        assert_eq!(writes.try_recv().unwrap().text, final_transcript);
        let before_final = Instant::now();
        app.dictation_key_pressed();
        assert_eq!(writes.try_recv().unwrap().text, final_transcript);
        assert_eq!(app.last_transcript.as_deref(), Some(final_transcript));
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copying));
        let deadline = app.status_until.unwrap();
        assert!(deadline >= before_final + COMPLETED_PREVIEW_TIME);
        assert!(deadline <= Instant::now() + COMPLETED_PREVIEW_TIME);
        app.copy_feedback(CopyOrigin::Final(0), ClipboardFeedback::Failed);
        feedback.send(ClipboardFeedback::Copied(29)).unwrap();
        app.tick();
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copying));
        feedback.send(ClipboardFeedback::Copied(29)).unwrap();
        app.tick();
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copied));
        assert!(app.status_until.unwrap() >= deadline);
        assert!(matches!(writes.try_recv(), Err(TryRecvError::Empty)));
        app.copy_feedback(CopyOrigin::Final(1), ClipboardFeedback::Failed);
        assert_eq!(app.completed_preview, Some(PreviewBadge::CopyFailed));
        assert!(app.status_until.unwrap() >= deadline);
        app.status_until = Some(Instant::now());
        app.tick();
        assert!(app.completed_preview.is_none());
        app.copy_feedback(CopyOrigin::Final(1), ClipboardFeedback::Copied(29));
        assert!(app.status_until.is_none());

        let (session, _send) = Session::test_channel();
        app.recording = Some(Recording {
            session,
            started_at: now,
            can_stop: true,
            listening: true,
            quiet: false,
            finishing: false,
            cleaning: false,
            cleanup_error: None,
            transcript: "This recording will be discarded.".into(),
        });
        app.begin_hold(HoldInput::Hotkey(Hotkey::default().key), now);
        app.tick_hold(now + ABORT_HOLD);
        assert!(app.hold.is_none());
        assert!(app.recording.is_none());
        assert_eq!(app.last_transcript.as_deref(), Some(final_transcript));
        assert!(matches!(writes.try_recv(), Err(TryRecvError::Empty)));
    }
}
