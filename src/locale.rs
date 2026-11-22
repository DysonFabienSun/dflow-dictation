#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Language {
    #[default]
    Auto,
    English,
    SimplifiedChinese,
}

impl Language {
    pub const OPTIONS: [Self; 3] = [Self::Auto, Self::English, Self::SimplifiedChinese];

    pub fn from_config(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "en" => Some(Self::English),
            "zh-CN" => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }

    pub fn config_value(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::English => Some("en"),
            Self::SimplifiedChinese => Some("zh-CN"),
        }
    }

    pub fn label(self, locale: Locale) -> &'static str {
        match self {
            Self::Auto => locale.text("Follow system language", "跟随系统语言设置"),
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }

    pub fn resolve(self, system: impl FnOnce() -> Locale) -> Locale {
        match self {
            Self::Auto => system(),
            Self::English => Locale::English,
            Self::SimplifiedChinese => Locale::SimplifiedChinese,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locale {
    English,
    SimplifiedChinese,
}

impl Locale {
    pub fn text(self, english: &'static str, chinese: &'static str) -> &'static str {
        match self {
            Self::English => english,
            Self::SimplifiedChinese => chinese,
        }
    }

    #[cfg(any(windows, test))]
    fn from_system_language(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if matches!(name.as_str(), "zh-cn" | "zh-sg" | "zh-hans") || name.starts_with("zh-hans-") {
            Self::SimplifiedChinese
        } else {
            Self::English
        }
    }

    pub fn system() -> Self {
        #[cfg(windows)]
        {
            use std::ptr::null_mut;
            use windows_sys::Win32::Globalization::{
                GetUserPreferredUILanguages, MUI_LANGUAGE_NAME,
            };

            let mut count = 0;
            let mut length = 0;
            let available = unsafe {
                GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, null_mut(), &mut length)
            };
            if available != 0 && length > 0 {
                let mut names = vec![0; length as usize];
                let available = unsafe {
                    GetUserPreferredUILanguages(
                        MUI_LANGUAGE_NAME,
                        &mut count,
                        names.as_mut_ptr(),
                        &mut length,
                    )
                };
                if available != 0 && count > 0 {
                    let primary = names.split(|&unit| unit == 0).next().unwrap_or_default();
                    return Self::from_system_language(&String::from_utf16_lossy(primary));
                }
            }
        }
        Self::English
    }

    pub fn language_changed(self) -> &'static str {
        self.text(
            "Interface language changes take effect after restart",
            "界面语言将在重启后生效",
        )
    }

    pub fn dictation_name(self) -> &'static str {
        self.text("DFlow Dictation", "DFlow 听写")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_primary_system_language_and_explicit_overrides() {
        for language in ["zh-CN", "zh-SG", "zh-Hans", "zh-hans-CN"] {
            assert_eq!(
                Locale::from_system_language(language),
                Locale::SimplifiedChinese
            );
        }
        for language in ["zh-TW", "zh-Hant", "en-US", "fr-FR", ""] {
            assert_eq!(Locale::from_system_language(language), Locale::English);
        }
        assert_eq!(
            Language::Auto.resolve(|| Locale::SimplifiedChinese),
            Locale::SimplifiedChinese
        );
        assert_eq!(
            Language::English.resolve(|| unreachable!()),
            Locale::English
        );
        assert_eq!(
            Language::SimplifiedChinese.resolve(|| unreachable!()),
            Locale::SimplifiedChinese
        );
    }
}
