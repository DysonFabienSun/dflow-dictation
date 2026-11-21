use super::tray::{Binding, instance, message, wide};
use dflow_scratchpad::{
    audio,
    config::{Config, parse_proxy_url},
    hotkey::Hotkey,
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

pub fn open(owner: HWND, path: &Path, binding: &mut Option<Binding>) {
    let result = (|| {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let command = format!("\"{}\"", exe.display());
        let startup = read_startup().map_err(|error| error.to_string())?;
        let (send, scan) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(audio::devices());
        });
        let mut dialog = Dialog {
            owner,
            config: Config::load(path.to_path_buf()),
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
        message(owner, &format!("Could not open settings. {error}"));
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
                    state.set_devices(hwnd, Err("Microphone scan ended unexpectedly.".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            1
        }
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => {
                let result = unsafe { &mut *state }.save(hwnd);
                match result {
                    Ok(()) => unsafe {
                        EndDialog(hwnd, 1);
                    },
                    Err(error) => message(hwnd, &error),
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
        let mut items = vec![(None, "System default".to_owned())];
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
                "Saved microphone (unavailable)".to_owned(),
            ));
        }
        unsafe {
            SendDlgItemMessageW(hwnd, DEVICE, CB_RESETCONTENT, 0, 0);
        }
        self.devices.clear();
        for (id, label) in items {
            unsafe {
                SendDlgItemMessageW(
                    hwnd,
                    DEVICE,
                    CB_ADDSTRING,
                    0,
                    wide(&label).as_ptr() as LPARAM,
                );
            }
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
        let hotkey: Hotkey = get(hwnd, HOTKEY).parse()?;
        let mut settings = self.config.settings.clone();
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
            return Err("Enter an absolute Codex auth-file path, or leave it blank for automatic discovery.".into());
        }
        settings.proxy.url = parse_proxy_url(get(hwnd, PROXY_URL).trim())?;
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
            Some(Binding::register(self.owner, hotkey, id)?)
        };
        let result = self.save_changes(settings, hotkey, checked(hwnd, STARTUP));
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
    ) -> Result<(), String> {
        let desired = if startup {
            Some(self.command.as_str())
        } else {
            self.startup
                .as_deref()
                .filter(|command| *command != self.command)
        };
        let changed = desired != self.startup.as_deref();
        if changed {
            write_startup(desired)
                .map_err(|error| format!("Could not change Start with Windows. {error}"))?;
        }
        if let Err(error) = self.config.save_tray(settings, hotkey) {
            if changed && let Err(restore) = write_startup(self.startup.as_deref()) {
                return Err(format!(
                    "{error}\nCould not restore Start with Windows. {restore}"
                ));
            }
            if error.contains("changed on disk") {
                return Err(
                    "Settings changed on disk. Close and reopen this dialog before saving.".into(),
                );
            }
            return Err(error);
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
