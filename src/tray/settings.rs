use super::tray::{Binding, instance, message, wide};
use dflow_scratchpad::{
    audio,
    config::{Config, SaveError, parse_proxy_url},
    hotkey::Hotkey,
    locale::{Language, Locale},
};
use std::{
    io,
    path::{Path, PathBuf},
    ptr::null_mut,
    sync::mpsc,
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, WPARAM},
    System::Registry::*,
    UI::{Input::KeyboardAndMouse::UnregisterHotKey, WindowsAndMessaging::*},
};

const HOTKEY: i32 = 101;
const DEVICE: i32 = 102;
const AUTH: i32 = 103;
const PROXY_ON: i32 = 104;
const PROXY_URL: i32 = 105;
const STARTUP: i32 = 106;
const LAUNCH_PREVIEW: i32 = 107;
const LANGUAGE: i32 = 108;
const HOTKEY_LABEL: i32 = 201;
const DEVICE_LABEL: i32 = 202;
const AUTH_LABEL: i32 = 203;
const LANGUAGE_LABEL: i32 = 204;
const RESTART_HINT: i32 = 205;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "DFlow Tray";

struct Dialog {
    owner: HWND,
    config: Config,
    binding: Option<Binding>,
    command: String,
    startup: Option<String>,
    devices: Vec<Option<String>>,
    scan: mpsc::Receiver<Result<Vec<audio::InputDevice>, String>>,
}

pub fn open(owner: HWND, path: &Path, binding: &mut Option<Binding>, locale: Locale) {
    let result = (|| {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let command = format!("\"{}\"", exe.display());
        let startup = read_startup().map_err(|error| error.to_string())?;
        let (send, scan) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(audio::devices(locale));
        });
        let mut dialog = Dialog {
            owner,
            config: Config::load_for(path.to_path_buf(), Some(locale)),
            binding: *binding,
            command,
            startup,
            devices: Vec::new(),
            scan,
        };
        let result = unsafe {
            DialogBoxParamW(
                instance(),
                100usize as *const u16,
                owner,
                Some(dialog_proc),
                &mut dialog as *mut Dialog as LPARAM,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error().to_string());
        }
        *binding = dialog.binding;
        Ok(())
    })();
    if let Err(error) = result {
        let hint = locale.text("Could not open settings.", "无法打开设置");
        message(owner, &format!("{hint} {error}"), locale);
    }
}

unsafe extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
    }
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Dialog;
    if state.is_null() {
        return 0;
    }
    match msg {
        WM_INITDIALOG => {
            let state = unsafe { &mut *state };
            state.populate(hwnd);
            1
        }
        WM_TIMER => {
            let state = unsafe { &mut *state };
            match state.scan.try_recv() {
                Ok(devices) => {
                    unsafe {
                        KillTimer(hwnd, 1);
                    }
                    state.set_devices(hwnd, devices);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    unsafe {
                        KillTimer(hwnd, 1);
                    }
                    let error = state
                        .config
                        .locale
                        .text("Microphone scan ended unexpectedly.", "麦克风搜索意外中断");
                    state.set_devices(hwnd, Err(error.into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            1
        }
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => {
                let state = unsafe { &mut *state };
                let result = state.save(hwnd);
                match result {
                    Ok(()) => unsafe {
                        EndDialog(hwnd, 1);
                    },
                    Err(error) => message(hwnd, &error, state.config.locale),
                }
                1
            }
            IDCANCEL => {
                unsafe {
                    EndDialog(hwnd, 0);
                }
                1
            }
            _ => 0,
        },
        _ => 0,
    }
}

impl Dialog {
    fn populate(&mut self, hwnd: HWND) {
        let locale = self.config.locale;
        let title = format!(
            "{}{}",
            locale.dictation_name(),
            locale.text(" Settings", "设置")
        );
        unsafe { SetWindowTextW(hwnd, wide(&title).as_ptr()) };
        for (id, name) in [
            (
                HOTKEY_LABEL,
                locale.text("Dictation shortcut", "听写快捷键"),
            ),
            (DEVICE_LABEL, locale.text("Microphone", "麦克风")),
            (AUTH_LABEL, locale.text("Codex auth file", "Codex 登录文件")),
            (PROXY_ON, locale.text("Use network proxy", "使用网络代理")),
            (STARTUP, locale.text("Start with Windows", "开机自启动")),
            (LAUNCH_PREVIEW, locale.text("Show startup tip", "启动提示")),
            (
                LANGUAGE_LABEL,
                locale.text("Interface language", "界面语言"),
            ),
            (RESTART_HINT, locale.language_changed()),
            (IDOK, locale.text("Save", "保存")),
            (IDCANCEL, locale.text("Cancel", "取消")),
        ] {
            set(hwnd, id, name);
        }
        for (index, language) in Language::OPTIONS.into_iter().enumerate() {
            add_combo_item(hwnd, LANGUAGE, language.label(locale));
            if language == self.config.settings.language {
                unsafe { SendDlgItemMessageW(hwnd, LANGUAGE, CB_SETCURSEL, index, 0) };
            }
        }

        let binding = self
            .config
            .tray_hotkey()
            .unwrap_or_else(|_| self.binding.map(|b| b.hotkey).unwrap_or_default());
        set(hwnd, HOTKEY, &binding.to_string());
        let settings = &self.config.settings;
        if let Some(path) = &settings.auth_path {
            set(hwnd, AUTH, &path.to_string_lossy());
        }
        set(hwnd, PROXY_URL, settings.proxy.url.as_str());
        check(hwnd, PROXY_ON, settings.proxy.enabled);
        check(
            hwnd,
            STARTUP,
            self.startup.as_deref() == Some(&self.command),
        );
        check(
            hwnd,
            LAUNCH_PREVIEW,
            self.config.tray_launch_preview().unwrap_or(true),
        );
        self.set_devices(hwnd, Ok(Vec::new()));
        unsafe {
            SetTimer(hwnd, 1, 50, None);
        }
    }

    fn set_devices(&mut self, hwnd: HWND, devices: Result<Vec<audio::InputDevice>, String>) {
        let selected = self
            .selected_device(hwnd)
            .unwrap_or(&self.config.settings.device)
            .clone();
        let locale = self.config.locale;
        let mut items = vec![(None, locale.text("System default", "系统默认").to_owned())];
        if let Ok(devices) = devices {
            items.extend(
                devices
                    .into_iter()
                    .map(|device| (Some(device.id), device.label)),
            );
        }
        if let Some(id) = selected
            .as_ref()
            .filter(|id| !items.iter().any(|(key, _)| key.as_ref() == Some(*id)))
        {
            items.push((
                Some(id.clone()),
                locale
                    .text("Saved microphone (unavailable)", "所选麦克风不可用")
                    .to_owned(),
            ));
        }
        unsafe {
            SendDlgItemMessageW(hwnd, DEVICE, CB_RESETCONTENT, 0, 0);
        }
        self.devices.clear();
        for (id, label) in items {
            add_combo_item(hwnd, DEVICE, &label);
            self.devices.push(id);
        }
        let index = self
            .devices
            .iter()
            .position(|id| *id == selected)
            .unwrap_or_default();
        unsafe {
            SendDlgItemMessageW(hwnd, DEVICE, CB_SETCURSEL, index, 0);
        }
    }

    fn selected_device(&self, hwnd: HWND) -> Option<&Option<String>> {
        let index = unsafe { SendDlgItemMessageW(hwnd, DEVICE, CB_GETCURSEL, 0, 0) };
        self.devices.get(index as usize)
    }

    fn save(&mut self, hwnd: HWND) -> Result<(), String> {
        let locale = self.config.locale;
        let hotkey = Hotkey::parse_localized(&get(hwnd, HOTKEY), locale)?;
        let mut settings = self.config.settings.clone();
        let index = unsafe { SendDlgItemMessageW(hwnd, LANGUAGE, CB_GETCURSEL, 0, 0) };
        if let Some(&language) = Language::OPTIONS.get(index as usize) {
            settings.language = language;
        }
        if let Some(device) = self.selected_device(hwnd) {
            settings.device = device.clone();
        }
        let auth = get(hwnd, AUTH);
        settings.auth_path = (!auth.trim().is_empty()).then(|| PathBuf::from(auth.trim()));
        if settings
            .auth_path
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(locale.text("Enter an absolute Codex auth-file path, or leave it blank for automatic discovery.", "请输入 Codex 登录文件的绝对路径，留空则自动查找").into());
        }
        settings.proxy.url = parse_proxy_url(get(hwnd, PROXY_URL).trim(), locale)?;
        settings.proxy.enabled = checked(hwnd, PROXY_ON);

        let previous = self.binding;
        let next = if previous.is_some_and(|binding| binding.hotkey == hotkey) {
            None
        } else {
            let id = if previous.is_some_and(|binding| binding.id == 1) {
                2
            } else {
                1
            };
            Some(Binding::register(self.owner, hotkey, id, locale)?)
        };
        let result = self.save_changes(
            settings,
            hotkey,
            checked(hwnd, STARTUP),
            checked(hwnd, LAUNCH_PREVIEW),
        );
        if let Err(error) = result {
            if let Some(binding) = next {
                unsafe {
                    UnregisterHotKey(self.owner, binding.id);
                }
            }
            return Err(error);
        }
        if let Some(binding) = next {
            if let Some(previous) = previous {
                unsafe {
                    UnregisterHotKey(self.owner, previous.id);
                }
            }
            self.binding = Some(binding);
        }
        Ok(())
    }

    fn save_changes(
        &mut self,
        settings: dflow_scratchpad::config::Settings,
        hotkey: Hotkey,
        startup: bool,
        show_launch_preview: bool,
    ) -> Result<(), String> {
        let locale = self.config.locale;
        let desired = if startup {
            Some(self.command.as_str())
        } else {
            self.startup
                .as_deref()
                .filter(|command| *command != self.command)
        };
        let changed = desired != self.startup.as_deref();
        if changed {
            write_startup(desired).map_err(|error| {
                let hint = locale.text(
                    "Could not change Start with Windows.",
                    "无法修改开机自启动设置",
                );
                format!("{hint} {error}")
            })?;
        }
        if let Err(error) = self.config.save_tray(settings, hotkey, show_launch_preview) {
            let message = match error {
                SaveError::ChangedOnDisk => locale
                    .text(
                        "Settings changed on disk. Close and reopen this dialog before saving.",
                        "设置已被修改，请关闭并重新打开此窗口后再保存",
                    )
                    .into(),
                SaveError::Message(message) => message,
            };
            if changed && let Err(restore) = write_startup(self.startup.as_deref()) {
                let hint = locale.text(
                    "Could not restore Start with Windows.",
                    "无法恢复开机自启动设置",
                );
                return Err(format!("{message}\n{hint} {restore}"));
            }
            return Err(message);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_scan_preserves_system_default_selected_during_scan() {
        let root = tempfile::tempdir().unwrap();
        let (_, scan) = mpsc::channel();
        let mut dialog = Dialog {
            owner: null_mut(),
            config: Config::load(root.path().join("dflow.toml")),
            binding: None,
            command: String::new(),
            startup: None,
            devices: Vec::new(),
            scan,
        };
        dialog.config.settings.device = Some("saved".into());
        let hwnd =
            unsafe { CreateDialogParamW(instance(), 100usize as *const u16, null_mut(), None, 0) };
        assert!(!hwnd.is_null());
        dialog.set_devices(hwnd, Ok(Vec::new()));
        assert_eq!(dialog.selected_device(hwnd), Some(&Some("saved".into())));

        unsafe { SendDlgItemMessageW(hwnd, DEVICE, CB_SETCURSEL, 0, 0) };
        dialog.set_devices(
            hwnd,
            Ok(vec![audio::InputDevice {
                id: "saved".into(),
                label: "Saved microphone".into(),
            }]),
        );
        assert_eq!(dialog.selected_device(hwnd), Some(&None));
        unsafe { DestroyWindow(hwnd) };
    }

    #[test]
    fn settings_dialog_displays_and_saves_the_shared_language() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        std::fs::write(&path, "[ui]\nlanguage = \"zh-CN\"\n").unwrap();
        let (_, scan) = mpsc::channel();
        let mut dialog = Dialog {
            owner: null_mut(),
            config: Config::load(path.clone()),
            binding: Some(Binding {
                id: 1,
                hotkey: Hotkey::default(),
            }),
            command: String::new(),
            startup: None,
            devices: Vec::new(),
            scan,
        };
        let hwnd =
            unsafe { CreateDialogParamW(instance(), 100usize as *const u16, null_mut(), None, 0) };
        assert!(!hwnd.is_null());
        dialog.populate(hwnd);
        assert_eq!(get(hwnd, LANGUAGE_LABEL), "界面语言");
        assert_eq!(get(hwnd, STARTUP), "开机自启动");
        assert_eq!(get(hwnd, PROXY_ON), "使用网络代理");
        assert_eq!(get(hwnd, LAUNCH_PREVIEW), "启动提示");
        assert_eq!(get(hwnd, RESTART_HINT), "界面语言将在重启后生效");
        assert_eq!(
            unsafe { SendDlgItemMessageW(hwnd, LANGUAGE, CB_GETCURSEL, 0, 0) },
            2
        );

        unsafe { SendDlgItemMessageW(hwnd, LANGUAGE, CB_SETCURSEL, 1, 0) };
        dialog.save(hwnd).unwrap();
        assert_eq!(dialog.config.locale, Locale::SimplifiedChinese);
        dialog.config = Config::load(path);
        dialog.populate(hwnd);
        assert_eq!(get(hwnd, LANGUAGE_LABEL), "Interface language");
        assert_eq!(get(hwnd, STARTUP), "Start with Windows");
        unsafe { DestroyWindow(hwnd) };
    }
}

fn add_combo_item(hwnd: HWND, id: i32, label: &str) {
    unsafe { SendDlgItemMessageW(hwnd, id, CB_ADDSTRING, 0, wide(label).as_ptr() as LPARAM) };
}

fn set(hwnd: HWND, id: i32, text: &str) {
    unsafe {
        SetDlgItemTextW(hwnd, id, wide(text).as_ptr());
    }
}
fn get(hwnd: HWND, id: i32) -> String {
    let mut data = vec![0; 32768];
    let len = unsafe { GetDlgItemTextW(hwnd, id, data.as_mut_ptr(), data.len() as i32) };
    String::from_utf16_lossy(&data[..len as usize])
}
fn check(hwnd: HWND, id: i32, enabled: bool) {
    unsafe {
        SendDlgItemMessageW(hwnd, id, BM_SETCHECK, usize::from(enabled), 0);
    }
}
fn checked(hwnd: HWND, id: i32) -> bool {
    unsafe { SendDlgItemMessageW(hwnd, id, BM_GETCHECK, 0, 0) != 0 }
}

fn read_startup() -> io::Result<Option<String>> {
    let mut data = [0u16; 1024];
    let mut size = size_of_val(&data) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_NAME).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    match status {
        0 => Ok(Some(
            String::from_utf16_lossy(&data[..size as usize / 2])
                .trim_end_matches('\0')
                .to_owned(),
        )),
        ERROR_FILE_NOT_FOUND => Ok(None),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

fn write_startup(command: Option<&str>) -> io::Result<()> {
    let (key, name) = (wide(RUN_KEY), wide(RUN_NAME));
    let status = if let Some(command) = command {
        let data = wide(command);
        if data.len() > 261 {
            return Err(io::Error::other(
                "The executable path is too long for Windows startup.",
            ));
        }
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                size_of_val(data.as_slice()) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
    };
    match status {
        0 => Ok(()),
        ERROR_FILE_NOT_FOUND if command.is_none() => Ok(()),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}
