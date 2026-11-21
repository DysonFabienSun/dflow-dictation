## Install or upgrade on Windows

Download the Windows ZIP under **Assets** and extract it into a writable folder. Each app is also available as a separate download. To upgrade, close both apps before replacing the executables in your existing folder. Keep the `config` and `cache` folders to retain your settings and scratchpad note.

- **DFlow Scratchpad** opens when you run `.\dflow.exe` in PowerShell 7 under Windows Terminal. Press **Ctrl+Space** to dictate into the note and **Ctrl+C** to copy it. The scratchpad saves the note and caret between launches.
- **DFlow Tray (`dflow-tray.exe`)** runs in the Windows notification area. Place the caret in the app you want to dictate into and press **Ctrl+Shift+/** to start and finish recording. The tray can insert single-line text directly into supported Windows edit controls. For other apps, multiline results, or a focus change, choose **Copy last transcript** from the tray menu and paste it where you want it.

## Use the two apps together

Both apps use your existing Codex CLI login for dictation and can run independently. Keep the executables in the same folder to share microphone, Codex auth, and proxy settings through `config/dflow.toml`. Separate folders keep settings independent. The tray reads the shared dictation settings before each recording. Restart the scratchpad after changing shared settings in the tray.

The scratchpad note and tray transcript are separate. To move a tray transcript into the scratchpad, use **Copy last transcript** and paste it into the note.
