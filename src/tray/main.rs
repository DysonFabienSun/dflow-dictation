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

#[cfg(not(windows))]
fn main() {
    eprintln!("DFlow Dictation requires Windows.");
}
