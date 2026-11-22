use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::{
    collections::{HashSet, VecDeque},
    io,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Console::{
            ENABLE_VIRTUAL_TERMINAL_INPUT, FOCUS_EVENT, GetConsoleMode,
            GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, LEFT_ALT_PRESSED,
            LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, ReadConsoleInputW,
            SHIFT_PRESSED, SetConsoleMode, WINDOW_BUFFER_SIZE_EVENT,
        },
        Threading::WaitForSingleObject,
    },
    UI::Input::KeyboardAndMouse::*,
};

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";
const ESC: u16 = 27;

pub fn enable_vt_input(handle: HANDLE) -> io::Result<()> {
    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0
        || unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub struct TerminalInput {
    handle: HANDLE,
    decoder: Decoder,
}

impl TerminalInput {
    pub fn new(handle: HANDLE) -> Self {
        Self {
            handle,
            decoder: Decoder::default(),
        }
    }

    pub fn read(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        if let Some(event) = self.decoder.events.pop_front() {
            return Ok(Some(event));
        }
        match unsafe { WaitForSingleObject(self.handle, timeout.as_millis() as u32) } {
            WAIT_TIMEOUT => return Ok(None),
            WAIT_OBJECT_0 => {}
            _ => return Err(io::Error::last_os_error()),
        }
        let mut available = 0;
        if unsafe { GetNumberOfConsoleInputEvents(self.handle, &mut available) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if available == 0 {
            return Ok(None);
        }
        let mut records = [INPUT_RECORD::default(); 128];
        let mut count = 0;
        let limit = available.min(records.len() as u32);
        if unsafe { ReadConsoleInputW(self.handle, records.as_mut_ptr(), limit, &mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        for record in &records[..count as usize] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    let key = unsafe { record.Event.KeyEvent };
                    let unit = unsafe { key.uChar.UnicodeChar };
                    if key.wVirtualKeyCode == 0
                        && key.wVirtualScanCode == 0
                        && key.dwControlKeyState == 0
                    {
                        if key.bKeyDown != 0 {
                            self.decoder.unit(unit);
                        }
                    } else {
                        self.decoder.key(
                            key.wVirtualKeyCode,
                            key.wVirtualScanCode,
                            unit,
                            key.bKeyDown != 0,
                            key.dwControlKeyState,
                            key.wRepeatCount,
                        );
                    }
                }
                FOCUS_EVENT => self
                    .decoder
                    .focus(unsafe { record.Event.FocusEvent.bSetFocus } != 0),
                WINDOW_BUFFER_SIZE_EVENT => {
                    let size = unsafe { record.Event.WindowBufferSizeEvent.dwSize };
                    self.decoder
                        .events
                        .push_back(Event::Resize(size.X as u16, size.Y as u16));
                }
                _ => {}
            }
        }
        Ok(self.decoder.events.pop_front())
    }
}

#[derive(Default)]
struct Decoder {
    events: VecDeque<Event>,
    sequence: String,
    paste: Option<Vec<u16>>,
    paste_end: usize,
    surrogate: Option<u16>,
    pressed: HashSet<(u16, u16)>,
}

impl Decoder {
    fn unit(&mut self, unit: u16) {
        if let Some(paste) = &mut self.paste {
            let end = PASTE_END.as_bytes();
            if unit == u16::from(end[self.paste_end]) {
                self.paste_end += 1;
                if self.paste_end == end.len() {
                    let text = pasted_text(paste);
                    self.paste = None;
                    self.paste_end = 0;
                    self.events.push_back(Event::Paste(text));
                }
            } else {
                paste.extend(end[..self.paste_end].iter().copied().map(u16::from));
                self.paste_end = usize::from(unit == ESC);
                if unit != ESC {
                    paste.push(unit);
                }
            }
            return;
        }

        if unit == ESC {
            self.sequence.clear();
            self.sequence.push('\x1b');
            return;
        }
        if self.sequence.is_empty() {
            self.character(unit, KeyModifiers::NONE, KeyEventKind::Press);
            return;
        }
        if unit > 127 || self.sequence.len() == 64 {
            self.sequence.clear();
            return;
        }
        let ch = unit as u8 as char;
        self.sequence.push(ch);
        if self.sequence == "\x1b[" {
            return;
        }
        if !self.sequence.starts_with("\x1b[") {
            self.sequence.clear();
        } else if ('@'..='~').contains(&ch) {
            let sequence = std::mem::take(&mut self.sequence);
            self.sequence(&sequence);
            self.sequence = sequence;
            self.sequence.clear();
        }
    }

    fn sequence(&mut self, sequence: &str) {
        if sequence == PASTE_START {
            self.paste = Some(Vec::new());
            return;
        }
        let parameters = &sequence[2..sequence.len() - 1];
        match sequence.as_bytes().last() {
            Some(b'_') => {
                if let Some([vk, scan, unit, down, state, repeat]) = win32_fields(parameters) {
                    self.key(
                        vk as u16,
                        scan as u16,
                        unit as u16,
                        down != 0,
                        state,
                        repeat as u16,
                    );
                }
            }
            Some(b'M' | b'm') if parameters.starts_with('<') => {
                let mut fields = parameters[1..].split(';').map(str::parse::<u16>);
                if let (Some(Ok(button)), Some(Ok(x)), Some(Ok(y))) =
                    (fields.next(), fields.next(), fields.next())
                {
                    self.mouse(button, x, y, sequence.ends_with('m'));
                }
            }
            Some(b'I') if parameters.is_empty() => self.focus(true),
            Some(b'O') if parameters.is_empty() => self.focus(false),
            _ => {}
        }
    }

    fn focus(&mut self, focused: bool) {
        if !focused {
            self.pressed.clear();
            self.surrogate = None;
            self.sequence.clear();
            self.paste = None;
            self.paste_end = 0;
        }
        self.events.push_back(if focused {
            Event::FocusGained
        } else {
            Event::FocusLost
        });
    }

    fn mouse(&mut self, flags: u16, x: u16, y: u16, released: bool) {
        let kind = match (flags & 0b1100011, released) {
            (0, true) => MouseEventKind::Up(MouseButton::Left),
            (0, false) => MouseEventKind::Down(MouseButton::Left),
            (32, false) => MouseEventKind::Drag(MouseButton::Left),
            _ => return,
        };
        let modifiers = modifiers(
            (if flags & 4 != 0 { SHIFT_PRESSED } else { 0 })
                | (if flags & 8 != 0 { LEFT_ALT_PRESSED } else { 0 })
                | (if flags & 16 != 0 {
                    LEFT_CTRL_PRESSED
                } else {
                    0
                }),
        );
        self.events.push_back(Event::Mouse(MouseEvent {
            kind,
            modifiers,
            column: x.saturating_sub(1),
            row: y.saturating_sub(1),
        }));
    }

    fn key(&mut self, vk: u16, scan: u16, unit: u16, down: bool, state: u32, repeat: u16) {
        let kind = if !down {
            self.pressed.remove(&(vk, scan));
            KeyEventKind::Release
        } else if self.pressed.insert((vk, scan)) {
            KeyEventKind::Press
        } else {
            KeyEventKind::Repeat
        };
        let modifiers = modifiers(state);
        let named = match vk {
            VK_ESCAPE => Some(KeyCode::Esc),
            VK_RETURN => Some(KeyCode::Enter),
            VK_TAB if modifiers.contains(KeyModifiers::SHIFT) => Some(KeyCode::BackTab),
            VK_TAB => Some(KeyCode::Tab),
            VK_BACK => Some(KeyCode::Backspace),
            VK_DELETE => Some(KeyCode::Delete),
            VK_LEFT => Some(KeyCode::Left),
            VK_RIGHT => Some(KeyCode::Right),
            VK_UP => Some(KeyCode::Up),
            VK_DOWN => Some(KeyCode::Down),
            VK_HOME => Some(KeyCode::Home),
            VK_END => Some(KeyCode::End),
            VK_SPACE => Some(KeyCode::Char(' ')),
            _ => None,
        };
        let unit = if modifiers.contains(KeyModifiers::CONTROL) && unit < 32 {
            match vk {
                0x41..=0x5a => vk + 32,
                _ if unit == 31 => b'/' as u16,
                _ if unit == 0 && down => shortcut_character(vk, scan),
                _ => unit,
            }
        } else {
            unit
        };
        if let Some(code) = named {
            self.surrogate = None;
            self.push_key(code, modifiers, kind, repeat);
        } else if (0xd800..=0xdfff).contains(&unit) {
            if down {
                self.character(unit, modifiers, kind);
            }
        } else if unit >= 32 {
            self.surrogate = None;
            if let Some(ch) = char::from_u32(u32::from(unit)) {
                self.push_key(KeyCode::Char(ch), modifiers, kind, repeat);
            }
        }
    }

    fn character(&mut self, unit: u16, modifiers: KeyModifiers, kind: KeyEventKind) {
        if (0xd800..=0xdbff).contains(&unit) {
            self.surrogate = Some(unit);
            return;
        }
        let previous = self.surrogate.take();
        let ch = if let Some(high) = previous.filter(|_| (0xdc00..=0xdfff).contains(&unit)) {
            char::decode_utf16([high, unit]).next().and_then(Result::ok)
        } else {
            char::from_u32(u32::from(unit))
        };
        if let Some(ch) = ch.filter(|ch| !ch.is_control()) {
            self.push_key(KeyCode::Char(ch), modifiers, kind, 1);
        }
    }

    fn push_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
        repeat: u16,
    ) {
        self.events
            .push_back(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)));
        if kind != KeyEventKind::Release {
            self.events.extend((1..repeat).map(|_| {
                Event::Key(KeyEvent::new_with_kind(
                    code,
                    modifiers,
                    KeyEventKind::Repeat,
                ))
            }));
        }
    }
}

fn modifiers(state: u32) -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::SHIFT, state & SHIFT_PRESSED != 0);
    modifiers.set(
        KeyModifiers::CONTROL,
        state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0,
    );
    modifiers.set(
        KeyModifiers::ALT,
        state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0,
    );
    modifiers
}

fn pasted_text(units: &[u16]) -> String {
    // ConPTY can encode control characters such as CR, LF, and Tab as
    // Win32 keyboard records even between the paste delimiters.
    let mut text = Vec::with_capacity(units.len());
    let mut remaining = units;
    while let Some((&unit, rest)) = remaining.split_first() {
        remaining = rest;
        if unit == ESC
            && let Some(parameters) = rest.strip_prefix(&[u16::from(b'[')])
            && let Some(end) = parameters
                .iter()
                .take(64)
                .position(|&unit| unit != u16::from(b';') && !(48..=57).contains(&unit))
            && parameters[end] == u16::from(b'_')
            && let Some([_, _, character, down, _, repeat]) = win32_fields(
                parameters[..end]
                    .iter()
                    .map(|&unit| unit as u8 as char)
                    .collect::<String>()
                    .as_str(),
            )
        {
            if down != 0 && character != 0 {
                text.extend(std::iter::repeat_n(
                    character as u16,
                    usize::from((repeat as u16).max(1)),
                ));
            }
            remaining = &parameters[end + 1..];
        } else {
            text.push(unit);
        }
    }
    String::from_utf16_lossy(&text)
}

fn win32_fields(parameters: &str) -> Option<[u32; 6]> {
    let mut fields = [0; 6];
    for (index, field) in parameters.split(';').enumerate() {
        *fields.get_mut(index)? = if field.is_empty() {
            0
        } else {
            field.parse().ok()?
        };
    }
    Some(fields)
}

fn shortcut_character(vk: u16, scan: u16) -> u16 {
    let keys = [0u8; 256];
    let mut text = [0u16; 8];
    let layout = unsafe { GetKeyboardLayout(0) };
    let count = unsafe {
        ToUnicodeEx(
            u32::from(vk),
            u32::from(scan),
            keys.as_ptr(),
            text.as_mut_ptr(),
            text.len() as i32,
            4,
            layout,
        )
    };
    if count == 1 { text[0] } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_preserves_unicode_and_does_not_dispatch_payload_as_keys() {
        let mut decoder = Decoder::default();
        for chunk in [
            "\x1b[20",
            "0~One\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_\n\t界😀\x03\x1b[27;1;27;1;0;1_\x1b[20",
            "1~",
        ] {
            for unit in chunk.encode_utf16() {
                decoder.unit(unit);
            }
        }
        assert_eq!(
            decoder.events.pop_front(),
            Some(Event::Paste("One\r\n\t界😀\x03\x1b".into()))
        );
        assert!(decoder.events.is_empty());
    }

    #[test]
    fn win32_keys_keep_releases_and_repeats_alongside_mouse_input() {
        let mut decoder = Decoder::default();
        for unit in concat!(
            "\x1b[32;57;0;1;8;2_",
            "\x1b[32;57;0;0;0;1_",
            "\x1b[<0;2;3M",
            "\x1b[<32;5;6M",
            "\x1b[<0;5;6m",
        )
        .encode_utf16()
        {
            decoder.unit(unit);
        }
        for kind in [
            KeyEventKind::Press,
            KeyEventKind::Repeat,
            KeyEventKind::Release,
        ] {
            let Some(Event::Key(key)) = decoder.events.pop_front() else {
                panic!("expected a Space key event");
            };
            assert_eq!((key.code, key.kind), (KeyCode::Char(' '), kind));
        }
        for (kind, position) in [
            (MouseEventKind::Down(MouseButton::Left), (1, 2)),
            (MouseEventKind::Drag(MouseButton::Left), (4, 5)),
            (MouseEventKind::Up(MouseButton::Left), (4, 5)),
        ] {
            let Some(Event::Mouse(mouse)) = decoder.events.pop_front() else {
                panic!("expected a mouse event");
            };
            assert_eq!((mouse.kind, (mouse.column, mouse.row)), (kind, position));
        }
    }
}
