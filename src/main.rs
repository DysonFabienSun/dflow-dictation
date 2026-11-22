mod agent;
mod app;
mod cache;
#[cfg(windows)]
mod terminal_input;
mod ui;

use dflow_scratchpad::{audio, clipboard, codex, config, editor, transcript};

use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    env,
    io::{self, IsTerminal, Write},
    path::Path,
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
    input_mode: InputMode,
    bracketed_paste: bool,
}

impl Screen {
    fn enter() -> io::Result<Self> {
        #[cfg(windows)]
        let input_mode = InputMode::capture()?;
        terminal::enable_raw_mode()?;
        let mut guard = Self {
            #[cfg(windows)]
            input_mode,
            bracketed_paste: false,
        };
        execute!(io::stdout(), EnterAlternateScreen, EnableFocusChange)?;
        execute!(io::stdout(), EnableMouseCapture)?;
        #[cfg(windows)]
        if env::var_os("WT_SESSION").is_some() {
            terminal_input::enable_vt_input(guard.input_mode.handle)?;
            guard.bracketed_paste = true;
            let mut out = io::stdout().lock();
            // ConPTY normally requests Win32 keyboard reporting. Keep it active
            // in VT input mode so the terminal supplies physical key releases.
            out.write_all(b"\x1b[?9001h")?;
            execute!(out, EnableBracketedPaste)?;
        }
        #[cfg(not(windows))]
        {
            guard.bracketed_paste = true;
            execute!(io::stdout(), EnableBracketedPaste)?;
        }
        Ok(guard)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if self.bracketed_paste {
            let _ = execute!(io::stdout(), DisableBracketedPaste);
        }
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = execute!(io::stdout(), DisableFocusChange, LeaveAlternateScreen, Show);
        #[cfg(not(windows))]
        let _ = terminal::disable_raw_mode();
    }
}

fn run(path: std::path::PathBuf, agent_access: bool) -> io::Result<()> {
    let cache_path = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Cannot locate the DFlow executable directory."))?
        .join("cache")
        .join("note.json");
    let mut app = app::App::new(config::Config::load(path));
    match cache::CachedNote::load(&cache_path) {
        Ok(note) => app.editor = note.into_editor(),
        Err(_) => app.cache_error = Some("Could not restore note · check cache/note.json"),
    }
    let mut cache = match cache::NoteCache::start(cache_path, app.editor.cursor()) {
        Ok(cache) => Some(cache),
        Err(_) => {
            app.cache_error = Some("Could not start saving the note cache");
            None
        }
    };
    let result = run_terminal(&mut app, cache.as_mut(), agent_access);
    if let Some(cache) = cache
        && cache.finish(app.revision(), &app.editor).is_err()
    {
        eprintln!("DFlow: Could not save the note cache. Check cache/note.json.");
    }
    result
}

fn run_terminal(
    app: &mut app::App,
    mut cache: Option<&mut cache::NoteCache>,
    agent_access: bool,
) -> io::Result<()> {
    let screen = Screen::enter()?;
    #[cfg(windows)]
    let mut input = screen
        .bracketed_paste
        .then(|| terminal_input::TerminalInput::new(screen.input_mode.handle));
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
        if let Some(cache) = cache.as_deref_mut()
            && let Some(saved) = cache.update(app.revision(), &app.editor)
        {
            let error = (!saved).then_some("Could not save note · check cache/note.json");
            redraw |= app.cache_error != error;
            app.cache_error = error;
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, app))?;
            redraw = false;
        }
        let timeout = Duration::from_millis(20);
        #[cfg(windows)]
        let event = match &mut input {
            Some(input) => input.read(timeout)?,
            None => read_event(timeout)?,
        };
        #[cfg(not(windows))]
        let event = read_event(timeout)?;
        if let Some(event) = event {
            app.event(event);
            redraw = true;
        }
    }
    Ok(())
}

fn read_event(timeout: Duration) -> io::Result<Option<event::Event>> {
    if event::poll(timeout)? {
        return event::read().map(Some);
    }
    Ok(None)
}

fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let mut arguments = env::args_os().skip(1);
    let mut agent_access = true;
    if let Some(first) = arguments.next() {
        match first.to_str() {
            Some("--help" | "-h") if arguments.next().is_none() => {
                println!(
                    "DFlow Scratchpad\n\nUsage: dflow [--no-agent-access]\n       dflow --mcp\n\nOpen DFlow Scratchpad in PowerShell 7 under Windows Terminal.\nClick to place the caret or drag to select text.\nCtrl+Space dictates and Esc opens commands.\nCtrl+Z undoes, Ctrl+Y redoes, Ctrl+C copies, and Ctrl+X cuts.\nUse :q or /quit from the command menu to exit."
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
    let result = config::settings_path().and_then(|path| run(path, agent_access));
    if let Err(error) = result {
        eprintln!("DFlow: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
