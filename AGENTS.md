# DFlow Project Guidelines

## Purpose and Scope

DFlow is a Windows-first, keyboard-first dictation scratchpad. Prioritize fast startup and responsive editing in PowerShell 7 under Windows Terminal. The Windows clipboard is the output. Read [the project spec](docs/DFLOW.md) before working on the app.

## Project Guidelines

- Use the Codex transcription backend documented in the spec. Read the Codex login in place and keep credentials out of DFlow settings, logs, and error messages.
- Store DFlow settings under `config/`. Preserve `ref-src/` as a local, Git-ignored upstream reference.
- Keep editor, audio, transcription, settings, and clipboard logic independent of the terminal UI so a future GUI can reuse them.
- Keep automated tests minimal. Add focused coverage for behavior that is hard to verify manually or likely to regress. Use targeted manual checks for terminal and device integration.
