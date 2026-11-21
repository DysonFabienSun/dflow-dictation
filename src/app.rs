use crate::{
    agent::{AgentResult, Command, Error, Reply},
    audio::{self, InputDevice, Session, Update},
    clipboard::{Clipboard, ClipboardFeedback},
    codex::login_status,
    config::{Config, ProxySettings, parse_proxy_url},
    editor::{BurstKind, Editor, Movement},
    transcript::clean_text,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use std::{
    ops::Range,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type DeviceList = Result<Vec<InputDevice>, String>;

#[derive(Clone, Debug)]
enum Action {
    Clear,
    Lowercase,
    Undo,
    Redo,
    Settings,
    Devices,
    Provider,
    Proxy,
    SetProxyEnabled(bool),
    EditProxyUrl,
    SaveProxyUrl(String),
    Help,
    Quit,
    SelectDevice(Option<String>),
    EditAuthPath,
    ResetAuthPath,
    SaveAuthPath(String),
    Back,
}

pub struct Row {
    pub name: String,
    pub detail: String,
    action: Action,
}

impl Row {
    fn new(name: impl Into<String>, detail: impl Into<String>, action: Action) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
            action,
        }
    }
}

pub struct Menu {
    pub title: &'static str,
    pub context: Vec<String>,
    pub query: Option<CommandQuery>,
    pub rows: Vec<Row>,
    pub selected: usize,
}

pub struct CommandQuery {
    text: String,
    slash_prompt: bool,
}

impl CommandQuery {
    pub fn display(&self) -> String {
        if self.slash_prompt && !self.text.starts_with(['/', ':']) {
            format!("/{}", self.text)
        } else {
            self.text.clone()
        }
    }
}

impl Menu {
    pub fn visible(&self) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|row| {
                self.query.as_ref().is_none_or(|query| {
                    let text = query.text.strip_prefix('/').unwrap_or(&query.text);
                    row.name.trim_start_matches('/').starts_with(text)
                })
            })
            .collect()
    }

    fn key(&mut self, key: KeyCode) -> Option<Action> {
        match key {
            KeyCode::Esc => return Some(Action::Back),
            KeyCode::Enter => {
                return self
                    .visible()
                    .get(self.selected)
                    .map(|row| row.action.clone());
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
            }
            KeyCode::Tab if self.query.is_some() => {
                if let Some(row) = self.visible().get(self.selected) {
                    let text = row.name.trim_start_matches('/').to_owned();
                    self.query.as_mut().unwrap().text = text;
                    self.selected = 0;
                }
            }
            KeyCode::Backspace if self.query.is_some() => {
                self.query.as_mut().unwrap().text.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) if !c.is_control() => {
                if let Some(query) = &mut self.query {
                    query.text.push(c.to_ascii_lowercase());
                    self.selected = 0;
                }
            }
            _ => {}
        }
        None
    }
}

pub enum Pane {
    Menu(Menu),
    TextInput { kind: InputKind, text: String },
    Help,
}

pub enum InputKind {
    AuthPath,
    ProxyUrl,
}

impl InputKind {
    pub fn labels(&self) -> (&'static str, &'static str) {
        match self {
            Self::AuthPath => ("Codex auth file", "Absolute path"),
            Self::ProxyUrl => ("Proxy address", "http://, https:// or socks5://"),
        }
    }
}

impl Pane {
    fn key(&mut self, key: KeyCode) -> Option<Action> {
        match self {
            Self::Menu(menu) => menu.key(key),
            Self::Help => matches!(key, KeyCode::Esc | KeyCode::Enter).then_some(Action::Back),
            Self::TextInput { kind, text } => match key {
                KeyCode::Esc => Some(Action::Back),
                KeyCode::Enter => Some(match kind {
                    InputKind::AuthPath => Action::SaveAuthPath(text.clone()),
                    InputKind::ProxyUrl => Action::SaveProxyUrl(text.clone()),
                }),
                KeyCode::Backspace => {
                    text.pop();
                    None
                }
                KeyCode::Char(c) if !c.is_control() => {
                    text.push(c);
                    None
                }
                _ => None,
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationPhase {
    Pending,
    Live { quiet: bool },
    Finishing,
}

struct Dictation {
    session: Session,
    gesture: SpaceGesture,
    phase: DictationPhase,
    range: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpaceGesture {
    Pressing(Instant),
    Hold,
    Toggle,
}

impl SpaceGesture {
    fn advance(&mut self, now: Instant, delay: Duration) -> bool {
        if matches!(*self, Self::Pressing(pressed) if now.duration_since(pressed) >= delay) {
            *self = Self::Hold;
            return true;
        }
        false
    }

    fn release(&mut self, now: Instant, delay: Duration) -> bool {
        self.advance(now, delay);
        if *self == Self::Hold {
            return true;
        }
        *self = Self::Toggle;
        false
    }
}

pub struct App {
    pub editor: Editor,
    pub panes: Vec<Pane>,
    pub top_row: usize,
    pub note_area: Rect,
    pub quit: bool,
    config: Config,
    clipboard: Clipboard,
    pending_cut: Option<PendingCut>,
    device_scan: Option<Receiver<DeviceList>>,
    dictation: Option<Dictation>,
    space_down: bool,
    mouse_selecting: bool,
    notice: Option<String>,
    copied: Option<(usize, Instant)>,
    revision: u64,
    agent_error: Option<String>,
    pub cache_error: Option<&'static str>,
}

struct PendingCut {
    revision: u64,
    range: Range<usize>,
    reply: oneshot::Receiver<ClipboardFeedback>,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            editor: Editor::default(),
            panes: Vec::new(),
            top_row: 0,
            note_area: Rect::ZERO,
            quit: false,
            config,
            clipboard: Clipboard::new(),
            pending_cut: None,
            device_scan: None,
            dictation: None,
            space_down: false,
            mouse_selecting: false,
            notice: None,
            copied: None,
            revision: 0,
            agent_error: None,
            cache_error: None,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn status(&self) -> (&str, bool) {
        if let Some(dictation) = &self.dictation {
            return match dictation.phase {
                DictationPhase::Pending => ("", false),
                DictationPhase::Live { quiet: true } => {
                    ("Mic input is quiet · check /device or input gain", true)
                }
                DictationPhase::Live { quiet: false }
                    if dictation.gesture == SpaceGesture::Hold =>
                {
                    ("● Recording · release Space to stop · Esc cancel", false)
                }
                DictationPhase::Live { quiet: false }
                    if matches!(dictation.gesture, SpaceGesture::Pressing(_)) =>
                {
                    (
                        "● Recording · release to keep recording · hold for push-to-talk",
                        false,
                    )
                }
                DictationPhase::Live { quiet: false } => {
                    ("● Recording · Ctrl+Space stop · Esc cancel", false)
                }
                DictationPhase::Finishing => ("Finishing transcription… · Esc cancel", false),
            };
        }
        if let Some(notice) = &self.notice {
            return (notice, true);
        }
        if let Some(error) = &self.config.error {
            return (error, true);
        }
        if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.query.is_some()) {
            return ("↑↓ select · Tab complete · Enter run · Esc close", false);
        }
        if !self.panes.is_empty() {
            return ("↑↓ select · Enter confirm · Esc back", false);
        }
        if let Some(error) = self.cache_error {
            return (error, true);
        }
        if let Some(error) = &self.agent_error {
            return (error, true);
        }
        ("Ctrl+Space dictate · Esc commands · Ctrl+Z undo", false)
    }

    pub fn copied_characters(&self) -> Option<usize> {
        self.copied.map(|(count, _)| count)
    }

    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let mut redraw = self.poll_devices();
        if let Some(cut) = &mut self.pending_cut {
            match cut.reply.try_recv() {
                Ok(feedback) => {
                    if matches!(feedback, ClipboardFeedback::Copied(_))
                        && cut.revision == self.revision
                        && self.dictation.is_none()
                    {
                        let range = cut.range.clone();
                        let changed = self.editor.edit(|editor| editor.erase(range));
                        self.note_changed(changed);
                        redraw |= changed;
                    }
                    self.pending_cut = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.pending_cut = None;
                    self.notice = Some("Could not update the Windows clipboard.".into());
                    redraw = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.copied.is_some_and(|(_, until)| now >= until) {
            self.copied = None;
            redraw = true;
        }
        while let Ok(feedback) = self.clipboard.feedback.try_recv() {
            match feedback {
                ClipboardFeedback::Copied(count) => {
                    self.copied = Some((count, now + Duration::from_millis(1200)))
                }
                ClipboardFeedback::Failed => {
                    self.notice = Some("Could not update the Windows clipboard.".into())
                }
            }
            redraw = true;
        }
        let delay = self.hold_delay();
        if let Some(dictation) = self.dictation.as_mut() {
            redraw |= dictation.gesture.advance(Instant::now(), delay);
        }
        loop {
            let Some(dictation) = self.dictation.as_mut() else {
                break;
            };
            let update = match dictation.session.updates.try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    Update::Failed("Dictation ended unexpectedly.".into())
                }
            };
            redraw = true;
            match update {
                Update::Live if dictation.phase == DictationPhase::Pending => {
                    dictation.phase = DictationPhase::Live { quiet: false };
                }
                Update::Live => {}
                Update::QuietInput(quiet) => {
                    if matches!(dictation.phase, DictationPhase::Live { .. }) {
                        dictation.phase = DictationPhase::Live { quiet };
                    }
                }
                Update::Finishing => dictation.phase = DictationPhase::Finishing,
                Update::Transcript(text) => {
                    let changed = self.editor.transcript(&mut dictation.range, &text);
                    self.note_changed(changed);
                }
                Update::Done => self.end(),
                Update::Failed(error) => {
                    self.end();
                    self.notice = Some(error);
                }
            }
        }
        redraw
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Release {
                    self.mouse_selecting = false;
                }
                self.key(key);
            }
            Event::Mouse(mouse) => self.mouse(mouse),
            Event::Paste(text) if self.dictation.is_none() => {
                self.mouse_selecting = false;
                let text = clean_text(&text);
                if let Some(Pane::TextInput { text: input, .. }) = self.panes.last_mut() {
                    if !text.contains('\n') {
                        input.push_str(&text);
                    }
                } else if self.panes.is_empty() {
                    let changed = self.editor.edit(|editor| editor.insert(&text));
                    self.note_changed(changed);
                }
            }
            Event::FocusLost => {
                self.mouse_selecting = false;
                self.editor.end_burst();
                self.space_down = false;
                if self.dictation.as_ref().is_some_and(|d| {
                    d.gesture != SpaceGesture::Toggle && d.phase != DictationPhase::Finishing
                }) {
                    self.stop();
                }
            }
            _ => {}
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let area = self.note_area;
        if area.is_empty() || self.dictation.is_some() || !self.panes.is_empty() {
            self.mouse_selecting = false;
            return;
        }
        let extend = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_selecting = area.contains(Position::new(mouse.column, mouse.row));
                if !self.mouse_selecting {
                    return;
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) if self.mouse_selecting => true,
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse_selecting = false;
                return;
            }
            _ => return,
        };
        let row = self.top_row + usize::from(mouse.row.saturating_sub(area.y));
        let column = usize::from(mouse.column.saturating_sub(area.x));
        self.notice = None;
        self.editor
            .point_caret(row, column, usize::from(area.width), extend);
    }

    fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char(' ') && key.kind == KeyEventKind::Release {
            self.space_down = false;
            let delay = self.hold_delay();
            if self
                .dictation
                .as_mut()
                .is_some_and(|d| d.gesture.release(Instant::now(), delay))
            {
                self.stop();
            }
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Esc && key.kind == KeyEventKind::Repeat {
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        if control {
            match key.code {
                KeyCode::Char('c') => {
                    if !self.editor.text().is_empty() {
                        self.copy_shortcut(false);
                    }
                    return;
                }
                KeyCode::Char('x') => {
                    if self.pending_cut.is_none() && !self.editor.text().is_empty() {
                        self.end();
                        self.editor.end_burst();
                        self.copy_shortcut(true);
                    }
                    return;
                }
                _ => {}
            }
        }
        if self.dictation.is_some() {
            match key.code {
                KeyCode::Esc => self.end(),
                KeyCode::Char(' ')
                    if control && !self.space_down && key.kind == KeyEventKind::Press =>
                {
                    self.space_down = true;
                    if self
                        .dictation
                        .as_ref()
                        .is_some_and(|d| d.gesture == SpaceGesture::Toggle)
                    {
                        self.stop();
                    }
                }
                _ => {}
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return;
        }
        if self.panes.is_empty() {
            let movement = match (key.code, control) {
                (KeyCode::Left, false) => Some(Movement::Left),
                (KeyCode::Right, false) => Some(Movement::Right),
                (KeyCode::Up, false) => Some(Movement::Up),
                (KeyCode::Down, false) => Some(Movement::Down),
                (KeyCode::Home, false) => Some(Movement::Home),
                (KeyCode::End, false) => Some(Movement::End),
                (KeyCode::Left, true) => Some(Movement::WordLeft),
                (KeyCode::Right, true) => Some(Movement::WordRight),
                (KeyCode::Home, true) => Some(Movement::NoteStart),
                (KeyCode::End, true) => Some(Movement::NoteEnd),
                _ => None,
            };
            if let Some(movement) = movement {
                self.notice = None;
                self.editor.move_caret(
                    movement,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                    usize::from(self.note_area.width),
                );
                return;
            }
        }
        if control {
            self.control_key(key);
            return;
        }
        self.notice = None;
        if let Some(pane) = self.panes.last_mut() {
            if let Some(action) = pane.key(key.code) {
                self.perform(action);
            }
            return;
        }

        let changed = match key.code {
            KeyCode::Esc => {
                self.editor.end_burst();
                self.panes.push(Pane::Menu(commands(false)));
                return;
            }
            KeyCode::Backspace => {
                self.editor
                    .edit_burst(BurstKind::Backspace, Instant::now(), Editor::backspace)
            }
            KeyCode::Delete => {
                self.editor
                    .edit_burst(BurstKind::Delete, Instant::now(), Editor::delete)
            }
            KeyCode::Enter => self.editor.edit(|editor| editor.insert("\n")),
            KeyCode::Tab => self.editor.edit(|editor| editor.insert("    ")),
            KeyCode::Char(c) if !c.is_control() => {
                self.editor
                    .edit_burst(BurstKind::Typing, Instant::now(), |editor| {
                        editor.insert(c.encode_utf8(&mut [0; 4]))
                    })
            }
            _ => return,
        };
        self.note_changed(changed);
    }

    fn control_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') => {
                if let Some(Pane::TextInput { text, .. }) = self.panes.last_mut() {
                    text.clear();
                    self.notice = None;
                }
            }
            KeyCode::Char('/') => {
                if self.panes.is_empty() {
                    self.editor.end_burst();
                    self.panes.push(Pane::Menu(commands(true)));
                    self.notice = None;
                } else if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.query.is_some())
                {
                    self.panes.pop();
                }
            }
            KeyCode::Char('a') if self.panes.is_empty() => {
                self.notice = None;
                self.editor.select_all();
            }
            KeyCode::Char('z') if self.panes.is_empty() => self.perform(Action::Undo),
            KeyCode::Char('y') if self.panes.is_empty() => self.perform(Action::Redo),
            KeyCode::Char(' ')
                if self.panes.is_empty()
                    && !self.space_down
                    && key.kind == KeyEventKind::Press
                    && self.config.error.is_none() =>
            {
                self.notice = None;
                self.space_down = true;
                self.start();
            }
            _ => {}
        }
    }

    fn hold_delay(&self) -> Duration {
        Duration::from_millis(u64::from(self.config.settings.hold_delay_ms))
    }

    fn start(&mut self) {
        let pressed = Instant::now();
        let cursor = self.editor.cursor();
        self.editor.clear_selection();
        self.editor.begin();
        self.dictation = Some(Dictation {
            session: Session::start(self.config.settings.clone()),
            gesture: SpaceGesture::Pressing(pressed),
            phase: DictationPhase::Pending,
            range: cursor..cursor,
        });
    }

    fn stop(&mut self) {
        if let Some(dictation) = self.dictation.as_mut() {
            dictation.session.stop();
            dictation.phase = DictationPhase::Finishing;
        }
    }

    fn end(&mut self) {
        if self.dictation.take().is_some() {
            self.editor.finish();
        }
    }

    fn note_changed(&mut self, changed: bool) {
        if changed {
            self.revision += 1;
        }
    }

    fn copy_shortcut(&mut self, cut: bool) {
        let range = self
            .editor
            .selection()
            .unwrap_or(0..self.editor.text().len());
        let (reply, confirmation) = if cut {
            let (reply, confirmation) = oneshot::channel();
            (Some(reply), Some(confirmation))
        } else {
            (None, None)
        };
        if self
            .clipboard
            .copy(&self.editor.text()[range.clone()], reply)
        {
            if let Some(reply) = confirmation {
                self.pending_cut = Some(PendingCut {
                    revision: self.revision,
                    range,
                    reply,
                });
            }
        } else {
            self.notice = Some("The Windows clipboard is busy.".into());
        }
    }

    pub fn set_agent_error(&mut self, error: String) {
        self.agent_error = Some(error);
    }

    pub fn agent_command(&mut self, command: Command) -> (AgentResult<Reply>, bool) {
        fn check(expected: Option<u64>, actual: u64) -> AgentResult<()> {
            if expected.is_some_and(|expected| expected != actual) {
                return Err(Error::new(
                    "stale_revision",
                    format!("The current note revision is {actual}. Read the note again."),
                ));
            }
            Ok(())
        }

        let mut changed = false;
        let result = (|| {
            use serde_json::json;
            match command {
                Command::Read => Ok(Reply::Value(
                    json!({"text": self.editor.text(), "revision": self.revision}),
                )),
                Command::Edit {
                    operation,
                    text,
                    revision,
                } => {
                    use crate::agent::Edit;
                    if matches!(operation, Edit::Replace | Edit::InsertAfterLine { .. })
                        && revision.is_none()
                    {
                        return Err(Error::new(
                            "revision_required",
                            "Read the note first and pass its revision.",
                        ));
                    }
                    if !self.panes.is_empty() || self.dictation.is_some() {
                        return Err(Error::new(
                            "busy",
                            "Close the pane or wait for dictation to end before editing.",
                        ));
                    }
                    check(revision, self.revision)?;
                    let text = clean_text(&text);
                    changed = match operation {
                        Edit::Append => self.editor.edit(|editor| editor.append(&text)),
                        Edit::Insert => self.editor.edit(|editor| {
                            if !text.is_empty() {
                                editor.clear_selection();
                            }
                            editor.insert(&text)
                        }),
                        Edit::InsertAfterLine { line } => self.insert_after_line(&line, text)?,
                        Edit::Replace => self.editor.edit(|editor| editor.replace(text)),
                    };
                    self.note_changed(changed);
                    Ok(Reply::Value(
                        json!({"revision": self.revision, "changed": changed}),
                    ))
                }
                Command::Copy { revision } => {
                    check(revision, self.revision)?;
                    if self.editor.text().is_empty() {
                        return Err(Error::new("empty_note", "The note is empty."));
                    }
                    let (reply, feedback) = tokio::sync::oneshot::channel();
                    if !self.clipboard.copy(self.editor.text(), Some(reply)) {
                        return Err(Error::new("busy", "The clipboard queue is full."));
                    }
                    Ok(Reply::Copy(feedback))
                }
            }
        })();
        (result, changed)
    }

    fn insert_after_line(&mut self, line: &str, mut text: String) -> AgentResult<bool> {
        let note = self.editor.text();
        let mut matches = note
            .split_inclusive('\n')
            .scan(0, |offset, chunk| {
                *offset += chunk.len();
                Some((*offset, chunk))
            })
            .filter(|(_, chunk)| chunk.lines().next() == Some(line));
        let (offset, chunk) = matches.next().ok_or_else(|| {
            Error::new(
                "anchor_not_found",
                "No line matches the supplied text exactly.",
            )
        })?;
        if matches.next().is_some() {
            return Err(Error::new(
                "ambiguous_anchor",
                "More than one line matches. Choose a unique line.",
            ));
        }
        if text.is_empty() {
            return Ok(false);
        }
        if chunk.ends_with('\n') {
            if !text.ends_with('\n') {
                text.push('\n');
            }
        } else {
            text.insert(0, '\n');
        }
        Ok(self.editor.edit(|editor| editor.insert_at(offset, &text)))
    }

    fn perform(&mut self, action: Action) {
        if matches!(action, Action::Back) {
            self.panes.pop();
            return;
        }
        if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.query.is_some()) {
            self.panes.pop();
        }
        match action {
            Action::Back => unreachable!(),
            Action::Quit => self.quit = true,
            Action::Undo => {
                self.notice = None;
                let changed = self.editor.undo();
                self.note_changed(changed);
            }
            Action::Redo => {
                self.notice = None;
                let changed = self.editor.redo();
                self.note_changed(changed);
            }
            Action::Clear => {
                let changed = self.editor.clear();
                self.note_changed(changed);
            }
            Action::Lowercase => {
                let changed = self.editor.lowercase();
                self.note_changed(changed);
            }
            Action::Settings => self.open_settings(),
            Action::Devices => self.open_devices(),
            Action::Provider => self.open_provider(),
            Action::Proxy => self.open_proxy(),
            Action::SetProxyEnabled(enabled) => {
                let mut proxy = self.config.settings.proxy.clone();
                proxy.enabled = enabled;
                self.save_proxy(proxy, false);
            }
            Action::EditProxyUrl => self.panes.push(Pane::TextInput {
                kind: InputKind::ProxyUrl,
                text: self
                    .config
                    .settings
                    .proxy
                    .url
                    .as_str()
                    .trim_end_matches('/')
                    .to_owned(),
            }),
            Action::SaveProxyUrl(input) => match parse_proxy_url(&input) {
                Ok(url) => self.save_proxy(
                    ProxySettings {
                        url,
                        ..self.config.settings.proxy.clone()
                    },
                    true,
                ),
                Err(error) => self.notice = Some(error),
            },
            Action::Help => self.panes.push(Pane::Help),
            Action::SelectDevice(device) => {
                let mut settings = self.config.settings.clone();
                settings.device = device;
                match self.config.save(settings) {
                    Ok(()) => {
                        self.panes.pop();
                        self.refresh_settings_menu();
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Action::EditAuthPath => {
                let path = self
                    .config
                    .settings
                    .auth_path
                    .as_ref()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                self.panes.push(Pane::TextInput {
                    kind: InputKind::AuthPath,
                    text: path,
                });
            }
            Action::ResetAuthPath => self.save_auth(None, false),
            Action::SaveAuthPath(path) => {
                let path = PathBuf::from(path.trim());
                if path.is_absolute() {
                    match login_status(&path) {
                        Ok(_) => self.save_auth(Some(path), true),
                        Err(error) => self.notice = Some(error),
                    }
                } else {
                    self.notice =
                        Some("Enter the absolute path to the Codex auth.json file.".into());
                }
            }
        }
    }

    fn save_auth(&mut self, path: Option<PathBuf>, editing: bool) {
        let mut settings = self.config.settings.clone();
        settings.auth_path = path;
        match self.config.save(settings) {
            Ok(()) => {
                if editing {
                    self.panes.pop();
                }
                self.panes.pop();
                self.open_provider();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn save_proxy(&mut self, proxy: ProxySettings, editing: bool) {
        let mut settings = self.config.settings.clone();
        settings.proxy = proxy;
        match self.config.save(settings) {
            Ok(()) => {
                if editing {
                    self.panes.pop();
                }
                if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.title == "Proxy") {
                    self.panes.pop();
                    self.refresh_settings_menu();
                    self.open_proxy();
                } else {
                    self.notice = Some(if self.config.settings.proxy.enabled {
                        "Proxy enabled for Codex.".into()
                    } else {
                        "Proxy disabled. Codex will connect directly.".into()
                    });
                }
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn open_proxy(&mut self) {
        let proxy = &self.config.settings.proxy;
        let status = if proxy.enabled {
            "On"
        } else {
            "Off · direct connection"
        };
        let (label, detail) = if proxy.enabled {
            ("Turn off", "Connect directly")
        } else {
            ("Turn on", "Use this proxy for Codex")
        };
        self.panes.push(Pane::Menu(Menu {
            title: "Proxy",
            query: None,
            selected: 0,
            context: vec![format!(
                "{status} · {}",
                proxy.url.as_str().trim_end_matches('/')
            )],
            rows: vec![
                Row::new(
                    "Edit address",
                    "HTTP, HTTPS, or SOCKS5 URL",
                    Action::EditProxyUrl,
                ),
                Row::new(label, detail, Action::SetProxyEnabled(!proxy.enabled)),
            ],
        }));
    }

    fn refresh_settings_menu(&mut self) {
        if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.title == "Settings") {
            let old = self.panes.pop();
            self.open_settings();
            if let (Some(Pane::Menu(old)), Some(Pane::Menu(new))) = (old, self.panes.last_mut()) {
                new.selected = old.selected;
            }
        }
    }

    fn open_settings(&mut self) {
        let label = if self.config.settings.device.is_some() {
            self.scan_devices();
            "Checking saved microphone…"
        } else {
            "System default"
        };
        self.panes.push(Pane::Menu(Menu {
            title: "Settings",
            query: None,
            context: Vec::new(),
            selected: 0,
            rows: vec![
                Row::new("Device", label, Action::Devices),
                Row::new("Provider", "Codex", Action::Provider),
                Row::new(
                    "Proxy",
                    if self.config.settings.proxy.enabled {
                        "On"
                    } else {
                        "Off"
                    },
                    Action::Proxy,
                ),
            ],
        }));
    }

    fn open_devices(&mut self) {
        self.scan_devices();
        self.panes.push(Pane::Menu(Self::device_menu(
            self.config.settings.device.as_deref(),
            None,
        )));
    }

    fn scan_devices(&mut self) {
        if self.device_scan.is_none() {
            let (send, receive) = mpsc::channel();
            thread::spawn(move || {
                let _ = send.send(audio::devices());
            });
            self.device_scan = Some(receive);
        }
    }

    fn poll_devices(&mut self) -> bool {
        let Some(scan) = &self.device_scan else {
            return false;
        };
        let result = match scan.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("Could not list microphone inputs.".into()),
        };
        self.device_scan = None;
        let saved = self.config.settings.device.as_deref();
        let mut changed = false;
        for pane in &mut self.panes {
            let Pane::Menu(menu) = pane else {
                continue;
            };
            match menu.title {
                "Input device" => *menu = Self::device_menu(saved, Some(&result)),
                "Settings" if saved.is_some() => {
                    menu.rows[0].detail = match &result {
                        Ok(devices) => devices
                            .iter()
                            .find(|device| Some(device.id.as_str()) == saved)
                            .map_or("Saved microphone unavailable", |device| &device.label)
                            .into(),
                        Err(error) => error.clone(),
                    };
                }
                _ => continue,
            }
            changed = true;
        }
        changed
    }

    fn device_menu(saved: Option<&str>, result: Option<&DeviceList>) -> Menu {
        let mut rows = vec![Row::new("System default", "", Action::SelectDevice(None))];
        let mut context = Vec::new();
        let mut selected = 0;
        match result {
            Some(Ok(devices)) => {
                for device in devices {
                    if saved == Some(device.id.as_str()) {
                        selected = rows.len();
                    }
                    rows.push(Row::new(
                        &device.label,
                        "",
                        Action::SelectDevice(Some(device.id.clone())),
                    ));
                }
            }
            Some(Err(error)) => context.push(error.clone()),
            None => context.push("Scanning microphones…".into()),
        }
        if saved.is_none() || selected > 0 {
            rows[selected].detail = "Current input".into();
        } else if matches!(result, Some(Ok(_))) {
            context.push("Saved microphone unavailable. Choose an input.".into());
        }
        Menu {
            title: "Input device",
            rows,
            context,
            query: None,
            selected,
        }
    }

    fn open_provider(&mut self) {
        let mut context = vec!["Codex · ChatGPT subscription".to_owned()];
        match self.config.settings.auth_file() {
            Ok(path) => {
                context.push(path.display().to_string());
                context.push(
                    login_status(&path)
                        .map(str::to_owned)
                        .unwrap_or_else(|error| error),
                );
            }
            Err(error) => context.push(error),
        }
        self.panes.push(Pane::Menu(Menu {
            title: "Provider",
            context,
            query: None,
            selected: 0,
            rows: vec![
                Row::new(
                    "Set auth file",
                    "Use an explicit path",
                    Action::EditAuthPath,
                ),
                Row::new(
                    "Use automatic path",
                    "CODEX_HOME or Windows user profile",
                    Action::ResetAuthPath,
                ),
            ],
        }));
    }
}

fn commands(slash_prompt: bool) -> Menu {
    Menu {
        title: "Commands",
        context: Vec::new(),
        query: Some(CommandQuery {
            text: String::new(),
            slash_prompt,
        }),
        selected: 0,
        rows: vec![
            Row::new("/clear", "Clear the note", Action::Clear),
            Row::new("/lowercase", "Lowercase the note", Action::Lowercase),
            Row::new(
                "/settings",
                "View device, provider, and proxy settings",
                Action::Settings,
            ),
            Row::new("/device", "Choose a microphone", Action::Devices),
            Row::new(
                "/provider",
                "View Codex login and change the auth path",
                Action::Provider,
            ),
            Row::new("/proxy", "View or edit the proxy address", Action::Proxy),
            Row::new(
                "/proxyon",
                "Enable the configured proxy",
                Action::SetProxyEnabled(true),
            ),
            Row::new(
                "/proxyoff",
                "Connect directly",
                Action::SetProxyEnabled(false),
            ),
            Row::new("/help", "Show keyboard shortcuts", Action::Help),
            Row::new("/quit", "Return to PowerShell", Action::Quit),
            Row::new(":q", "Return to PowerShell", Action::Quit),
            Row::new(":h", "Show keyboard shortcuts", Action::Help),
            Row::new(":u", "Undo the last edit", Action::Undo),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_places_caret_and_copies_only_real_line_breaks() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let (clipboard, notes, _report) = Clipboard::test_sink();
        app.clipboard = clipboard;
        app.note_area = Rect::new(1, 0, 4, 4);
        app.editor.insert("ab界cd\nefgh\nij");
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let left = MouseButton::Left;

        app.event(mouse(MouseEventKind::Down(left), 4, 2));
        app.event(mouse(MouseEventKind::Up(left), 4, 2));
        assert_eq!(&app.editor.text()[app.editor.cursor()..], "h\nij");
        assert_eq!(app.editor.selection(), None);

        app.event(mouse(MouseEventKind::Down(left), 2, 0));
        app.event(mouse(MouseEventKind::Drag(left), 3, 2));
        app.event(mouse(MouseEventKind::Up(left), 3, 2));
        app.event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            notes.recv_timeout(Duration::from_secs(1)).unwrap().text,
            "b界cd\nef"
        );

        app.event(mouse(MouseEventKind::Down(left), 2, 4));
        assert_eq!(
            &app.editor.text()[app.editor.selection().unwrap()],
            "b界cd\nef"
        );
    }

    #[test]
    fn clipboard_shortcuts_copy_cut_and_undo() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let (clipboard, notes, report) = Clipboard::test_sink();
        app.clipboard = clipboard;
        app.editor.insert("Entire note");
        let ctrl = |letter| KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL);

        app.key(ctrl('c'));
        assert!(!app.quit);
        assert_eq!(app.editor.text(), "Entire note");
        let copy = notes.try_recv().unwrap();
        assert_eq!(copy.text, "Entire note");
        app.tick();
        assert_eq!(app.copied_characters(), None);
        report.send(ClipboardFeedback::Copied(11)).unwrap();
        assert!(app.tick());
        assert_eq!(app.copied_characters(), Some(11));
        app.copied = Some((11, Instant::now()));
        assert!(app.tick());
        assert_eq!(app.copied_characters(), None);

        let (result, changed) = app.agent_command(Command::Copy { revision: Some(0) });
        assert!(!changed);
        let Ok(Reply::Copy(mut confirmation)) = result else {
            panic!("expected clipboard confirmation");
        };
        let request = notes.try_recv().unwrap();
        assert_eq!(request.text, "Entire note");
        assert!(confirmation.try_recv().is_err());
        request
            .reply
            .unwrap()
            .send(ClipboardFeedback::Failed)
            .ok()
            .unwrap();
        assert!(matches!(
            confirmation.try_recv(),
            Ok(ClipboardFeedback::Failed)
        ));
        assert_eq!(app.copied_characters(), None);

        app.key(ctrl('x'));
        app.key(ctrl('x'));
        assert_eq!(app.editor.text(), "Entire note");
        let cut = notes.try_recv().unwrap();
        assert_eq!(cut.text, "Entire note");
        assert!(notes.try_recv().is_err());
        cut.reply
            .unwrap()
            .send(ClipboardFeedback::Failed)
            .ok()
            .unwrap();
        report.send(ClipboardFeedback::Failed).unwrap();
        assert!(app.tick());
        assert_eq!(app.editor.text(), "Entire note");

        app.key(ctrl('x'));
        app.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        let copied = ClipboardFeedback::Copied(11);
        let cut = notes.try_recv().unwrap();
        cut.reply.unwrap().send(copied).ok().unwrap();
        assert!(!app.tick());
        assert_eq!(app.editor.text(), "Entire note!");

        app.key(ctrl('x'));
        let cut = notes.try_recv().unwrap();
        cut.reply
            .unwrap()
            .send(ClipboardFeedback::Copied(12))
            .ok()
            .unwrap();
        assert!(app.tick());
        assert_eq!(app.editor.text(), "");

        app.key(ctrl('z'));
        assert_eq!(app.editor.text(), "Entire note!");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(notes.try_recv().is_err());
    }

    #[test]
    fn keyboard_selection_replaces_text_and_cuts_the_original_range() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let (clipboard, notes, _report) = Clipboard::test_sink();
        app.clipboard = clipboard;
        let ctrl = |letter| KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL);
        let shifted = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
        app.editor.insert("one two 👩‍💻");

        app.key(shifted(KeyCode::Left));
        let select_word_left =
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        app.key(select_word_left);
        assert_eq!(app.editor.selection(), Some(4..19));
        app.key(ctrl('c'));
        assert_eq!(notes.try_recv().unwrap().text, "two 👩‍💻");

        app.key(ctrl('x'));
        let cut = notes.try_recv().unwrap();
        assert_eq!(cut.text, "two 👩‍💻");
        app.key(ctrl('a'));
        assert_eq!(app.editor.selection(), Some(0..19));
        cut.reply
            .unwrap()
            .send(ClipboardFeedback::Copied(5))
            .ok()
            .unwrap();
        assert!(app.tick());
        assert_eq!(app.editor.text(), "one ");
        assert!(app.editor.selection().is_none());

        app.key(ctrl('a'));
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.editor.text(), "x");
        app.key(ctrl('z'));
        assert_eq!(app.editor.text(), "one ");
        app.key(ctrl('a'));
        app.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(app.editor.text().is_empty());
    }

    #[test]
    fn command_menu_entry_points_dismiss_and_run_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let esc = key(KeyCode::Esc);
        let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL);
        let enter = key(KeyCode::Enter);

        app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(!app.quit);

        for (entry, prefix) in [(esc, ""), (slash, "/")] {
            app.key(entry);
            let Some(Pane::Menu(menu)) = app.panes.last() else {
                panic!("expected command menu");
            };
            assert_eq!(menu.query.as_ref().unwrap().display(), prefix);
            app.key(key(KeyCode::Char('c')));
            app.key(esc);
            assert!(app.panes.is_empty());
        }

        let run = |app: &mut App, entry, command: &str| {
            app.key(entry);
            for c in command.chars() {
                app.key(key(KeyCode::Char(c)));
            }
            app.key(enter);
        };
        app.key(key(KeyCode::Char('A')));
        run(&mut app, esc, "lowercase");
        assert_eq!(app.editor.text(), "a");
        run(&mut app, slash, ":u");
        assert_eq!(app.editor.text(), "A");
        run(&mut app, esc, ":h");
        assert!(matches!(app.panes.as_slice(), [Pane::Help]));
        app.key(esc);
        for entry in [esc, slash] {
            run(&mut app, entry, "/help");
            assert!(matches!(app.panes.as_slice(), [Pane::Help]));
            app.key(esc);
        }
        run(&mut app, esc, ":q!");
        assert!(!app.quit);
        app.key(esc);
        run(&mut app, esc, ":q");
        assert!(app.quit);
        for entry in [esc, slash] {
            app.quit = false;
            run(&mut app, entry, "/quit");
            assert!(app.quit);
        }
    }

    #[test]
    fn keyboard_history_keeps_replacements_newlines_and_pastes_separate() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl = |letter| KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL);
        app.editor.insert("draft");
        app.key(ctrl('a'));
        for c in "new".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        app.event(Event::Paste("pasted".into()));
        app.key(key(KeyCode::Char('!')));
        assert_eq!(app.editor.text(), "new\npasted!");

        for previous in ["new\npasted", "new\n", "new", "draft"] {
            app.key(ctrl('z'));
            assert_eq!(app.editor.text(), previous);
        }
        for next in ["new", "new\n", "new\npasted", "new\npasted!"] {
            app.key(ctrl('y'));
            assert_eq!(app.editor.text(), next);
        }
    }

    #[test]
    fn microphone_scan_updates_open_menus_without_reopening_dismissed_panes() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::load(directory.path().join("dflow.toml"));
        config.settings.device = Some("saved".into());
        let mut app = App::new(config);
        let (send, receive) = mpsc::channel();
        app.device_scan = Some(receive);
        app.open_settings();
        app.open_devices();
        assert!(!app.tick());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        send.send(Ok(vec![InputDevice {
            id: "saved".into(),
            label: "Headset".into(),
        }]))
        .unwrap();
        assert!(app.tick());
        let [Pane::Menu(settings)] = app.panes.as_slice() else {
            panic!("expected only the settings menu");
        };
        assert_eq!(settings.rows[0].detail, "Headset");

        let (send, receive) = mpsc::channel();
        app.device_scan = Some(receive);
        app.open_devices();
        send.send(Err("Enumeration failed".into())).unwrap();
        assert!(app.tick());
        let Some(Pane::Menu(devices)) = app.panes.last() else {
            panic!("expected the device menu");
        };
        assert_eq!(devices.context, ["Enumeration failed"]);
        assert_eq!(devices.rows[0].name, "System default");
    }

    #[test]
    fn agent_inserts_below_a_unique_line_and_undoes_the_edit() {
        let directory = tempfile::tempdir().unwrap();
        for (before, after) in [
            (
                "  ## Étapes\nExisting",
                "  ## Étapes\nFirst\nSecond\nExisting",
            ),
            ("  ## Étapes", "  ## Étapes\nFirst\nSecond"),
            ("  ## Étapes\n", "  ## Étapes\nFirst\nSecond\n"),
        ] {
            let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
            app.editor = Editor::from_note(before.into(), 0);
            let insert = || Command::Edit {
                operation: crate::agent::Edit::InsertAfterLine {
                    line: "  ## Étapes".into(),
                },
                text: "First\nSecond".into(),
                revision: Some(0),
            };

            let (result, changed) = app.agent_command(insert());
            assert!(result.is_ok());
            assert!(changed);
            assert_eq!(app.editor.text(), after);
            assert_eq!(app.revision(), 1);

            let (result, changed) = app.agent_command(insert());
            assert_eq!(
                serde_json::to_value(result.err().unwrap()).unwrap()["code"],
                "stale_revision"
            );
            assert!(!changed);
            assert_eq!(app.editor.text(), after);

            app.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
            assert_eq!(app.editor.text(), before);
            assert_eq!(app.editor.cursor(), 0);
        }
    }

    #[test]
    fn agent_insertion_rejects_missing_and_duplicate_lines() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let note = "## Plans\n## Plans\n";
        app.editor = Editor::from_note(note.into(), 0);
        for (line, code) in [
            ("Plans", "anchor_not_found"),
            ("## Plans", "ambiguous_anchor"),
        ] {
            let (result, changed) = app.agent_command(Command::Edit {
                operation: crate::agent::Edit::InsertAfterLine { line: line.into() },
                text: "New text".into(),
                revision: Some(0),
            });
            assert_eq!(
                serde_json::to_value(result.err().unwrap()).unwrap()["code"],
                code
            );
            assert!(!changed);
            assert_eq!(app.editor.text(), note);
            assert_eq!(app.revision(), 0);
        }
    }

    #[test]
    fn dictation_revises_without_copying_and_escape_discards_pending_text() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let (clipboard, notes, _report) = Clipboard::test_sink();
        app.clipboard = clipboard;
        app.editor.insert("Note:");
        let cursor = app.editor.cursor();
        app.editor.begin();
        let (session, send) = Session::test_channel();
        app.dictation = Some(Dictation {
            session,
            gesture: SpaceGesture::Toggle,
            phase: DictationPhase::Live { quiet: false },
            range: cursor..cursor,
        });

        let (edit, changed) = app.agent_command(Command::Edit {
            operation: crate::agent::Edit::Append,
            text: "Agent output".into(),
            revision: None,
        });
        assert!(edit.is_err());
        assert!(!changed);

        for text in ["We should meet.", "We should meet on Tuesday."] {
            send.send(Update::Transcript(text.into())).unwrap();
            assert!(app.tick());
            assert_eq!(app.editor.text(), format!("Note: {text}"));
            assert!(notes.try_recv().is_err());
        }

        send.send(Update::Transcript("A pending result.".into()))
            .unwrap();
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.tick());
        assert_eq!(app.editor.text(), "Note: We should meet on Tuesday.");
        assert!(notes.try_recv().is_err());

        app.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.text(), "Note:");
        assert!(notes.try_recv().is_err());
    }

    #[test]
    fn space_release_keeps_a_tap_recording_and_stops_a_hold() {
        let pressed = Instant::now();
        let delay = Duration::from_millis(250);
        let mut tap = SpaceGesture::Pressing(pressed);
        assert!(!tap.release(pressed + delay - Duration::from_millis(1), delay));
        assert!(tap == SpaceGesture::Toggle);

        let mut hold = SpaceGesture::Pressing(pressed);
        assert!(hold.advance(pressed + delay, delay));
        assert!(hold.release(pressed + delay, delay));

        let mut release_before_tick = SpaceGesture::Pressing(pressed);
        assert!(release_before_tick.release(pressed + delay, delay));
    }
}
