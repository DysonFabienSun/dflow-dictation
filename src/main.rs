mod agent;
mod app;
mod audio;
mod clipboard;
mod codex;
mod config;
mod editor;
mod transcript;
mod ui;

use crossterm::{
    cursor::Show,
    event::{self, DisableFocusChange, EnableFocusChange},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    env,
    io::{self, IsTerminal},
    path::PathBuf,
    time::Duration,
};

#[cfg(windows)]
struct InputMode {
    handle: windows_sys::Win32::Foundation::HANDLE,
    mode: u32,
}

#[cfg(windows)]
impl InputMode {
    fn capture() -> io::Result<Self> {
        use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, mode })
    }
}

#[cfg(windows)]
impl Drop for InputMode {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        unsafe { SetConsoleMode(self.handle, self.mode) };
    }
}

struct Screen {
    #[cfg(windows)]
    _input_mode: InputMode,
}

impl Screen {
    fn enter() -> io::Result<Self> {
        #[cfg(windows)]
        let input_mode = InputMode::capture()?;
        terminal::enable_raw_mode()?;
        let guard = Self {
            #[cfg(windows)]
            _input_mode: input_mode,
        };
        execute!(io::stdout(), EnterAlternateScreen, EnableFocusChange)?;
        Ok(guard)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableFocusChange, LeaveAlternateScreen, Show);
        #[cfg(not(windows))]
        let _ = terminal::disable_raw_mode();
    }
}

fn run(path: PathBuf, agent_access: bool) -> io::Result<()> {
    let mut app = app::App::new(config::Config::load(path));
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let window = if agent_access {
        match agent::Window::start() {
            Ok(window) => Some(window),
            Err(error) => {
                app.set_agent_error(format!("Agent access unavailable: {error}"));
                None
            }
        }
    } else {
        None
    };
    let mut redraw = true;
    while !app.quit {
        redraw |= app.tick();
        if let Some(window) = &window {
            for (command, reply) in window.pending.try_iter().take(4) {
                if !reply.is_closed() {
                    let (result, changed) = app.agent_command(command);
                    let _ = reply.send(result);
                    redraw |= changed;
                }
            }
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            redraw = false;
        }
        if event::poll(Duration::from_millis(20))? {
            app.event(event::read()?);
            redraw = true;
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let mut arguments = env::args_os().skip(1);
    let mut agent_access = true;
    if let Some(first) = arguments.next() {
        match first.to_str() {
            Some("--help" | "-h") if arguments.next().is_none() => {
                println!(
                    "DFlow · Dictation scratchpad\n\nUsage: dflow [--no-agent-access]\n       dflow --mcp\n\nOpen DFlow in PowerShell 7 under Windows Terminal.\nCtrl+Space dictates, Ctrl+/ opens commands, and Ctrl+Z undoes.\nCtrl+C copies the note, Ctrl+X cuts it, and Ctrl+W exits."
                );
                return ExitCode::SUCCESS;
            }
            Some("--version" | "-V") if arguments.next().is_none() => {
                println!("DFlow {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            Some("--mcp") if arguments.next().is_none() => {
                return match agent::serve() {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("DFlow MCP: {error}");
                        ExitCode::FAILURE
                    }
                };
            }
            Some("--no-agent-access") if arguments.next().is_none() => {
                agent_access = false;
            }
            _ => {
                eprintln!("DFlow: unrecognized arguments. Run dflow --help.");
                return ExitCode::FAILURE;
            }
        }
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "DFlow needs an interactive terminal. Open it in PowerShell 7 under Windows Terminal."
        );
        return ExitCode::FAILURE;
    }
    let result = env::current_exe().and_then(|path| {
        let root = path
            .parent()
            .ok_or_else(|| io::Error::other("Cannot locate the DFlow executable directory."))?;
        run(root.join("config").join("dflow.toml"), agent_access)
    });
    if let Err(error) = result {
        eprintln!("DFlow: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
