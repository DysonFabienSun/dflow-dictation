#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() {
    use dflow_scratchpad::{
        config::{self, Config},
        locale::Locale,
    };
    use std::ptr::null_mut;

    match std::env::args_os()
        .nth(1)
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("--help" | "-h") => {
            console_output(
                "DFlow Dictation\n\nUsage: dflow-dictation [--help | --version]\nRuns in the Windows notification area.",
            );
            return;
        }
        Some("--version" | "-V") => {
            console_output(concat!("DFlow Dictation ", env!("CARGO_PKG_VERSION")));
            return;
        }
        _ => {}
    }

    let config = match config::settings_path() {
        Ok(path) => Config::load(path),
        Err(error) => {
            let locale = Locale::system();
            let message = locale.text("Could not locate settings", "无法定位设置文件");
            tray::message(null_mut(), &format!("{message} · {error}"), locale);
            return;
        }
    };

    let locale = config.locale;
    if let Err(error) = tray::run(config) {
        tray::message(null_mut(), &error, locale);
    }
}

#[cfg(windows)]
fn console_output(text: &str) {
    use std::io::Write;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, FreeConsole, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    let attached = unsafe {
        GetStdHandle(STD_OUTPUT_HANDLE).is_null() && AttachConsole(ATTACH_PARENT_PROCESS) != 0
    };
    let _ = writeln!(std::io::stdout().lock(), "{text}");
    if attached {
        unsafe { FreeConsole() };
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("DFlow Dictation requires Windows.");
}
