use crate::settings;
use dflow_scratchpad::{
    audio::{Session, Update},
    clipboard::{Clipboard, ClipboardFeedback},
    config::{self, Config},
    hotkey::Hotkey,
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
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const CANCEL_WINDOW: Duration = Duration::from_secs(2);
const COMPLETED_PREVIEW_TIME: Duration = Duration::from_secs(3);
const LAUNCH_PREVIEW_TIME: Duration = Duration::from_secs(7);
const PREVIEW_LINES: usize = 3;
const PREVIEW_SIDE_PADDING: i32 = 12;
const PREVIEW_VERTICAL_PADDING: i32 = 10;
const PREVIEW_LINE_GAP: i32 = 3;
const SHORTCUT_HEIGHT: i32 = 18;
const PILL_WELCOME_SLOT: i32 = size_of::<isize>() as i32;
const TIMER: usize = 1;
const SETTINGS: u32 = 1;
const CANCEL: u32 = 2;
const COPY: u32 = 3;
const EXIT: u32 = 4;

thread_local! {
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

struct Recording {
    session: Session,
    started_at: Instant,
    can_stop: bool,
    listening: bool,
    quiet: bool,
    finishing: bool,
    transcript: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
enum PreviewBadge {
    Cancel = 0,
    Stop = 1,
    Dismiss,
    Transcribing,
    Copying,
    Copied,
    CopyFailed,
    StopPending,
}

impl PreviewBadge {
    fn countdown(action: isize) -> Option<Duration> {
        match action {
            value if value == Self::Dismiss as isize => Some(LAUNCH_PREVIEW_TIME),
            value if value == Self::Copied as isize => Some(COMPLETED_PREVIEW_TIME),
            _ => None,
        }
    }

    fn label(self, hotkey: Option<Hotkey>) -> String {
        let label = match self {
            Self::Cancel => "CANCEL",
            Self::Stop | Self::StopPending => "STOP & COPY",
            Self::Dismiss => "DISMISS",
            Self::Transcribing | Self::Copying => "Finishing…",
            Self::Copied => "Copied",
            Self::CopyFailed => "Copy failed",
        };
        match hotkey {
            Some(hotkey)
                if matches!(
                    self,
                    Self::Cancel | Self::Stop | Self::StopPending | Self::Copied
                ) =>
            {
                format!("{label} · {}", hotkey.to_string().to_ascii_uppercase())
            }
            _ => label.into(),
        }
    }

    fn for_preview_rows(self, rows: i32) -> Self {
        if rows == 1 && matches!(self, Self::Transcribing | Self::Copying) {
            Self::StopPending
        } else {
            self
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
        self.update_cancel_window(now);
        if !self.can_stop {
            return false;
        }
        self.finishing = true;
        self.session.stop();
        true
    }

    fn previewing(&self) -> bool {
        !self.transcript.trim().is_empty()
    }

    fn preview_badge(&self) -> PreviewBadge {
        if self.finishing {
            PreviewBadge::Transcribing
        } else if self.can_stop {
            PreviewBadge::Stop
        } else {
            PreviewBadge::Cancel
        }
    }

    fn status(&self) -> String {
        if self.previewing() {
            return self.transcript.clone();
        }
        let activity = if self.finishing {
            "Transcribing…"
        } else if self.quiet {
            "Listening… microphone is quiet"
        } else if self.listening {
            "Listening…"
        } else {
            "Starting microphone…"
        };
        if !self.can_stop {
            format!("{activity} · Stop now to discard")
        } else if !self.finishing && self.listening && !self.quiet {
            "Listening… · Waiting for transcript".into()
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
    hwnd: HWND,
    pill: HWND,
    preview_shortcut: HWND,
    pill_font: Option<(i32, HFONT)>,
    icon: NOTIFYICONDATAW,
    path: PathBuf,
    binding: Option<Binding>,
    recording: Option<Recording>,
    last_transcript: Option<String>,
    recording_id: u64,
    completed_preview: Option<PreviewBadge>,
    status_until: Option<Instant>,
    clipboard: Clipboard,
    copies_pending: VecDeque<Option<u64>>,
    timer: bool,
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
            wide("DFlow Dictation").as_ptr(),
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
            wide("DFlow Dictation").as_ptr(),
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
        hwnd,
        pill,
        preview_shortcut: create_preview_shortcut(pill),
        pill_font: None,
        path,
        binding: None,
        recording: None,
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
    } else if let Some(binding) = app.binding {
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
                    app.toggle()
                }
                PREVIEW_ACTION if msg.lParam as HWND == app.preview_shortcut => {
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
        cbWndExtra: PILL_WELCOME_SLOT + size_of::<isize>() as i32,
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

fn draw_pill(hwnd: HWND, dc: HDC) {
    unsafe {
        let mut area = RECT::default();
        GetClientRect(hwnd, &mut area);
        let layout_dpi = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as i32;
        let preview = layout_dpi > 0;
        let dpi = layout_dpi.abs().max(96);
        let face = GetSysColor(COLOR_BTNFACE);
        let ink = GetSysColor(COLOR_BTNTEXT);
        let background = if preview {
            blend_color(face, ink, 1)
        } else {
            face
        };
        SetDCBrushColor(dc, background);
        FillRect(dc, &area, GetStockObject(DC_BRUSH));
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
            let welcome = GetWindowLongPtrW(hwnd, PILL_WELCOME_SLOT) != 0;
            for (index, row) in rows.enumerate() {
                let row = row.strip_suffix(&[b'\r' as u16]).unwrap_or(row);
                area.bottom = area.top + height;
                if !(welcome && index == 0 && draw_welcome_title(dc, row, area, dpi)) {
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

fn draw_welcome_title(dc: HDC, row: &[u16], area: RECT, dpi: i32) -> bool {
    let prefix = "Welcome to ".encode_utf16().count();
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
        cbWndExtra: size_of::<isize>() as i32,
        hInstance: instance(),
        lpszClassName: class.as_ptr(),
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_HAND) },
        ..Default::default()
    };
    unsafe {
        if RegisterClassW(&window_class) == 0 {
            return null_mut();
        }
        let shortcut = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED,
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
        );
        if !shortcut.is_null() {
            SetLayeredWindowAttributes(shortcut, 0, 230, LWA_ALPHA);
        }
        shortcut
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
            WM_SETCURSOR
                if GetWindowLongPtrW(hwnd, GWLP_USERDATA) > PreviewBadge::Dismiss as isize =>
            {
                SetCursor(LoadCursorW(null_mut(), IDC_ARROW));
                1
            }
            WM_LBUTTONDOWN
                if GetWindowLongPtrW(hwnd, GWLP_USERDATA) <= PreviewBadge::Dismiss as isize =>
            {
                SetCapture(hwnd);
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
                if action <= PreviewBadge::Dismiss as isize
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
        let ink = GetSysColor(COLOR_BTNTEXT);
        let action = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        let face = GetSysColor(COLOR_BTNFACE);
        SetDCBrushColor(dc, blend_color(face, ink, 1));
        FillRect(dc, &area, GetStockObject(DC_BRUSH));
        if let Some(duration) = PreviewBadge::countdown(action) {
            let remaining_ms = GetWindowLongPtrW(hwnd, 0) as i64;
            let duration_ms = duration.as_millis() as i64;
            let filled = (i64::from(area.right - area.left) * remaining_ms / duration_ms) as i32;
            let fill = RECT {
                right: area.left + filled,
                ..area
            };
            SetDCBrushColor(dc, blend_color(face, GetSysColor(COLOR_HIGHLIGHT), 3));
            FillRect(dc, &fill, GetStockObject(DC_BRUSH));
        }
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, ink);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let alignment = if action == PreviewBadge::Dismiss as isize
            || action == PreviewBadge::Copied as isize
        {
            DT_CENTER
        } else {
            area.right -= 9 * dpi / 96;
            DT_RIGHT
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
            format!("DFlow Dictation · {} to start or finish", binding.hotkey)
        } else {
            "DFlow Dictation · Shortcut unavailable · Open Settings".into()
        };
        self.icon.szTip.fill(0);
        for (slot, unit) in self.icon.szTip.iter_mut().zip(label.encode_utf16()) {
            *slot = unit;
        }
    }

    fn toggle(&mut self) {
        if let Some(recording) = self.recording.as_mut() {
            if !recording.finishing {
                if recording.finish_requested(Instant::now()) {
                    self.recording_status();
                } else {
                    self.cancel_recording();
                }
            }
        } else {
            self.hide();
            self.recording_id += 1;
            let config = Config::load(self.path.clone());
            if let Some(error) = config.error {
                self.notice(&error);
                return;
            }
            self.recording = Some(Recording {
                session: Session::start(config.settings),
                started_at: Instant::now(),
                can_stop: false,
                listening: false,
                quiet: false,
                finishing: false,
                transcript: String::new(),
            });
            self.recording_status();
        }
    }

    fn show_launch_preview(&mut self, hotkey: Hotkey) {
        self.indicator(
            &format!("Welcome to DFlow Dictation!\nPress {hotkey} to dictate. Press again to copy your transcript."),
            Some(PreviewBadge::Dismiss),
        );
        self.status_until = Some(Instant::now() + LAUNCH_PREVIEW_TIME);
    }

    fn activate_preview_shortcut(&mut self, action: isize) {
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
            if action == PreviewBadge::Stop as isize {
                self.toggle();
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
        let mut redraw = self
            .recording
            .as_mut()
            .is_some_and(|recording| recording.update_cancel_window(Instant::now()));
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
                    recording.finishing = true;
                    redraw = true;
                }
                Ok(Update::Live | Update::QuietInput(_)) => {}
                Ok(Update::Transcript(text)) => {
                    recording.transcript = text;
                    redraw = true;
                }
                Ok(Update::Done) => {
                    let recording = self.recording.take().unwrap();
                    if recording.transcript.trim().is_empty() {
                        self.notice_for("No speech detected.", Duration::from_secs(1));
                    } else {
                        self.completed_preview = Some(PreviewBadge::Copying);
                        self.indicator(&recording.transcript, self.completed_preview);
                        if !self.pill.is_null() {
                            unsafe {
                                RedrawWindow(
                                    self.pill,
                                    null(),
                                    null_mut(),
                                    RDW_INVALIDATE | RDW_UPDATENOW,
                                );
                            }
                        }
                        self.status_until = Some(Instant::now() + COMPLETED_PREVIEW_TIME);
                        self.last_transcript = Some(recording.transcript);
                        self.copy_last_transcript(Some(self.recording_id));
                    }
                }
                Ok(Update::Failed(error)) => self.fail(&error),
                Err(TryRecvError::Disconnected) => self.fail("Dictation ended unexpectedly."),
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
        if let Some(until) = self.status_until
            && !self.preview_shortcut.is_null()
        {
            unsafe {
                let action = GetWindowLongPtrW(self.preview_shortcut, GWLP_USERDATA);
                if IsWindowVisible(self.preview_shortcut) != 0
                    && PreviewBadge::countdown(action).is_some()
                {
                    let remaining = until.saturating_duration_since(Instant::now());
                    SetWindowLongPtrW(self.preview_shortcut, 0, remaining.as_millis() as isize);
                    InvalidateRect(self.preview_shortcut, null(), 0);
                }
            }
        }
    }

    fn copy_feedback(&mut self, origin: Option<u64>, feedback: ClipboardFeedback) {
        if self.recording.is_some() {
            return;
        }
        let copied = matches!(feedback, ClipboardFeedback::Copied(_));
        if let Some(id) = origin {
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
                let mut area = RECT::default();
                if !self.pill.is_null() && unsafe { GetWindowRect(self.pill, &mut area) } != 0 {
                    let dpi = unsafe { GetDpiForWindow(self.pill) }.max(96) as i32;
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
            "Transcript copied."
        } else {
            "Could not copy the transcript."
        });
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
            (SETTINGS, "Settings", self.recording.is_none()),
            (CANCEL, "Cancel recording", self.recording.is_some()),
            (
                COPY,
                "Copy last transcript",
                self.last_transcript.is_some() && self.copies_pending.is_empty(),
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
                if !self.preview_shortcut.is_null()
                    && unsafe {
                        GetWindowLongPtrW(self.preview_shortcut, GWLP_USERDATA)
                            == PreviewBadge::Dismiss as isize
                            && IsWindowVisible(self.preview_shortcut) != 0
                    }
                {
                    self.hide();
                }
                settings::open(self.hwnd, &self.path, &mut self.binding);
                self.set_tooltip();
                unsafe {
                    Shell_NotifyIconW(NIM_MODIFY, &self.icon);
                }
            }
            CANCEL => self.cancel_recording(),
            COPY => self.copy_last_transcript(None),
            EXIT => unsafe {
                PostQuitMessage(0);
            },
            _ => {}
        }
    }

    fn copy_last_transcript(&mut self, origin: Option<u64>) {
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
            self.timer = unsafe { SetTimer(self.hwnd, TIMER, 30, None) != 0 };
        } else if !needed && self.timer {
            unsafe {
                KillTimer(self.hwnd, TIMER);
            }
            self.timer = false;
        }
    }

    fn indicator(&mut self, text: &str, mut badge: Option<PreviewBadge>) {
        if self.pill.is_null() {
            return;
        }
        let preview = badge.is_some();
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
            badge = badge.map(|badge| badge.for_preview_rows(rows));
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
        unsafe {
            SetWindowLongPtrW(
                self.pill,
                GWLP_USERDATA,
                if preview { dpi } else { -dpi } as isize,
            );
            SetWindowLongPtrW(
                self.pill,
                PILL_WELCOME_SLOT,
                isize::from(badge == Some(PreviewBadge::Dismiss)),
            );
            SetWindowTextW(self.pill, wide(text).as_ptr());
            SetWindowPos(
                self.pill,
                HWND_TOPMOST,
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            InvalidateRect(self.pill, null(), 0);
        }
        self.show_preview_shortcut(badge, left + width, top, dpi);
    }

    fn show_preview_shortcut(&self, badge: Option<PreviewBadge>, right: i32, top: i32, dpi: i32) {
        if self.preview_shortcut.is_null() {
            return;
        }
        let Some(badge) = badge else {
            self.hide_preview_shortcut();
            return;
        };
        let hotkey = self.binding.map(|binding| binding.hotkey);
        let label = wide(&badge.label(hotkey));
        let sizing_label = if badge == PreviewBadge::Dismiss {
            label.clone()
        } else {
            wide(&PreviewBadge::Stop.label(hotkey))
        };
        let height = SHORTCUT_HEIGHT * dpi / 96;
        let mut length = SIZE {
            cx: 130 * dpi / 96,
            cy: 0,
        };
        unsafe {
            let dc = GetDC(self.pill);
            if !dc.is_null() {
                let font = create_indicator_font(dpi, 11);
                if !font.is_null() {
                    let previous = SelectObject(dc, font);
                    GetTextExtentPoint32W(
                        dc,
                        sizing_label.as_ptr(),
                        (sizing_label.len() - 1) as i32,
                        &mut length,
                    );
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
            let width = length.cx + scale_preview(padding, dpi);
            if badge as isize > PreviewBadge::Dismiss as isize
                && GetCapture() == self.preview_shortcut
            {
                ReleaseCapture();
            }
            if let Some(duration) = PreviewBadge::countdown(badge as isize) {
                let remaining = self.status_until.map_or(duration, |until| {
                    until
                        .saturating_duration_since(Instant::now())
                        .min(duration)
                });
                SetWindowLongPtrW(self.preview_shortcut, 0, remaining.as_millis() as isize);
            }
            SetWindowLongPtrW(self.preview_shortcut, GWLP_USERDATA, badge as isize);
            SetWindowTextW(self.preview_shortcut, label.as_ptr());
            let left = right - width;
            let top = top - height - 4 * dpi / 96;
            let mut current = RECT::default();
            if IsWindowVisible(self.preview_shortcut) == 0
                || GetWindowRect(self.preview_shortcut, &mut current) == 0
                || (current.left, current.top, current.right, current.bottom)
                    != (left, top, right, top + height)
            {
                SetWindowPos(
                    self.preview_shortcut,
                    HWND_TOPMOST,
                    left,
                    top,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            InvalidateRect(self.preview_shortcut, null(), 0);
        }
    }

    fn recording_status(&mut self) {
        if let Some(recording) = &self.recording {
            self.indicator(
                &recording.status(),
                recording.previewing().then(|| recording.preview_badge()),
            );
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
        self.status_until = None;
        self.completed_preview = None;
        self.hide_preview_shortcut();
        unsafe {
            ShowWindow(self.pill, SW_HIDE);
        }
    }

    fn hide_preview_shortcut(&self) {
        if !self.preview_shortcut.is_null() {
            unsafe {
                if GetCapture() == self.preview_shortcut {
                    ReleaseCapture();
                }
                ShowWindow(self.preview_shortcut, SW_HIDE);
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
            let background = blend_color(GetSysColor(COLOR_BTNFACE), GetSysColor(COLOR_BTNTEXT), 1);
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
            let background = blend_color(GetSysColor(COLOR_BTNFACE), GetSysColor(COLOR_BTNTEXT), 1);
            let left = GetPixel(dc, 10, 8);
            let right_has_text = (0..SHORTCUT_HEIGHT)
                .any(|y| ((width - 120)..(width - 9)).any(|x| GetPixel(dc, x, y) != background));
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
            let accent = blend_color(GetSysColor(COLOR_BTNFACE), GetSysColor(COLOR_HIGHLIGHT), 3);
            for (badge, duration) in [
                (PreviewBadge::Dismiss, LAUNCH_PREVIEW_TIME),
                (PreviewBadge::Copied, COMPLETED_PREVIEW_TIME),
            ] {
                SetWindowLongPtrW(shortcut, GWLP_USERDATA, badge as isize);
                let total_ms = duration.as_millis() as isize;
                for (remaining_ms, expected_right) in [
                    (total_ms, accent),
                    (total_ms / 2, background),
                    (0, background),
                ] {
                    SetWindowLongPtrW(shortcut, 0, remaining_ms);
                    SendMessageW(shortcut, WM_PRINTCLIENT, dc as WPARAM, 0);
                    assert_eq!(GetPixel(dc, width * 3 / 4, 0), expected_right);
                    assert_eq!(
                        GetPixel(dc, width / 4, 0),
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
            let (clipboard, _writes, _feedback) = Clipboard::test_sink();
            let hotkey = "Ctrl+Shift+F2".parse().unwrap();
            let mut app = App {
                hwnd: parent,
                pill,
                preview_shortcut: shortcut,
                pill_font: None,
                icon: NOTIFYICONDATAW::default(),
                path: PathBuf::new(),
                binding: Some(Binding { id: 1, hotkey }),
                recording: None,
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
                assert_eq!(
                    GetWindowLongPtrW(shortcut, GWLP_USERDATA),
                    PreviewBadge::StopPending as isize
                );
                assert_eq!(GetWindowLongPtrW(pill, PILL_WELCOME_SLOT), 0);
                let long_text = "This longer transcript should wrap in the preview. ".repeat(20);
                app.indicator(&long_text, Some(badge));
                assert_eq!(GetWindowLongPtrW(shortcut, GWLP_USERDATA), badge as isize);
            }
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
            transcript: String::new(),
        };
        assert_eq!(recording.status(), "Listening… · Stop now to discard");
        recording.transcript = "Meet Wednesday.".into();
        assert!(recording.previewing());
        assert_eq!(recording.status(), "Meet Wednesday.");
        assert_eq!(
            recording.preview_badge().label(Some(Hotkey::default())),
            "CANCEL · CTRL+SHIFT+/"
        );
        assert!(!recording.finish_requested(now + CANCEL_WINDOW - Duration::from_millis(1)));
        assert!(!recording.finishing);
        recording.transcript.clear();
        assert!(recording.update_cancel_window(now + CANCEL_WINDOW));
        assert_eq!(recording.status(), "Listening… · Waiting for transcript");
        recording.transcript = "Meet Wednesday.".into();
        assert_eq!(recording.status(), "Meet Wednesday.");
        assert_eq!(
            recording.preview_badge().label(Some(Hotkey::default())),
            "STOP & COPY · CTRL+SHIFT+/"
        );
        let (clipboard, writes, feedback) = Clipboard::test_sink();
        let mut app = App {
            hwnd: null_mut(),
            pill: null_mut(),
            preview_shortcut: null_mut(),
            pill_font: None,
            icon: NOTIFYICONDATAW::default(),
            path: PathBuf::new(),
            binding: None,
            recording: Some(recording),
            last_transcript: None,
            recording_id: 1,
            completed_preview: None,
            status_until: None,
            clipboard,
            copies_pending: VecDeque::new(),
            timer: false,
        };
        app.activate_preview_shortcut(PreviewBadge::Stop as isize);
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
        send.send(Update::Done).unwrap();
        let before_final = Instant::now();
        app.tick();
        assert_eq!(writes.try_recv().unwrap().text, final_transcript);
        assert_eq!(app.last_transcript.as_deref(), Some(final_transcript));
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copying));
        let deadline = app.status_until.unwrap();
        assert!(deadline >= before_final + COMPLETED_PREVIEW_TIME);
        assert!(deadline <= Instant::now() + COMPLETED_PREVIEW_TIME);
        app.copy_feedback(Some(0), ClipboardFeedback::Failed);
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copying));
        feedback.send(ClipboardFeedback::Copied(29)).unwrap();
        app.tick();
        assert_eq!(app.completed_preview, Some(PreviewBadge::Copied));
        assert_eq!(app.status_until, Some(deadline));
        assert!(matches!(writes.try_recv(), Err(TryRecvError::Empty)));
        app.copy_feedback(Some(1), ClipboardFeedback::Failed);
        assert_eq!(app.completed_preview, Some(PreviewBadge::CopyFailed));
        assert_eq!(app.status_until, Some(deadline));
        app.status_until = Some(Instant::now());
        app.tick();
        assert!(app.completed_preview.is_none());
        app.copy_feedback(Some(1), ClipboardFeedback::Copied(29));
        assert!(app.status_until.is_none());
    }
}
