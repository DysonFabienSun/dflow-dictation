mod agent;
mod app;
mod cache;
mod ui;

use dflow_scratchpad::{audio, clipboard, codex, config, editor, transcript};

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
                    "DFlow · Dictation scratchpad\n\nUsage: dflow [--no-agent-access]\n       dflow --mcp\n\nOpen DFlow in PowerShell 7 under Windows Terminal.\nCtrl+Space dictates and Esc opens commands.\nCtrl+Z undoes, Ctrl+Y redoes, Ctrl+C copies, and Ctrl+X cuts.\nUse :q or /quit from the command menu to exit."
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
