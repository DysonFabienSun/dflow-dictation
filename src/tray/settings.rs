use super::tray::{Binding, instance, message, wide};
use dflow_scratchpad::{
    audio,
    config::{Config, ProxySettings, SaveError, Settings, parse_proxy_url},
    dictionary::Change,
    hotkey::Hotkey,
    locale::{Language, Locale},
    setup::{self, NetworkCheck, NetworkTarget, Setup},
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
    UI::{
        Input::KeyboardAndMouse::{EnableWindow, SetFocus, UnregisterHotKey},
        WindowsAndMessaging::*,
    },
};

const HOTKEY: i32 = 101;
const DEVICE: i32 = 102;
const AUTH: i32 = 103;
const PROXY_ON: i32 = 104;
const PROXY_URL: i32 = 105;
const STARTUP: i32 = 106;
const LAUNCH_PREVIEW: i32 = 107;
const LANGUAGE: i32 = 108;
const CONSENT: i32 = 109;
const CLEANUP: i32 = 110;
const TEST: i32 = 111;
const DICTIONARY: i32 = 112;
const TEST_RESULT: i32 = 113;
const CHECK_PROXY: i32 = 114;
const CHECK_CHATGPT: i32 = 115;
const NETWORK_STATUS: i32 = 116;
const HOTKEY_LABEL: i32 = 201;
const DEVICE_LABEL: i32 = 202;
const AUTH_LABEL: i32 = 203;
const LANGUAGE_LABEL: i32 = 204;
const RESTART_HINT: i32 = 205;
const SETUP_TITLE: i32 = 206;
const CONSENT_STATUS: i32 = 207;
const LOGIN_STATUS: i32 = 208;
const CLEANUP_DISCLOSURE: i32 = 209;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "DFlow Tray";

struct Dialog {
    owner: HWND,
    config: Config,
    binding: Option<Binding>,
    command: String,
    startup: Option<String>,
    devices: Vec<Option<String>>,
    scan: Option<mpsc::Receiver<Result<Vec<audio::InputDevice>, String>>>,
    setup: Setup,
    network: NetworkCheck,
    wizard: bool,
}

pub fn open(owner: HWND, path: &Path, binding: &mut Option<Binding>, locale: Locale, wizard: bool) {
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
            scan: Some(scan),
            setup: Setup::default(),
            network: NetworkCheck::default(),
            wizard,
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
            if let Some(scan) = &state.scan {
                match scan.try_recv() {
                    Ok(devices) => {
                        state.scan = None;
                        state.set_devices(hwnd, devices);
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        state.scan = None;
                        let error = state
                            .config
                            .locale
                            .text("Microphone scan ended unexpectedly.", "麦克风搜索意外中断");
                        state.set_devices(hwnd, Err(error.into()));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            if state.setup.poll(state.config.locale) {
                state.refresh_setup(hwnd);
            }
            if state.network.poll(state.config.locale) {
                render_network(
                    hwnd,
                    &state.network,
                    setup::availability_hint(state.config.locale),
                );
            }
            1
        }
        WM_COMMAND => match (wp & 0xffff) as i32 {
            CHECK_PROXY | CHECK_CHATGPT => {
                let state = unsafe { &mut *state };
                start_check(
                    hwnd,
                    &mut state.network,
                    (wp & 0xffff) as i32,
                    state.config.locale,
                );
                1
            }
            CONSENT | TEST | DICTIONARY => {
                let result = match (wp & 0xffff) as i32 {
                    CONSENT => {
                        let (enabled, locale, proxy) = unsafe {
                            (
                                (*state).config.settings.cloud_dictation == Some(true),
                                (*state).config.locale,
                                (*state).config.settings.proxy.clone(),
                            )
                        };
                        let choice = if enabled {
                            Ok(Some((false, None)))
                        } else {
                            let proxy = read_proxy(hwnd, locale).unwrap_or(proxy);
                            ask_consent(hwnd, proxy, locale)
                                .map(|result| result.map(|(enabled, proxy)| (enabled, Some(proxy))))
                        };
                        choice.and_then(|choice| {
                            if let Some((enabled, proxy)) = choice {
                                unsafe { &mut *state }.save_consent(hwnd, enabled, proxy)?;
                            }
                            Ok(())
                        })
                    }
                    TEST => unsafe { &mut *state }.test(hwnd),
                    DICTIONARY => {
                        let (path, locale, settings) = {
                            let state = unsafe { &*state };
                            (
                                state.config.path.clone(),
                                state.config.locale,
                                state.current_dictation_settings(hwnd),
                            )
                        };
                        settings.and_then(|settings| open_dictionary(hwnd, path, settings, locale))
                    }
                    _ => unreachable!(),
                };
                if let Err(error) = result {
                    set(hwnd, TEST_RESULT, &error);
                }
                1
            }
            IDOK => {
                let locale = unsafe { (*state).config.locale };
                let result = unsafe { &mut *state }.save(hwnd);
                match result {
                    Ok(()) => unsafe {
                        EndDialog(hwnd, 1);
                    },
                    Err(error) => message(hwnd, &error, locale),
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
        let title = if self.wizard {
            locale.text("Dictation setup", "听写设置").to_owned()
        } else {
            format!(
                "{}{}",
                locale.dictation_name(),
                locale.text(" Settings", "设置")
            )
        };
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
            (SETUP_TITLE, locale.text("Dictation setup", "听写设置")),
            (
                CLEANUP,
                locale.text("Clean up after dictation", "听写后自动整理"),
            ),
            (CLEANUP_DISCLOSURE, setup::cleanup_disclosure(locale)),
            (
                DICTIONARY,
                locale.text("ChatGPT Voice Dictionary…", "ChatGPT 语音词典…"),
            ),
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
        label_network_buttons(hwnd, locale);
        render_network(hwnd, &self.network, setup::availability_hint(locale));
        check(hwnd, CLEANUP, settings.cleanup);
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
        self.check_login();
        self.refresh_setup(hwnd);
        unsafe {
            SetTimer(hwnd, 1, 50, None);
        }
    }

    fn check_login(&mut self) {
        if self.config.settings.cloud_dictation == Some(true) {
            self.setup
                .check_login(self.config.settings.clone(), self.config.locale);
        }
    }

    fn refresh_setup(&self, hwnd: HWND) {
        let locale = self.config.locale;
        let enabled = self.config.settings.cloud_dictation == Some(true);
        set(
            hwnd,
            CONSENT,
            if enabled {
                locale.text("Disable cloud dictation", "关闭云端听写")
            } else {
                locale.text("Enable cloud dictation", "启用云端听写")
            },
        );
        set(
            hwnd,
            CONSENT_STATUS,
            if enabled {
                locale.text("On · chatgpt.com", "已启用 · chatgpt.com")
            } else {
                locale.text("Off · chatgpt.com", "已关闭 · chatgpt.com")
            },
        );
        set(
            hwnd,
            LOGIN_STATUS,
            if let Some(error) = &self.config.error {
                error
            } else if enabled {
                &self.setup.login
            } else {
                setup::consent_text(locale)
            },
        );
        set(
            hwnd,
            TEST,
            if self.setup.testing() {
                locale.text("Stop test recording", "停止测试录音")
            } else {
                locale.text("Test dictation", "测试听写")
            },
        );
        set(hwnd, TEST_RESULT, &self.setup.test_result);
        enable(
            hwnd,
            TEST,
            enabled && !self.setup.stopping() && (!self.setup.busy() || self.setup.testing()),
        );
        enable(hwnd, DICTIONARY, enabled && !self.setup.busy());
        enable(hwnd, CLEANUP, enabled);
        enable(hwnd, IDOK, !self.setup.testing());
    }

    fn save_consent(
        &mut self,
        hwnd: HWND,
        enabled: bool,
        proxy: Option<ProxySettings>,
    ) -> Result<(), String> {
        let mut settings = self.config.settings.clone();
        settings.cloud_dictation = Some(enabled);
        if let Some(proxy) = &proxy {
            settings.proxy = proxy.clone();
        }
        self.config
            .save(settings)
            .map_err(|error| error.restart_message(&self.config))?;
        if let Some(proxy) = proxy {
            set(hwnd, PROXY_URL, proxy.url.as_str());
            check(hwnd, PROXY_ON, proxy.enabled);
        }
        self.setup = Setup::default();
        self.check_login();
        self.refresh_setup(hwnd);
        Ok(())
    }

    fn current_dictation_settings(&self, hwnd: HWND) -> Result<Settings, String> {
        let locale = self.config.locale;
        let current = Config::load_for(self.config.path.clone(), Some(locale));
        if let Some(error) = current.error {
            return Err(error);
        }
        current.settings.require_dictation(locale)?;
        self.dictation_settings(hwnd, current.settings)
    }

    fn test(&mut self, hwnd: HWND) -> Result<(), String> {
        let settings = if self.setup.testing() {
            self.config.settings.clone()
        } else {
            self.current_dictation_settings(hwnd)?
        };
        self.setup.test(settings, self.config.locale);
        self.refresh_setup(hwnd);
        Ok(())
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
        let mut settings = self.dictation_settings(hwnd, self.config.settings.clone())?;
        let index = unsafe { SendDlgItemMessageW(hwnd, LANGUAGE, CB_GETCURSEL, 0, 0) };
        if let Some(&language) = Language::OPTIONS.get(index as usize) {
            settings.language = language;
        }
        if settings.cloud_dictation == Some(true) {
            settings.cleanup = checked(hwnd, CLEANUP);
        }

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

    fn dictation_settings(&self, hwnd: HWND, mut settings: Settings) -> Result<Settings, String> {
        let locale = self.config.locale;
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
        settings.proxy = read_proxy(hwnd, locale)?;
        Ok(settings)
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

struct ConsentDialog {
    locale: Locale,
    proxy: ProxySettings,
    network: NetworkCheck,
}

pub fn ask_consent(
    owner: HWND,
    proxy: ProxySettings,
    locale: Locale,
) -> Result<Option<(bool, ProxySettings)>, String> {
    let mut dialog = ConsentDialog {
        locale,
        proxy,
        network: NetworkCheck::default(),
    };
    let result = unsafe {
        DialogBoxParamW(
            instance(),
            101usize as *const u16,
            owner,
            Some(consent_proc),
            &mut dialog as *mut ConsentDialog as LPARAM,
        )
    };
    match result as i32 {
        IDYES | IDNO => Ok(Some((result == IDYES as isize, dialog.proxy))),
        -1 => Err(io::Error::last_os_error().to_string()),
        _ => Ok(None),
    }
}

unsafe extern "system" fn consent_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
    }
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ConsentDialog;
    if state.is_null() {
        return 0;
    }
    let state = unsafe { &mut *state };
    let locale = state.locale;
    match msg {
        WM_INITDIALOG => {
            unsafe {
                SetWindowTextW(
                    hwnd,
                    wide(locale.text("Dictation setup", "听写设置")).as_ptr(),
                )
            };
            set(hwnd, 201, setup::consent_text(locale));
            set(hwnd, 202, setup::availability_hint(locale));
            set(
                hwnd,
                PROXY_ON,
                locale.text("Use network proxy", "使用网络代理"),
            );
            set(hwnd, PROXY_URL, state.proxy.url.as_str());
            check(hwnd, PROXY_ON, state.proxy.enabled);
            label_network_buttons(hwnd, locale);
            set(
                hwnd,
                IDYES,
                locale.text("Enable cloud dictation", "启用云端听写"),
            );
            set(
                hwnd,
                IDNO,
                locale.text("Continue without dictation", "继续使用，暂不启用听写"),
            );
            unsafe {
                SetFocus(GetDlgItem(hwnd, IDNO));
                SetTimer(hwnd, 1, 50, None);
            }
            0
        }
        WM_TIMER => {
            if state.network.poll(locale) {
                render_network(hwnd, &state.network, "");
            }
            1
        }
        WM_COMMAND => match (wp & 0xffff) as i32 {
            CHECK_PROXY | CHECK_CHATGPT => {
                start_check(hwnd, &mut state.network, (wp & 0xffff) as i32, locale);
                1
            }
            id @ (IDYES | IDNO) => {
                match read_proxy(hwnd, locale) {
                    Ok(proxy) => state.proxy = proxy,
                    Err(error) if id == IDYES => {
                        set(hwnd, NETWORK_STATUS, &error);
                        return 1;
                    }
                    Err(_) => {}
                }
                unsafe { EndDialog(hwnd, id as isize) };
                1
            }
            IDCANCEL => {
                unsafe { EndDialog(hwnd, IDCANCEL as isize) };
                1
            }
            _ => 0,
        },
        _ => 0,
    }
}

fn read_proxy(hwnd: HWND, locale: Locale) -> Result<ProxySettings, String> {
    Ok(ProxySettings {
        url: parse_proxy_url(&get(hwnd, PROXY_URL), locale)?,
        enabled: checked(hwnd, PROXY_ON),
    })
}

fn label_network_buttons(hwnd: HWND, locale: Locale) {
    set(hwnd, CHECK_PROXY, NetworkTarget::Proxy.label(locale));
    set(hwnd, CHECK_CHATGPT, NetworkTarget::ChatGpt.label(locale));
}

fn start_check(hwnd: HWND, check: &mut NetworkCheck, command: i32, locale: Locale) {
    let target = if command == CHECK_PROXY {
        NetworkTarget::Proxy
    } else {
        NetworkTarget::ChatGpt
    };
    let proxy = if command == CHECK_CHATGPT && !checked(hwnd, PROXY_ON) {
        Ok(ProxySettings::default())
    } else {
        read_proxy(hwnd, locale)
    };
    match proxy {
        Ok(proxy) => {
            check.start(proxy, target, locale);
            render_network(hwnd, check, "");
        }
        Err(error) => set(hwnd, NETWORK_STATUS, &error),
    }
}

fn render_network(hwnd: HWND, check: &NetworkCheck, hint: &str) {
    let status = if check.status.is_empty() {
        hint
    } else {
        &check.status
    };
    set(hwnd, NETWORK_STATUS, status);
    enable(hwnd, CHECK_PROXY, !check.checking());
    enable(hwnd, CHECK_CHATGPT, !check.checking());
}

struct DictionaryDialog {
    path: PathBuf,
    settings: Settings,
    locale: Locale,
    setup: Setup,
}

const WORDS: i32 = 101;
const WORD: i32 = 102;
const ADD: i32 = 103;
const REMOVE: i32 = 104;
const RELOAD: i32 = 105;
const DICTIONARY_STATUS: i32 = 202;

fn open_dictionary(
    owner: HWND,
    path: PathBuf,
    settings: Settings,
    locale: Locale,
) -> Result<(), String> {
    let mut dialog = DictionaryDialog {
        path,
        settings,
        locale,
        setup: Setup::default(),
    };
    let result = unsafe {
        DialogBoxParamW(
            instance(),
            102usize as *const u16,
            owner,
            Some(dictionary_proc),
            &mut dialog as *mut DictionaryDialog as LPARAM,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

unsafe extern "system" fn dictionary_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
    }
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut DictionaryDialog;
    if state.is_null() {
        return 0;
    }
    let state = unsafe { &mut *state };
    match msg {
        WM_INITDIALOG => {
            let locale = state.locale;
            unsafe {
                SetWindowTextW(
                    hwnd,
                    wide(locale.text("ChatGPT Voice Dictionary", "ChatGPT 语音词典")).as_ptr(),
                );
                SetTimer(hwnd, 1, 50, None);
            }
            set(hwnd, 201, setup::dictionary_disclosure(locale));
            set(hwnd, ADD, locale.text("Add", "添加"));
            set(hwnd, REMOVE, locale.text("Remove selected", "移除所选词条"));
            set(hwnd, RELOAD, locale.text("Reload", "重新加载"));
            set(hwnd, IDCANCEL, locale.text("Close", "关闭"));
            state.request(hwnd, None);
            1
        }
        WM_TIMER => {
            if state.setup.poll(state.locale) {
                state.refresh(hwnd);
            }
            1
        }
        WM_COMMAND => {
            match (wp & 0xffff) as i32 {
                IDCANCEL => {
                    unsafe { EndDialog(hwnd, 0) };
                }
                RELOAD => state.request(hwnd, None),
                ADD => state.request(hwnd, Some(Change::Add(get(hwnd, WORD)))),
                REMOVE => {
                    let selected = unsafe { SendDlgItemMessageW(hwnd, WORDS, LB_GETCURSEL, 0, 0) };
                    let entry = state
                        .setup
                        .dictionary
                        .as_ref()
                        .and_then(|dictionary| dictionary.words.get(selected as usize));
                    if let Some(entry) = entry {
                        state.request(hwnd, Some(Change::Remove(entry.id.clone())));
                    }
                }
                _ => return 0,
            }
            1
        }
        _ => 0,
    }
}

impl DictionaryDialog {
    fn request(&mut self, hwnd: HWND, change: Option<Change>) {
        let current = Config::load_for(self.path.clone(), Some(self.locale));
        match current
            .error
            .map_or_else(|| current.settings.require_dictation(self.locale), Err)
        {
            Ok(()) => self
                .setup
                .dictionary(self.settings.clone(), self.locale, change),
            Err(error) => {
                self.setup.dictionary = None;
                self.setup.dictionary_status = error;
            }
        }
        self.refresh(hwnd);
    }

    fn refresh(&self, hwnd: HWND) {
        unsafe { SendDlgItemMessageW(hwnd, WORDS, LB_RESETCONTENT, 0, 0) };
        let mut status = self.setup.dictionary_status.clone();
        let ready = self.setup.dictionary.is_some();
        if let Some(dictionary) = &self.setup.dictionary {
            for entry in &dictionary.words {
                unsafe {
                    SendDlgItemMessageW(
                        hwnd,
                        WORDS,
                        LB_ADDSTRING,
                        0,
                        wide(&entry.text).as_ptr() as LPARAM,
                    )
                };
            }
            status.push_str(&match self.locale {
                Locale::English => format!(
                    " · Up to {} UTF-16 units per entry",
                    dictionary.max_entry_utf16_units
                ),
                Locale::SimplifiedChinese => format!(
                    " · 每个词条最多 {} 个 UTF-16 单元",
                    dictionary.max_entry_utf16_units
                ),
            });
        }
        set(hwnd, DICTIONARY_STATUS, &status);
        enable(hwnd, ADD, ready);
        enable(hwnd, REMOVE, ready);
        enable(hwnd, RELOAD, !self.setup.busy());
    }
}

fn enable(hwnd: HWND, id: i32, enabled: bool) {
    unsafe { EnableWindow(GetDlgItem(hwnd, id), enabled.into()) };
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
            scan: Some(scan),
            setup: Setup::default(),
            network: NetworkCheck::default(),
            wizard: false,
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
            scan: Some(scan),
            setup: Setup::default(),
            network: NetworkCheck::default(),
            wizard: false,
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
