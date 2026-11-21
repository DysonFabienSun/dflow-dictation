# DFlow

<img src="assets/dflow.svg" alt="DFlow icon" width="72">

**ChatGPT-powered dictation for Windows using the subscription you already have.** DFlow reuses your ChatGPT sign-in from Codex CLI, including accounts on ChatGPT Plus or Pro. There's no separate DFlow subscription or OpenAI API key to set up.

Use a global hotkey to dictate from the tray, or capture, edit, and save text in the scratchpad. Both apps send audio to ChatGPT's speech-to-text service and can refine earlier wording as more of the sentence comes in.

## Install on Windows

Download either or both executables under **Assets** and save them to a writable folder. Choose **DFlow Dictation** (`dflow-dictation.exe`) to dictate to the clipboard or **DFlow Scratchpad** (`dflow.exe`) to dictate into a saved note. [GitHub Releases](https://github.com/DysonFabienSun/dflow-dictation/releases)

Already signed in to Codex CLI with ChatGPT on this PC? DFlow uses that login. To sign in for the first time or renew the session, run `codex login` and complete the browser sign-in with your ChatGPT account. [OpenAI's Codex sign-in guide](https://learn.chatgpt.com/docs/auth)

When upgrading, close both apps before replacing the executables. Keep the `config` and `cache` folders to preserve your settings and saved note. If you used `dflow-tray.exe`, launch `dflow-dictation.exe` instead and re-enable **Start with Windows** in its Settings if you used that option.

## Quick start

### DFlow Dictation (`dflow-dictation.exe`)

1. Launch `dflow-dictation.exe`. It starts in the Windows system tray without opening a main window. Use the tray icon to open **Settings** or choose **Copy last transcript**.
2. Press **Ctrl+Shift+/** to start dictation. The default shortcut uses the slash/question-mark key on a US keyboard. The live preview appears as soon as words are ready and grows to fit up to three lines.
3. Press the shortcut again within two seconds to discard the recording. After two seconds, press it to stop and copy the full transcript when it’s ready. You can also click **Cancel** or **Stop & Copy** above the live preview. Use **Cancel recording** in the tray menu to discard at any time.

Paste the transcript into any app with **Ctrl+V**. In **Settings**, you can change the shortcut or enable **Start with Windows**.

### DFlow Scratchpad (`dflow.exe`)

Run `.\dflow.exe` in PowerShell 7 under Windows Terminal. It works like Notepad and saves your note as you edit. Tap **Ctrl+Space** to start or stop dictation.

Press **Esc** or **Ctrl+/** to open commands. Use `/help` for shortcuts, `/clear` to clear the note, or `/lower` (short for `/lowercase`) to lowercase it.

## Settings

Keep both executables in one folder to share microphone, Codex auth, and proxy settings. DFlow creates `config/dflow.toml` beside the executable when you save a setting. Use separate folders for independent settings.

Change shared settings in the tray's **Settings** dialog or the scratchpad's `/settings` menu. The tray reads the shared dictation settings before each recording. Restart the scratchpad after changing shared settings in the tray or editing the file directly.

- **`/device`** chooses a microphone. DFlow uses the current Windows default unless you select a specific device.
- **`/provider`** shows whether DFlow can find your Codex login. Set an auth-file path there if needed. Automatic discovery uses `$CODEX_HOME/auth.json` when `CODEX_HOME` is set, or `%USERPROFILE%\.codex\auth.json` otherwise.
- **`/proxy`** configures an optional proxy for dictation. It starts disabled with a saved address of `http://127.0.0.1:10808`. Use `/proxyon` or `/proxyoff` to switch for the next dictation. Supported URLs use `http://`, `https://`, or `socks5://` with a host and optional port.

The scratchpad's push-to-talk threshold defaults to one second. To shorten it, set `hold_delay_ms = 250` under `[audio]` in the config file and restart the scratchpad.

## Connect a local agent

Open DFlow normally and configure your local Windows MCP client to launch the same executable with `--mcp` over stdio. In PowerShell with Codex CLI, use the absolute path to your copy of DFlow.

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

Start a new agent session and ask it to append text to DFlow. The tools can list windows, read the note, append or insert text, replace the note, and explicitly copy it. With multiple windows open, pass the chosen window ID. Replacement uses the revision returned by a read. Each edit is undoable. Edit tools return a busy result while a pane is open or dictation is active or finishing.

Agent access is local to your Windows logon session. Start DFlow with `--no-agent-access` to disable it for that window.

## Build from source

With Rust and Cargo installed on Windows, run this from the repository root.

```powershell
cargo build --release --locked --bins
```

Launch `target\release\dflow-dictation.exe` to dictate to the clipboard or run `.\target\release\dflow.exe` in Windows Terminal for the scratchpad.
