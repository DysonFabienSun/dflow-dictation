#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() {
    if let Err(error) = tray::run() {
        tray::message(std::ptr::null_mut(), &error);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("DFlow Dictation requires Windows.");
}
