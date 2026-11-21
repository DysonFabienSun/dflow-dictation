# DFlow

<img src="assets/dflow.svg" alt="DFlow icon" width="72">

DFlow is a keyboard-first scratchpad for typing or dictating text and copying it to the Windows clipboard. It restores the last saved note and caret when you reopen it.

## Get started

Download the Windows ZIP from [Releases](https://github.com/DysonFabienSun/dflow-scratchpad/releases) and extract it into a writable folder. It contains the DFlow scratchpad (`dflow.exe`) and DFlow Tray (`dflow-tray.exe`). You can also download either executable individually.

Run `.\dflow.exe` in PowerShell 7 under Windows Terminal for the scratchpad, or launch `dflow-tray.exe` for dictation into other apps. Both use your existing Codex CLI login for dictation. When upgrading, close both apps before replacing the executables and keep the `config` and `cache` folders.

To build from source on Windows, run these commands from the repository root with Rust and Cargo installed.

```powershell
cargo build --release --locked --bins
.\target\release\dflow.exe
```

## Dictate into other apps

Run `dflow-tray.exe` to start DFlow Tray without a terminal window. Put the caret in the app where you want to type, press **Ctrl+Shift+/** to start dictation, and press it again to finish. DFlow Tray can insert single-line text directly into the original supported Windows edit control if focus stays there. Use **Copy last transcript** from the tray menu for other apps, after a focus change, or for transcripts with line breaks. DFlow Tray keeps the latest result in memory for the current session.

Open **Settings** from the tray icon to change the shortcut, microphone, Codex auth file, or proxy, or to enable **Start with Windows**. The default shortcut uses the slash/question-mark key on a US keyboard. Each executable reads `config/dflow.toml` beside itself. Put both executables in one folder to share settings, or use separate folders for independent settings. DFlow Tray reads shared audio and provider settings before each recording. Restart the scratchpad after changes made by the tray.

The scratchpad note and tray transcript are separate. To move a tray transcript into the scratchpad, choose **Copy last transcript** and paste it into the note.

## Edit, dictate, and copy

| Shortcut | Action |
| --- | --- |
| **Ctrl+Space** | Tap to start dictation and tap again to stop. Hold through the threshold for push-to-talk, then release to stop. |
| **Esc** | Open the command menu from the note, close a menu, or cancel dictation. Text already inserted stays in the note. |
| **Ctrl+A** | Select the whole note. |
| **Shift+arrows**, **Shift+Home/End** | Extend or shrink the selection. |
| **Ctrl+Shift+Left/Right**, **Ctrl+Shift+Home/End** | Select by word or to the start or end of the note. |
| **Ctrl+Z**, **Ctrl+Y** | Undo or redo an edit. |
| **Ctrl+C** | Copy the selection, or the whole note if there is no selection. Windows Terminal also handles its own selections. |
| **Ctrl+X** | Cut the selection, or the whole note if there is no selection. DFlow removes the text after the clipboard write succeeds if the note is still current. Undo restores the cut. |
| **Ctrl+/** | Open the command menu. Type to filter, use arrows to select, Tab to complete, and Enter to run. |

Type directly in the note. Typing replaces a selection, and Backspace or Delete removes it. Dictation inserts text at your caret as results arrive, and editing resumes when dictation finishes. Only an explicit copy or cut writes to the clipboard. A successful write briefly shows **Copied 123 chars** at the bottom right.

Undo groups consecutive typing, Backspace, or Delete until you pause for 750 ms, move the caret, change the selection, or switch actions. A line break, cut, dictation session, agent edit, or `/clear` is its own step. A paste delivered as one event is also one step. Windows Terminal can deliver multiline paste as keystrokes, so the line breaks can split it into several steps. A new edit clears redo history.

Use `/clear` to clear the note, `/lowercase` to lowercase it, and `/help` to see shortcuts. The menu opens with a blank input on **Esc** and a `/` prefix on **Ctrl+/**. In either menu, type the command name or use `:h` for help, `:u` to undo, and `:q` to quit. Press **Enter** to run or **Esc** to close.

DFlow saves the note in `cache/note.json` beside the `config` folder as you edit. The cache contains plain text, and `/clear` saves an empty note. Windows launched from the same folder share one cache, so the last saved edit supplies the note on next launch. Undo and redo history start fresh each time.

## Settings

DFlow creates `config/dflow.toml` beside the executable when you save a setting. If the file changes while DFlow is open, restart DFlow before saving settings. Copy the executable by itself into another writable folder to start with separate settings.

- **`/device`** chooses a microphone. DFlow uses the current Windows default unless you select a specific device.
- **`/provider`** shows whether DFlow can find your Codex login. Set an auth-file path there if needed. Automatic discovery uses `$CODEX_HOME/auth.json` when `CODEX_HOME` is set, or `%USERPROFILE%\.codex\auth.json` otherwise.
- **`/proxy`** configures an optional proxy for dictation. It starts disabled with a saved address of `http://127.0.0.1:10808`. Use `/proxyon` or `/proxyoff` to switch for the next dictation. Supported URLs use `http://`, `https://`, or `socks5://` with a host and optional port.

Open all three from `/settings`. The push-to-talk threshold defaults to one second. To shorten it, set `hold_delay_ms = 250` under `[audio]` in the config file and restart DFlow.

## Connect a local agent

Open DFlow normally and configure your local Windows MCP client to launch the same executable with `--mcp` over stdio. In PowerShell with Codex CLI, use the absolute path to your copy of DFlow.

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

Start a new agent session and ask it to append text to DFlow. The tools can list windows, read the note, append or insert text, replace the note, and explicitly copy it. With multiple windows open, pass the chosen window ID. Replacement uses the revision returned by a read. Each edit is undoable. Edit tools return a busy result while a pane is open or dictation is active or finishing.

Agent access is local to your Windows logon session. Start DFlow with `--no-agent-access` to disable it for that window.
