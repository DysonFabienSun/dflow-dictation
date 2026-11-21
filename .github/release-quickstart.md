## Install or upgrade on Windows

Download either or both executables under **Assets** and save them to a writable folder. To upgrade, close both apps before replacing the executables in your existing folder. Keep the `config` and `cache` folders to retain your settings and scratchpad note.

- **DFlow Scratchpad (`dflow.exe`)** opens a saved note in PowerShell 7 under Windows Terminal. Click to place the caret, drag to select text, press **Ctrl+Space** to dictate, and press **Ctrl+C** to copy.
- **DFlow Dictation (`dflow-dictation.exe`)** runs in the Windows notification area and dictates to the clipboard. Press **Ctrl+Shift+/** to start recording and see a live preview as words arrive. Press the shortcut again within two seconds to discard, or after two seconds to stop and copy the final transcript.

If you used `dflow-tray.exe`, launch `dflow-dictation.exe` instead and re-enable **Start with Windows** in its Settings if you used that option.

## Use the two apps together

Both apps use your existing Codex CLI login for dictation and can run independently. Keep the executables in the same folder to share microphone, Codex auth, and proxy settings through `config/dflow.toml`. Separate folders keep settings independent. The tray reads the shared dictation settings before each recording. Restart the scratchpad after changing shared settings in the tray.

The scratchpad note and tray transcript are separate. To move a tray transcript into the scratchpad, use **Copy last transcript** and paste it into the note.
