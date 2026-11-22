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
use dflow_scratchpad::locale::{Language, Locale};
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
    Language,
    SetLanguage(Language),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Commands,
    Settings,
    Devices,
    Language,
    Provider,
    Proxy,
}

pub struct Menu {
    kind: MenuKind,
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
        let limit = if self.query.is_some() {
            5
        } else {
            self.rows.len()
        };
        self.rows
            .iter()
            .filter(|row| {
                self.query.as_ref().is_none_or(|query| {
                    let text = query.text.strip_prefix('/').unwrap_or(&query.text);
                    row.name.trim_start_matches('/').starts_with(text)
                })
            })
            .take(limit)
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
    pub fn labels(&self, locale: Locale) -> (&'static str, &'static str) {
        match self {
            Self::AuthPath => (
                locale.text("Codex auth file", "Codex 登录文件"),
                locale.text("Absolute path", "绝对路径"),
            ),
            Self::ProxyUrl => (
                locale.text("Proxy address", "代理地址"),
                locale.text(
                    "http://, https:// or socks5://",
                    "http://、https:// 或 socks5://",
                ),
            ),
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
    pub locale: Locale,
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
            locale: config.locale,
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
        let locale = self.locale;
        if let Some(dictation) = &self.dictation {
            return match dictation.phase {
                DictationPhase::Pending => ("", false),
                DictationPhase::Live { quiet: true } => (
                    locale.text(
                        "Mic input is quiet · check /device or input gain",
                        "麦克风输入音量较低 · 请检查 /device 或提高输入音量",
                    ),
                    true,
                ),
                DictationPhase::Live { quiet: false }
                    if dictation.gesture == SpaceGesture::Hold =>
                {
                    (
                        locale.text(
                            "● Recording · release Space to stop · Esc cancel",
                            "● 正在录音 · 松开空格键停止 · Esc 取消",
                        ),
                        false,
                    )
                }
                DictationPhase::Live { quiet: false }
                    if matches!(dictation.gesture, SpaceGesture::Pressing(_)) =>
                {
                    (
                        locale.text(
                            "● Recording · release to keep recording · hold for push-to-talk",
                            "● 正在录音 · 松开后继续录音 · 按住可持续听写",
                        ),
                        false,
                    )
                }
                DictationPhase::Live { quiet: false } => (
                    locale.text(
                        "● Recording · Ctrl+Space stop · Esc cancel",
                        "● 正在录音 · Ctrl+Space 停止 · Esc 取消",
                    ),
                    false,
                ),
                DictationPhase::Finishing => (
                    locale.text(
                        "Finishing transcription… · Esc cancel",
                        "正在完成转写… · Esc 取消",
                    ),
                    false,
                ),
            };
        }
        if let Some(notice) = &self.notice {
            return (notice, true);
        }
        if let Some(error) = &self.config.error {
            return (error, true);
        }
        if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.query.is_some()) {
            return (
                locale.text(
                    "↑↓ select · Tab complete · Enter run · Esc close",
                    "↑↓ 选择 · Tab 补全 · Enter 运行 · Esc 关闭",
                ),
                false,
            );
        }
        if !self.panes.is_empty() {
            return (
                locale.text(
                    "↑↓ select · Enter confirm · Esc back",
                    "↑↓ 选择 · Enter 确认 · Esc 返回",
                ),
                false,
            );
        }
        if let Some(error) = self.cache_error {
            return (error, true);
        }
        if let Some(error) = &self.agent_error {
            return (error, true);
        }
        (
            locale.text(
                "Ctrl+Space dictate · Esc commands · Ctrl+Z undo",
                "Ctrl+Space 听写 · Esc 命令列表 · Ctrl+Z 撤销",
            ),
            false,
        )
    }

    pub fn copied_characters(&self) -> Option<usize> {
        self.copied.map(|(count, _)| count)
    }

    pub fn tick(&mut self) -> bool {
        let locale = self.locale;
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
                    self.notice = Some(
                        locale
                            .text("Could not update the Windows clipboard.", "无法写入剪贴板")
                            .into(),
                    );
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
                    self.notice = Some(
                        locale
                            .text("Could not update the Windows clipboard.", "无法写入剪贴板")
                            .into(),
                    )
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
                Err(TryRecvError::Disconnected) => Update::Failed(
                    locale
                        .text("Dictation ended unexpectedly.", "听写意外中断")
                        .into(),
                ),
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
                    let changed = self.editor.edit(|editor| editor.insert_incoming(&text));
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
                self.panes.push(Pane::Menu(commands(false, self.locale)));
                return;
            }
            KeyCode::Char('/')
                if key.kind == KeyEventKind::Press && self.editor.take_underline_slash() =>
            {
                self.control_key(key);
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
                    self.panes.push(Pane::Menu(commands(true, self.locale)));
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
            session: Session::start(self.config.settings.clone(), self.locale),
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
            self.notice = Some(
                self.locale
                    .text("The Windows clipboard is busy.", "剪贴板正被占用")
                    .into(),
            );
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
                        Edit::Append => self
                            .editor
                            .edit(|editor| editor.insert_incoming_at(editor.text().len(), &text)),
                        Edit::Insert => self.editor.edit(|editor| {
                            if !text.is_empty() {
                                editor.clear_selection();
                            }
                            editor.insert_incoming(&text)
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
        Ok(self
            .editor
            .edit(|editor| editor.insert_incoming_at(offset, &text)))
    }

    fn perform(&mut self, action: Action) {
        let locale = self.locale;
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
            Action::Language => self.open_language(),
            Action::SetLanguage(language) => self.set_language(language),
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
            Action::SaveProxyUrl(input) => match parse_proxy_url(&input, locale) {
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
                    Err(error) => self.notice = Some(error.restart_message(&self.config)),
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
                    match login_status(&path, locale) {
                        Ok(_) => self.save_auth(Some(path), true),
                        Err(error) => self.notice = Some(error),
                    }
                } else {
                    self.notice = Some(
                        locale
                            .text(
                                "Enter the absolute path to the Codex auth.json file.",
                                "请输入 Codex 登录文件 auth.json 的绝对路径",
                            )
                            .into(),
                    );
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
            Err(error) => self.notice = Some(error.restart_message(&self.config)),
        }
    }

    fn save_proxy(&mut self, proxy: ProxySettings, editing: bool) {
        let locale = self.locale;
        let mut settings = self.config.settings.clone();
        settings.proxy = proxy;
        match self.config.save(settings) {
            Ok(()) => {
                if editing {
                    self.panes.pop();
                }
                if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.kind == MenuKind::Proxy)
                {
                    self.panes.pop();
                    self.refresh_settings_menu();
                    self.open_proxy();
                } else {
                    self.notice = Some(if self.config.settings.proxy.enabled {
                        locale
                            .text("Proxy enabled for Codex.", "已启用网络代理")
                            .into()
                    } else {
                        locale
                            .text(
                                "Proxy disabled. Codex will connect directly.",
                                "已关闭网络代理，将直接连接 ChatGPT",
                            )
                            .into()
                    });
                }
            }
            Err(error) => self.notice = Some(error.restart_message(&self.config)),
        }
    }

    fn open_proxy(&mut self) {
        let locale = self.locale;
        let proxy = &self.config.settings.proxy;
        let status = if proxy.enabled {
            locale.text("On", "已启用")
        } else {
            locale.text("Off · direct connection", "已关闭 · 直接连接")
        };
        let (label, detail) = if proxy.enabled {
            (
                locale.text("Turn off", "关闭代理"),
                locale.text("Connect directly", "直接连接"),
            )
        } else {
            (
                locale.text("Turn on", "启用代理"),
                locale.text("Use this proxy for Codex", "使用此代理连接 ChatGPT"),
            )
        };
        self.panes.push(Pane::Menu(Menu {
            kind: MenuKind::Proxy,
            title: locale.text("Proxy", "网络代理"),
            query: None,
            selected: 0,
            context: vec![format!(
                "{status} · {}",
                proxy.url.as_str().trim_end_matches('/')
            )],
            rows: vec![
                Row::new(
                    locale.text("Edit address", "修改地址"),
                    locale.text("HTTP, HTTPS, or SOCKS5 URL", "HTTP、HTTPS 或 SOCKS5 地址"),
                    Action::EditProxyUrl,
                ),
                Row::new(label, detail, Action::SetProxyEnabled(!proxy.enabled)),
            ],
        }));
    }

    fn refresh_settings_menu(&mut self) {
        if matches!(self.panes.last(), Some(Pane::Menu(menu)) if menu.kind == MenuKind::Settings) {
            let old = self.panes.pop();
            self.open_settings();
            if let (Some(Pane::Menu(old)), Some(Pane::Menu(new))) = (old, self.panes.last_mut()) {
                new.selected = old.selected;
            }
        }
    }

    fn open_settings(&mut self) {
        let locale = self.locale;
        let label = if self.config.settings.device.is_some() {
            self.scan_devices();
            locale.text("Checking saved microphone…", "正在检查所选麦克风…")
        } else {
            locale.text("System default", "系统默认")
        };
        self.panes.push(Pane::Menu(Menu {
            kind: MenuKind::Settings,
            title: locale.text("Settings", "设置"),
            query: None,
            context: Vec::new(),
            selected: 0,
            rows: vec![
                Row::new(locale.text("Device", "麦克风"), label, Action::Devices),
                Row::new(
                    locale.text("Codex sign-in", "Codex 登录"),
                    "",
                    Action::Provider,
                ),
                Row::new(
                    locale.text("Proxy", "网络代理"),
                    if self.config.settings.proxy.enabled {
                        locale.text("On", "已启用")
                    } else {
                        locale.text("Off", "已关闭")
                    },
                    Action::Proxy,
                ),
                Row::new(
                    locale.text("Interface language", "界面语言"),
                    self.config.settings.language.label(locale),
                    Action::Language,
                ),
            ],
        }));
    }

    fn open_language(&mut self) {
        let locale = self.locale;
        let rows = Language::OPTIONS
            .into_iter()
            .map(|language| Row::new(language.label(locale), "", Action::SetLanguage(language)))
            .collect();
        let selected = Language::OPTIONS
            .iter()
            .position(|&language| language == self.config.settings.language)
            .unwrap_or_default();
        self.panes.push(Pane::Menu(Menu {
            kind: MenuKind::Language,
            title: locale.text("Interface language", "界面语言"),
            context: vec![locale.language_changed().into()],
            query: None,
            rows,
            selected,
        }));
    }

    fn set_language(&mut self, language: Language) {
        if language == self.config.settings.language {
            self.panes.pop();
            return;
        }
        let mut settings = self.config.settings.clone();
        settings.language = language;
        match self.config.save(settings) {
            Ok(()) => {
                self.panes.pop();
                self.refresh_settings_menu();
                if let Some(Pane::Menu(menu)) = self.panes.last_mut() {
                    menu.context.push(self.locale.language_changed().into());
                }
            }
            Err(error) => self.notice = Some(error.restart_message(&self.config)),
        }
    }

    fn open_devices(&mut self) {
        self.scan_devices();
        self.panes.push(Pane::Menu(Self::device_menu(
            self.config.settings.device.as_deref(),
            None,
            self.locale,
        )));
    }

    fn scan_devices(&mut self) {
        if self.device_scan.is_none() {
            let (send, receive) = mpsc::channel();
            let locale = self.locale;
            thread::spawn(move || {
                let _ = send.send(audio::devices(locale));
            });
            self.device_scan = Some(receive);
        }
    }

    fn poll_devices(&mut self) -> bool {
        let locale = self.locale;
        let Some(scan) = &self.device_scan else {
            return false;
        };
        let result = match scan.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(locale
                .text("Could not list microphone inputs.", "无法获取麦克风列表")
                .into()),
        };
        self.device_scan = None;
        let saved = self.config.settings.device.as_deref();
        let mut changed = false;
        for pane in &mut self.panes {
            let Pane::Menu(menu) = pane else {
                continue;
            };
            match menu.kind {
                MenuKind::Devices => *menu = Self::device_menu(saved, Some(&result), locale),
                MenuKind::Settings if saved.is_some() => {
                    menu.rows[0].detail = match &result {
                        Ok(devices) => devices
                            .iter()
                            .find(|device| Some(device.id.as_str()) == saved)
                            .map_or(
                                locale.text("Saved microphone unavailable", "所选麦克风不可用"),
                                |device| &device.label,
                            )
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

    fn device_menu(saved: Option<&str>, result: Option<&DeviceList>, locale: Locale) -> Menu {
        let mut rows = vec![Row::new(
            locale.text("System default", "系统默认"),
            "",
            Action::SelectDevice(None),
        )];
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
            None => context.push(
                locale
                    .text("Scanning microphones…", "正在搜索麦克风…")
                    .into(),
            ),
        }
        if saved.is_none() || selected > 0 {
            rows[selected].detail = locale.text("Current input", "当前输入设备").into();
        } else if matches!(result, Some(Ok(_))) {
            context.push(
                locale
                    .text(
                        "Saved microphone unavailable. Choose an input.",
                        "所选麦克风不可用，请选择输入设备",
                    )
                    .into(),
            );
        }
        Menu {
            kind: MenuKind::Devices,
            title: locale.text("Input device", "输入设备"),
            rows,
            context,
            query: None,
            selected,
        }
    }

    fn open_provider(&mut self) {
        let locale = self.locale;
        let mut context = vec![
            locale
                .text("Codex · ChatGPT subscription", "Codex · ChatGPT 订阅")
                .to_owned(),
        ];
        match self.config.settings.auth_file(locale) {
            Ok(path) => {
                context.push(path.display().to_string());
                context.push(
                    login_status(&path, locale)
                        .map(str::to_owned)
                        .unwrap_or_else(|error| error),
                );
            }
            Err(error) => context.push(error),
        }
        self.panes.push(Pane::Menu(Menu {
            kind: MenuKind::Provider,
            title: locale.text("Codex sign-in", "Codex 登录"),
            context,
            query: None,
            selected: 0,
            rows: vec![
                Row::new(
                    locale.text("Set auth file", "指定登录文件"),
                    locale.text("Use an explicit path", "手动指定路径"),
                    Action::EditAuthPath,
                ),
                Row::new(
                    locale.text("Use automatic path", "自动查找登录文件"),
                    locale.text(
                        "CODEX_HOME or Windows user profile",
                        "CODEX_HOME 或系统用户目录",
                    ),
                    Action::ResetAuthPath,
                ),
            ],
        }));
    }
}

fn commands(slash_prompt: bool, locale: Locale) -> Menu {
    Menu {
        kind: MenuKind::Commands,
        title: locale.text("Commands", "命令列表"),
        context: Vec::new(),
        query: Some(CommandQuery {
            text: String::new(),
            slash_prompt,
        }),
        selected: 0,
        rows: vec![
            Row::new(
                "/clear",
                locale.text("Clear the note", "清空便笺"),
                Action::Clear,
            ),
            Row::new(
                "/lowercase",
                locale.text(
                    "Selection or underline · keep abbreviations",
                    "将选中或带下划线的文字转为小写 · 保留缩写",
                ),
                Action::Lowercase,
            ),
            Row::new(
                "/settings",
                locale.text(
                    "View device, provider, and proxy settings",
                    "查看麦克风、登录、代理和界面设置",
                ),
                Action::Settings,
            ),
            Row::new(
                "/device",
                locale.text("Choose a microphone", "选择麦克风"),
                Action::Devices,
            ),
            Row::new(
                "/provider",
                locale.text(
                    "View Codex login and change the auth path",
                    "查看 Codex 登录状态和修改登录文件路径",
                ),
                Action::Provider,
            ),
            Row::new(
                "/proxy",
                locale.text("View or edit the proxy address", "查看或修改代理地址"),
                Action::Proxy,
            ),
            Row::new(
                "/proxyon",
                locale.text("Enable the configured proxy", "启用已配置的代理"),
                Action::SetProxyEnabled(true),
            ),
            Row::new(
                "/proxyoff",
                locale.text("Connect directly", "直接连接"),
                Action::SetProxyEnabled(false),
            ),
            Row::new(
                "/help",
                locale.text("Show keyboard shortcuts", "显示键盘快捷键"),
                Action::Help,
            ),
            Row::new(
                "/quit",
                locale.text("Return to PowerShell", "返回 PowerShell"),
                Action::Quit,
            ),
            Row::new(
                ":q",
                locale.text("Return to PowerShell", "返回 PowerShell"),
                Action::Quit,
            ),
            Row::new(
                ":h",
                locale.text("Show keyboard shortcuts", "显示键盘快捷键"),
                Action::Help,
            ),
            Row::new(
                ":u",
                locale.text("Undo the last edit", "撤销上一次编辑"),
                Action::Undo,
            ),
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
            assert_eq!(
                menu.visible()
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>(),
                ["/clear", "/lowercase", "/settings", "/device", "/provider"]
            );
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
        run(&mut app, esc, "/lower");
        assert_eq!(app.editor.text(), "A");
        assert!(app.panes.is_empty());
        for (entry, command) in [(esc, "lowercase"), (esc, "/lower"), (slash, "lower")] {
            app.key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
            run(&mut app, entry, command);
            assert_eq!(app.editor.text(), "a");
            run(&mut app, slash, ":u");
            assert_eq!(app.editor.text(), "A");
        }
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
    fn first_slash_after_each_underline_opens_commands_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let slash = key(KeyCode::Char('/'));
        let esc = key(KeyCode::Esc);

        app.key(slash);
        assert_eq!(app.editor.text(), "/");
        app.event(Event::Paste("First".into()));
        app.key(slash);
        let [Pane::Menu(menu)] = app.panes.as_slice() else {
            panic!("expected the slash command menu");
        };
        assert_eq!(menu.query.as_ref().unwrap().display(), "/");
        assert_eq!(app.editor.text(), "/First");
        assert_eq!(app.editor.underline(), Some(1..6));
        app.key(esc);
        app.key(slash);
        assert!(app.panes.is_empty());
        assert_eq!(app.editor.text(), "/First/");

        app.event(Event::Paste("Second".into()));
        app.key(slash);
        for c in "lower".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(app.editor.text(), "/First/second");

        app.event(Event::Paste("Third".into()));
        app.key(key(KeyCode::Char('!')));
        app.key(slash);
        assert!(app.panes.is_empty());
        assert_eq!(app.editor.text(), "/First/secondThird!/");
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
        let range = app.editor.underline().unwrap();
        assert_eq!(&app.editor.text()[range], "pasted");
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
        assert_eq!(
            devices.rows[0].name,
            app.locale.text("System default", "系统默认")
        );
    }

    #[test]
    fn language_menu_saves_the_selection_for_the_next_launch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        let mut app = App::new(Config::load_for(path.clone(), Some(Locale::English)));
        app.open_settings();
        app.perform(Action::Language);
        let Some(Pane::Menu(menu)) = app.panes.last() else {
            panic!("expected the language menu");
        };
        assert_eq!(
            menu.visible()
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["Follow system language", "English", "简体中文"]
        );
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.locale, Locale::English);
        let mut restarted = App::new(Config::load(path));
        restarted.open_settings();
        let Some(Pane::Menu(menu)) = restarted.panes.last() else {
            panic!("expected the settings menu");
        };
        assert_eq!(menu.title, "设置");
        assert_eq!(menu.rows[3].name, "界面语言");
        assert_eq!(menu.rows[3].detail, "简体中文");
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
            let range = app.editor.underline().unwrap();
            assert_eq!(app.editor.text()[range].trim(), "First\nSecond");

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
    fn agent_replacement_underlines_the_new_note_for_lowercase() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(Config::load(directory.path().join("dflow.toml")));
        app.editor = Editor::from_note("Old Text".into(), 0);
        let replacement = "New NASA Text";
        let (result, changed) = app.agent_command(Command::Edit {
            operation: crate::agent::Edit::Replace,
            text: replacement.into(),
            revision: Some(0),
        });
        assert!(result.is_ok());
        assert!(changed);
        assert_eq!(app.editor.underline(), Some(0..replacement.len()));
        app.perform(Action::Lowercase);
        assert_eq!(app.editor.text(), "new NASA text");
        assert_eq!(app.editor.underline(), None);
        app.perform(Action::Undo);
        assert_eq!(app.editor.text(), replacement);
        assert_eq!(app.editor.underline(), None);
        app.perform(Action::Undo);
        assert_eq!(app.editor.text(), "Old Text");
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
            assert_eq!(
                &app.editor.text()[app.editor.underline().unwrap()],
                format!(" {text}")
            );
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
        assert_eq!(app.editor.underline(), None);
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
