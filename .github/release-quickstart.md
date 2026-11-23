## Install or upgrade on Windows

Download either or both executables under **Assets** and save them to a writable folder. To upgrade, close both apps before replacing the executables in your existing folder. Keep the `config` and `cache` folders to retain your settings and scratchpad note.

- **DFlow Scratchpad (`dflow.exe`)** opens a saved note in PowerShell 7 under Windows Terminal. Click to place the caret, drag to select text, press **Ctrl+Space** to dictate, and press **Ctrl+C** to copy.
- **DFlow Dictation (`dflow-dictation.exe`)** runs in the Windows notification area and dictates to the clipboard. Press **Ctrl+Shift+/** to start recording and see a live preview as words arrive. Press the shortcut again within two seconds to discard, or after two seconds to stop and copy the final transcript.

If you used `dflow-tray.exe`, launch `dflow-dictation.exe` instead and re-enable **Start with Windows** in its Settings if you used that option.

## Set up cloud dictation

Install Codex CLI, run `codex login` with your ChatGPT account, and allow Windows microphone access. On first launch, choose **Enable cloud dictation** to use that sign-in and send audio to OpenAI at `chatgpt.com`. Choose **Continue without dictation** to use the scratchpad for local editing.

Open **Dictation setup** to check sign-in, choose a microphone, and run an optional test recording. The proxy starts disabled. Use **ChatGPT Voice Dictionary** to add or remove entries for your account. Changes also affect dictation in ChatGPT.

**Clean up after dictation** is off by default and needs a current Codex CLI on PATH. Each cleanup runs one GPT-6 Luna request and uses additional tokens from your ChatGPT plan’s Codex allowance. The amount varies with the transcript and vocabulary. Applicable credits or billing may apply. [Codex usage and billing](https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan)

Transcription and Voice Dictionary management depend on private ChatGPT endpoints and account access.

## Use the two apps together

Both apps can run independently. Keep the executables in the same folder to share consent, microphone, Codex login path, proxy, and cleanup settings through `config/dflow.toml`. Both apps read the shared settings before each recording.

The scratchpad note and tray transcript are separate. To move a tray transcript into the scratchpad, use **Copy last transcript** and paste it into the note.
