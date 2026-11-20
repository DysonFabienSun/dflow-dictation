# DFlow Mk1

DFlow is a keyboard-first scratchpad for typing and dictating text into the Windows clipboard. Mk1 runs in PowerShell 7 under Windows Terminal and starts with an empty, session-only note. A full GUI comes later and should retain the emphasis on startup speed, responsiveness, and keyboard access.

## Terminal interface

Build a portable Windows x64 Rust console executable named `dflow.exe` from the `dflow-codex` Cargo package. Use Ratatui for rendering and native Windows console input records for physical key presses and releases. Verify Space release under PowerShell 7 in Windows Terminal before building dictation. Use the alternate screen and restore the shell's screen, cursor, and input modes when the app exits or reports a fatal error. [Windows console input](https://learn.microsoft.com/en-us/windows/console/console-input-buffer)

Take the layout cues from Codex CLI's Rust TUI. Codex uses Ratatui and Crossterm. Its composer chooses the footer state, the footer measures and renders it, and its command popup filters a list of names and descriptions. Give the note most of the screen. Reserve a compact footer for mode and shortcuts, such as `NORMAL   i insert · Space dictate · / commands`. Let recording, finishing, and actionable errors take that row when needed. Redraw on state changes and batch terminal output where supported. [Codex TUI dependencies](https://raw.githubusercontent.com/openai/codex/main/codex-rs/tui/Cargo.toml) · [Footer](https://raw.githubusercontent.com/openai/codex/main/codex-rs/tui/src/bottom_pane/footer.rs) · [Terminal rendering](https://raw.githubusercontent.com/openai/codex/main/codex-rs/tui/src/tui.rs)

In Normal mode, `/` opens the command palette above the footer. Keep the note visible and show aligned command names and descriptions. In a narrow window, show the selected description beneath the list. Filter as the user types. Arrows select, Tab completes, Enter runs, and Esc closes. [Codex command popup](https://raw.githubusercontent.com/openai/codex/main/codex-rs/tui/src/bottom_pane/command_popup.rs)

| Command | Action |
| --- | --- |
| `/clear` | Clear the note as one undoable edit. |
| `/settings` | Open the settings pane. |
| `/device` | Open the audio input picker. |
| `/provider` | Open the Codex provider settings. |
| `/help` | Show keyboard shortcuts. |
| `/quit` | Exit to PowerShell. |

## Editor, dictation, and clipboard

- Start in Normal mode. Press `i` to type at the cursor and Esc to return to Normal mode. In Normal mode, use arrows and `h`/`j`/`k` to move, `0` and `$` for line boundaries, `x` to delete a character, `dd` to delete a line, and `u` to undo. Use lowercase `l` to lowercase the entire note and Right Arrow to move right. Each editing command, Insert session, or dictation session is one undo step.
- Tap Space and release to start toggle recording. Press Space again to stop. Holding Space for one second starts push-to-talk at the threshold, and releasing stops it. Ignore key repeat for these gestures. Show the pending hold, when the microphone is live, and when transcription is finishing. Lock manual editing and cursor movement until the session ends.
- Capture the insertion point when recording starts. Prefer a quiet chunk boundary after about three seconds, force a cut by six seconds, and send remaining audio on stop. Keep one request in flight. Insert results in order at the saved cursor as they arrive, advance the insertion point, and preserve the surrounding note. Join chunks with appropriate spacing. Esc stops capture and discards pending results. Text already inserted stays in the note and one `u` undoes the session.
- Publish the complete note to the Windows clipboard after each change, including each transcript chunk, clear, and undo. Leave the clipboard alone at startup and between edits. If a write hits a temporary lock, retry only the newest note and report persistent failures.

## Settings

`/settings` lists the current device and provider. Selecting a row opens the same editor as its direct command. Extend this pane with future preferences.

`/device` shows **System default** first, followed by available microphones. Save an explicit device by stable ID. Resolve the system default at the start of each capture. If a saved device is unavailable, ask the user to choose one before recording.

`/provider` shows **Codex**, the only provider, plus the resolved auth-file path and a local status check for the ChatGPT login. Allow setting an explicit path or returning to automatic discovery.

Store settings in `config/dflow.toml` beside the distributed executable. Use `[audio]` for the optional device ID and `[transcription]` for `provider = "codex"` and the optional `codex_auth_path`. An absent file uses system-default audio and automatic auth discovery. Give new preferences defaults so existing configs keep working.

Use one typed settings model. Validate and atomically save confirmed changes while preserving unrelated TOML keys. Keep the active settings and show a footer error if saving fails. If the file is malformed, keep the scratchpad usable, show the path and parse error, and preserve the file for manual correction.

## Reference plugin and transcription backend

`ref-src/` is a local, Git-ignored copy of `KotDath/dsh-voice` v0.4.3 at commit `2d5c4c8ba8004fb7350ca0709840a6d0feee9afa`. The browser client mounts the recorder in the DeepSeek Harness composer and sends audio to the DSH host. In `ref-src/src/plugin/index.ts`, the host uses ffmpeg to convert the audio to 16 kHz mono PCM WAV. The Codex provider reads the existing ChatGPT login and sends the WAV as a multipart `file` to `https://chatgpt.com/backend-api/transcribe`, with the access token in the authorization header and a `chatgpt-account-id` header when available. It reads a single JSON `text` result per request. Its POSIX shell pipeline sits behind an explicit Windows guard. [Plugin architecture](https://github.com/KotDath/dsh-voice#architecture)

DFlow uses CPAL/WASAPI and encodes each WAV in memory. Use the same fixed HTTPS endpoint and disable redirects. Progressive text comes from repeated chunk requests. Results can lose context at boundaries, and when requests take longer than capture the remaining text can arrive during finishing. A failed chunk stops pending work, preserves inserted text, and reports the error. Keep audio, HTTP, and clipboard work off the input loop. [CPAL](https://github.com/RustAudio/cpal)

Resolve the login from `codex_auth_path` when configured, then `$CODEX_HOME/auth.json` when set, then `.codex/auth.json` in the Windows user profile. Read the file in place before each request so a renewed login takes effect. Leave login renewal to Codex CLI.

## Prototype verification

Keep automated coverage small. Use a fake backend to check that chunks appear before recording stops, insert in order at a mid-note cursor, and undo as one action. Use a temporary TOML file to check defaults, reload after save, and preservation of unrelated settings.

Check the rest manually in PowerShell 7 under Windows Terminal. Launch from a path with spaces, resize the window, and verify that quitting restores the shell. Check Space tap, hold, release and repeat, cursor locking, cancellation, and clipboard output after edits and undo. Check device selection, a missing saved device, invalid TOML, and missing login. Verify live endpoint access separately with a short opt-in recording.
