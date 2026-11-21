use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

const MODIFIERS: [(&str, u32); 4] = [("Ctrl", 2), ("Shift", 4), ("Alt", 1), ("Win", 8)];

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            modifiers: 2 | 4,
            key: 0xbf,
        }
    }
}

impl FromStr for Hotkey {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        const ERROR: &str = "Enter a shortcut such as Ctrl+Shift+/, with a modifier and a key.";
        let (modifiers, key) = value.rsplit_once('+').ok_or(ERROR)?;
        let mut bits = 0;
        for modifier in modifiers.split('+').map(str::trim) {
            let (_, bit) = MODIFIERS
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(modifier))
                .ok_or(ERROR)?;
            if bits & bit != 0 {
                return Err(ERROR);
            }
            bits |= bit;
        }
        let key = key.trim().to_ascii_uppercase();
        let vk = match key.as_str() {
            "/" => 0xbf,
            "`" => 0xc0,
            "," => 0xbc,
            "." => 0xbe,
            "-" => 0xbd,
            "SPACE" => 0x20,
            _ => match key.as_bytes() {
                [key @ (b'A'..=b'Z' | b'0'..=b'9')] => u32::from(*key),
                _ => key
                    .strip_prefix('F')
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|n| (1..=24).contains(n) && *n != 12)
                    .map(|n| 0x6f + n)
                    .ok_or(ERROR)?,
            },
        };
        Ok(Self {
            modifiers: bits,
            key: vk,
        })
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, bit) in MODIFIERS {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            0xbf => f.write_str("/"),
            0xc0 => f.write_str("`"),
            0xbc => f.write_str(","),
            0xbe => f.write_str("."),
            0xbd => f.write_str("-"),
            0x20 => f.write_str("Space"),
            0x70..=0x87 => write!(f, "F{}", self.key - 0x6f),
            other => write!(f, "{}", char::from_u32(other).ok_or(fmt::Error)?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_custom_hotkeys_round_trip() {
        assert_eq!("Ctrl+Shift+/".parse(), Ok(Hotkey::default()));
        for input in ["Ctrl+Shift+/", "Ctrl+Shift+F2", "Ctrl+Alt+A", "Shift+Win+9"] {
            assert_eq!(input.parse::<Hotkey>().unwrap().to_string(), input);
        }
        for invalid in ["A", "Ctrl+Ctrl+A", "Ctrl+F12", "Ctrl+Unknown"] {
            assert!(invalid.parse::<Hotkey>().is_err());
        }
    }
}
