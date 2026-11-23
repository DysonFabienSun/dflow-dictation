# DFlow

**English** · [简体中文](README.zh-CN.md) · [Changelog](https://github.com/DysonFabienSun/dflow-dictation/releases)

<img src="assets/dflow.svg" alt="DFlow icon" width="72">

**ChatGPT-powered dictation for Windows using the subscription you already have.** DFlow reuses your ChatGPT sign-in from Codex CLI, including accounts on ChatGPT Plus or Pro. There's no separate DFlow subscription or OpenAI API key to set up.

Use a global hotkey to dictate from the tray, or capture, edit, and save text in the scratchpad. Both apps send audio to ChatGPT's speech-to-text service and can refine earlier wording as more of the sentence comes in.

## Install or upgrade on Windows

Download either or both executables under **Assets** and save them to a writable folder. Choose **DFlow Dictation** (`dflow-dictation.exe`) to dictate to the clipboard or **DFlow Scratchpad** (`dflow.exe`) to dictate into a saved note. [GitHub Releases](https://github.com/DysonFabienSun/dflow-dictation/releases)

For cloud dictation, install Codex CLI, run `codex login` with your ChatGPT account, and allow Windows microphone access. DFlow uses the existing file-backed Codex sign-in. Keep a current Codex CLI on PATH to use automatic cleanup. [OpenAI's Codex sign-in guide](https://learn.chatgpt.com/docs/auth)

When upgrading, close both apps before replacing the executables. Keep the `config` and `cache` folders to preserve your settings and saved note. If you used `dflow-tray.exe`, launch `dflow-dictation.exe` instead and re-enable **Start with Windows** in its Settings if you used that option.

## Quick start

On the first interactive launch, including an upgrade from a version without consent settings, choose **Enable cloud dictation** or **Continue without dictation**. Enabling uses your existing Codex sign-in and sends microphone audio to OpenAI at `chatgpt.com`. You can use the scratchpad for local editing after declining.

In **Dictation setup**, check the sign-in, select a microphone, and configure a proxy if needed. The proxy starts disabled. Select **Test dictation**, speak, and stop the test to see its transcript in setup. You can skip the test. Its transcript stays out of the note and clipboard.

Before enabling dictation, use **Check ChatGPT availability** to check the connection to `chatgpt.com` and whether DFlow can find Codex CLI. **Check proxy** tests the entered address even when the proxy is off. Use **Test dictation** after enabling to verify account access.

### DFlow Dictation (`dflow-dictation.exe`)

1. Launch `dflow-dictation.exe`. With dictation enabled, the tray app shows a seven-second shortcut guide. Select **Dismiss** to close it early, or turn off **Show startup tip** in **Settings**. Use the tray icon to open **Settings** or choose **Copy last transcript**.
2. Press **Ctrl+Shift+/** to start dictation. The default shortcut uses the slash/question-mark key on a US keyboard. The live preview appears as soon as words are ready and grows to fit up to three lines.
3. Press the shortcut again within two seconds to discard the recording. After two seconds, press it to stop and copy the full transcript when it’s ready. You can also click **Cancel** or **Stop & Copy** above the live preview. Use **Cancel recording** in the tray menu to discard at any time.

Paste the transcript into any app with **Ctrl+V**. In **Settings**, you can change the shortcut or enable **Start with Windows**.

### DFlow Scratchpad (`dflow.exe`)

Run `.\dflow.exe` in PowerShell 7 under Windows Terminal. It works like Notepad and saves your note as you edit. Tap **Ctrl+Space** to start or stop dictation.

Press **Esc** or **Ctrl+/** to open commands. Use `/help` for shortcuts or `/clear` to clear the note.

DFlow underlines the latest dictation, paste, or text supplied by an agent. The first **/** you type while a new underline is active opens commands like **Ctrl+/**. Use `/lower` (short for `/lowercase`) on that text or select text yourself to lower it instead. Selecting text or changing the note clears the previous underline. Words with consecutive uppercase letters, such as NASA and OpenAI, keep their original case.

## Settings

Keep both executables in one folder to share consent, microphone, Codex auth, proxy, and cleanup settings. DFlow creates `config/dflow.toml` beside the executable when you save a setting. Use separate folders for independent settings.

Reopen **Dictation setup** from the tray menu or the scratchpad's `/settings` menu to change consent or run another test. Both apps read the shared dictation settings before recording. Reopen settings after a change in another app.

Use **Interface language** in either app’s settings to follow the system language or select English or Simplified Chinese. The preference takes effect when you restart an app. The config file accepts `auto`, `en`, and `zh-CN` for `language` under `[ui]`.

- **`/device`** chooses a microphone. DFlow uses the current Windows default unless you select a specific device.
- **`/provider`** shows whether DFlow can find your Codex login. Set an auth-file path there if needed. Automatic discovery uses `$CODEX_HOME/auth.json` when `CODEX_HOME` is set, or `%USERPROFILE%\.codex\auth.json` otherwise.
- **`/proxy`** configures an optional proxy for dictation. It starts disabled with a saved address of `http://127.0.0.1:10808`. Use `/proxyon` or `/proxyoff` to switch for the next dictation. Supported URLs use `http://`, `https://`, or `socks5://` with a host and optional port.

The scratchpad's push-to-talk threshold defaults to one second. To shorten it, set `hold_delay_ms = 250` under `[audio]` in the config file and restart the scratchpad.

## ChatGPT Voice Dictionary and cleanup

Open **ChatGPT Voice Dictionary** in Dictation setup to view, add, or remove canonical words and phrases. These edits also affect dictation in ChatGPT. The service supplies the entry count and length limits.

**Clean up after dictation** starts off. When enabled, DFlow sends the completed dictated span and account vocabulary to GPT-6 Luna through Codex App Server. The scratchpad keeps surrounding note text local and groups capture and cleanup into one undo step.

Each cleanup runs one GPT-6 Luna request and uses additional tokens from your ChatGPT plan’s Codex allowance. The amount varies with the transcript and vocabulary. Applicable credits or billing may apply. [Codex usage and billing](https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan)

While **Cleaning up** is visible, press **Esc** in the scratchpad or select **Skip cleanup** in the tray to keep the raw transcript. DFlow also keeps the raw transcript if cleanup fails or reaches its 12-second deadline. The tray copies the final result once.

Transcription and Voice Dictionary management use private ChatGPT endpoints. Availability depends on ChatGPT account access, and endpoint or authentication changes can interrupt these features.

## Connect a local agent

Open DFlow normally and configure your local Windows MCP client to launch the same executable with `--mcp` over stdio. In PowerShell with Codex CLI, use the absolute path to your copy of DFlow.

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

Start a new agent session and ask it to append text to DFlow or insert text below a line such as `## Next steps`. The `insert_after_line` tool takes the exact text of a unique line, the new text, and the revision from `read_note`. The tools can also list windows, insert at the caret, replace the note, and explicitly copy it. With multiple windows open, pass the chosen window ID. Replacing the note also requires its revision. Each edit is undoable. Edit tools return a busy result while a pane is open or dictation is active or finishing.

Agent access is local to your Windows logon session. Start DFlow with `--no-agent-access` to disable it for that window.

## Build from source

With Rust and Cargo installed on Windows, run this from the repository root.

```powershell
cargo build --release --locked --bins
```

Launch `target\release\dflow-dictation.exe` to dictate to the clipboard or run `.\target\release\dflow.exe` in Windows Terminal for the scratchpad.

For maintainers, see the [release guide](.github/RELEASING.md).
