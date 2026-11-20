# DFlow

<img src="assets/dflow.svg" alt="DFlow icon" width="72">

DFlow is a keyboard-first scratchpad for typing or dictating text and copying it to the Windows clipboard. Each window starts with an empty note that lasts until you close the window.

## Get started

Download `dflow.exe` from [Releases](https://github.com/DysonFabienSun/dflow-scratchpad/releases). Put it in a writable folder and run `.\dflow.exe` in PowerShell 7 under Windows Terminal. Dictation uses your existing Codex CLI login.

To build from source on Windows, run these commands from the repository root with Rust and Cargo installed.

```powershell
cargo build --release --locked
.\target\release\dflow.exe
```

## Edit, dictate, and copy

| Shortcut | Action |
| --- | --- |
| **Ctrl+Space** | Tap to start dictation and tap again to stop. Hold through the threshold for push-to-talk, then release to stop. |
| **Esc** | Close a menu or cancel dictation. Text already inserted stays in the note. |
| **Ctrl+Z** | Undo the last edit. A dictation session or agent edit is one undo step. |
| **Ctrl+C** | Copy the whole note. With a Windows Terminal selection, Ctrl+C copies the selection instead. |
| **Ctrl+X** | Copy the note, then clear it if the clipboard write succeeds and the note is still current. Undo restores the cut. |
| **Ctrl+/** | Open the command menu. Type to filter, use arrows to select, Tab to complete, and Enter to run. |
| **Ctrl+W** | Quit. |

Type directly in the note. Dictation inserts text at your caret as results arrive, and editing resumes when dictation finishes. Only an explicit copy or cut writes to the clipboard. A successful write briefly shows **Copied 123 chars** at the bottom right.

Use `/clear` to clear the note, `/lowercase` to lowercase it, and `/help` to see shortcuts.

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
