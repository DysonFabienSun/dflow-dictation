use crate::{
    hotkey::Hotkey,
    locale::{Language, Locale},
};
use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Clone)]
pub struct Settings {
    pub language: Language,
    pub device: Option<String>,
    pub hold_delay_ms: u32,
    pub auth_path: Option<PathBuf>,
    pub cloud_dictation: Option<bool>,
    pub cleanup: bool,
    pub proxy: ProxySettings,
}

pub const DEFAULT_PROXY: &str = "http://127.0.0.1:10808";
const DEFAULT_HOLD_DELAY_MS: u32 = 1000;

pub fn settings_path() -> std::io::Result<PathBuf> {
    settings_path_for(&env::current_exe()?)
}

fn settings_path_for(executable: &Path) -> std::io::Result<PathBuf> {
    executable
        .parent()
        .map(|root| root.join("config").join("dflow.toml"))
        .ok_or_else(|| std::io::Error::other("Cannot locate the DFlow executable directory."))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::Auto,
            device: None,
            hold_delay_ms: DEFAULT_HOLD_DELAY_MS,
            auth_path: None,
            cloud_dictation: None,
            cleanup: false,
            proxy: ProxySettings::default(),
        }
    }
}

#[derive(Clone)]
pub struct ProxySettings {
    pub url: reqwest::Url,
    pub enabled: bool,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            url: reqwest::Url::parse(DEFAULT_PROXY).expect("valid default proxy"),
            enabled: false,
        }
    }
}

pub fn parse_proxy_url(input: &str, locale: Locale) -> Result<reqwest::Url, String> {
    let input = input.trim();
    let invalid = locale.text(
        "Enter an http://, https://, or socks5:// proxy URL with a host and optional port.",
        "请输入以 http://、https:// 或 socks5:// 开头的代理地址，须包含主机名，可指定端口",
    );
    let (_, address) = input.split_once("://").ok_or(invalid)?;
    let authority = address.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(locale
            .text(
                "Proxy URLs must not include usernames or passwords.",
                "代理地址不能包含用户名或密码",
            )
            .into());
    }
    if authority.is_empty()
        || input
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(invalid.into());
    }
    let url = reqwest::Url::parse(input).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "https" | "socks5")
        || url.host_str().is_none()
        || url.port() == Some(0)
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid.into());
    }
    Ok(url)
}

impl Settings {
    pub fn require_dictation(&self, locale: Locale) -> Result<(), String> {
        if self.cloud_dictation == Some(true) {
            return Ok(());
        }
        Err(locale
            .text(
                "Cloud dictation is off. Enable it in Dictation setup.",
                "云端听写已关闭，请在听写设置中启用",
            )
            .into())
    }

    pub fn auth_file(&self, locale: Locale) -> Result<PathBuf, String> {
        if let Some(path) = &self.auth_path {
            return Ok(path.clone());
        }
        if let Some(home) = env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(home).join("auth.json"));
        }
        env::var_os("USERPROFILE")
            .filter(|s| !s.is_empty())
            .map(|home| PathBuf::from(home).join(".codex").join("auth.json"))
            .ok_or_else(|| {
                locale
                    .text(
                        "Set the Codex auth file in Settings.",
                        "请在设置中指定 Codex 登录文件",
                    )
                    .into()
            })
    }
}

pub struct Config {
    pub path: PathBuf,
    pub settings: Settings,
    pub error: Option<String>,
    pub locale: Locale,
    document: DocumentMut,
    original: Option<String>,
}

#[derive(Debug)]
pub enum SaveError {
    ChangedOnDisk,
    Message(String),
}

impl SaveError {
    pub fn restart_message(self, config: &Config) -> String {
        match self {
            Self::ChangedOnDisk => {
                let path = config.path.display();
                match config.locale {
                    Locale::English => {
                        format!("{path} changed on disk. Restart DFlow before saving.")
                    }
                    Locale::SimplifiedChinese => format!("{path} 已被修改，请重启 DFlow 后再保存"),
                }
            }
            Self::Message(message) => message,
        }
    }
}

fn read_config(path: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

impl Config {
    pub fn load(path: PathBuf) -> Self {
        Self::load_for(path, None)
    }

    pub fn load_for(path: PathBuf, active_locale: Option<Locale>) -> Self {
        let mut locale = active_locale;
        let loaded: Result<_, String> = (|| {
            let original = read_config(&path).map_err(|error| error.to_string())?;
            let document = original
                .as_deref()
                .unwrap_or_default()
                .parse::<DocumentMut>()
                .map_err(|error| error.message().to_owned())?;
            let language = read_language(&document).ok_or_else(|| {
                locale
                    .get_or_insert_with(Locale::system)
                    .text(
                        "ui.language must be auto, en, or zh-CN.",
                        "ui.language 必须是 auto、en 或 zh-CN",
                    )
                    .to_owned()
            })?;
            let locale = *locale.get_or_insert_with(|| language.resolve(Locale::system));
            let settings = parse_settings(&document, language, locale)?;
            Ok((document, settings, original))
        })();
        let locale = locale.unwrap_or_else(Locale::system);
        match loaded {
            Ok((document, settings, original)) => Self {
                path,
                settings,
                error: None,
                locale,
                document,
                original,
            },
            Err(error) => Self {
                error: Some(format!(
                    "{} · {}: {error}",
                    locale.text("Could not load settings", "无法加载设置"),
                    path.display()
                )),
                path,
                settings: Settings::default(),
                locale,
                document: DocumentMut::new(),
                original: None,
            },
        }
    }

    pub fn save(&mut self, settings: Settings) -> Result<(), SaveError> {
        self.save_with_tray(settings, None)
    }

    pub fn tray_hotkey(&self) -> Result<Hotkey, String> {
        string(
            table(&self.document, "tray", self.locale)?,
            "hotkey",
            self.locale,
        )?
        .map(|text| Hotkey::parse_localized(text, self.locale))
        .unwrap_or_else(|| Ok(Hotkey::default()))
    }

    pub fn tray_launch_preview(&self) -> Result<bool, String> {
        table(&self.document, "tray", self.locale)?
            .and_then(|table| table.get("show_launch_preview"))
            .map(|item| {
                item.as_bool().ok_or_else(|| {
                    self.locale
                        .text(
                            "tray.show_launch_preview must be true or false.",
                            "tray.show_launch_preview 必须是 true 或 false",
                        )
                        .into()
                })
            })
            .unwrap_or(Ok(true))
    }

    pub fn save_tray(
        &mut self,
        settings: Settings,
        hotkey: Hotkey,
        show_launch_preview: bool,
    ) -> Result<(), SaveError> {
        self.save_with_tray(settings, Some((hotkey, show_launch_preview)))
    }

    fn save_with_tray(
        &mut self,
        settings: Settings,
        tray: Option<(Hotkey, bool)>,
    ) -> Result<(), SaveError> {
        let locale = self.locale;
        let path = self.path.display();
        if self.error.is_some() {
            return Err(SaveError::Message(match locale {
                Locale::English => format!("Correct {path} and restart DFlow before saving."),
                Locale::SimplifiedChinese => format!("请修正 {path} 并重启 DFlow 后再保存"),
            }));
        }
        let mut document = self.document.clone();
        if let Some((hotkey, show_launch_preview)) = tray {
            table(&document, "tray", locale).map_err(SaveError::Message)?;
            set_optional(&mut document, "tray", "hotkey", Some(&hotkey.to_string()));
            document["tray"]["show_launch_preview"] = value(show_launch_preview);
        }
        set_optional(
            &mut document,
            "ui",
            "language",
            settings.language.config_value(),
        );
        set_optional(
            &mut document,
            "audio",
            "device_id",
            settings.device.as_deref(),
        );
        document["audio"]["hold_delay_ms"] = value(i64::from(settings.hold_delay_ms));
        set_optional(
            &mut document,
            "transcription",
            "codex_auth_path",
            settings.auth_path.as_deref().and_then(Path::to_str),
        );
        document["transcription"]["provider"] = value("codex");
        if let Some(enabled) = settings.cloud_dictation {
            document["transcription"]["cloud_dictation"] = value(enabled);
        } else if let Some(table) = document
            .get_mut("transcription")
            .and_then(Item::as_table_like_mut)
        {
            table.remove("cloud_dictation");
        }
        document["transcription"]["cleanup"] = value(settings.cleanup);
        document["proxy"]["url"] = value(settings.proxy.url.as_str());
        document["proxy"]["enabled"] = value(settings.proxy.enabled);

        let text = document.to_string();
        let parent = self.path.parent().ok_or_else(|| {
            SaveError::Message(
                locale
                    .text("Missing config directory.", "找不到配置目录")
                    .into(),
            )
        })?;
        let save = || -> std::io::Result<bool> {
            fs::create_dir_all(parent)?;
            // Lock a stable path since persisting the temporary file replaces the TOML file.
            let lock = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.path.with_extension("toml.lock"))?;
            lock.try_lock()?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(text.as_bytes())?;
            temp.as_file().sync_all()?;
            if read_config(&self.path)? != self.original {
                return Ok(false);
            }
            temp.persist(&self.path).map_err(|error| error.error)?;
            Ok(true)
        };
        if !save().map_err(|error| {
            SaveError::Message(match locale {
                Locale::English => format!("Could not save {path}: {error}"),
                Locale::SimplifiedChinese => format!("无法保存 {path}：{error}"),
            })
        })? {
            return Err(SaveError::ChangedOnDisk);
        }
        self.document = document;
        self.original = Some(text);
        self.settings = settings;
        Ok(())
    }
}

fn read_language(document: &DocumentMut) -> Option<Language> {
    let Some(ui) = document.get("ui") else {
        return Some(Language::Auto);
    };
    match ui.as_table_like()?.get("language") {
        None => Some(Language::Auto),
        Some(item) => Language::from_config(item.as_str()?),
    }
}

fn parse_settings(
    document: &DocumentMut,
    language: Language,
    locale: Locale,
) -> Result<Settings, String> {
    let audio = table(document, "audio", locale)?;
    let transcription = table(document, "transcription", locale)?;
    let proxy = table(document, "proxy", locale)?;
    if string(transcription, "provider", locale)?.is_some_and(|provider| provider != "codex") {
        return Err(locale
            .text(
                "transcription.provider must be codex.",
                "transcription.provider 必须是 codex",
            )
            .into());
    }
    let settings = Settings {
        language,
        device: string(audio, "device_id", locale)?.map(str::to_owned),
        hold_delay_ms: audio
            .and_then(|table| table.get("hold_delay_ms"))
            .map(|item| {
                item.as_integer()
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or(locale.text(
                        "audio.hold_delay_ms must be a positive integer in milliseconds.",
                        "audio.hold_delay_ms 必须是正整数，单位为毫秒",
                    ))
            })
            .transpose()?
            .unwrap_or(DEFAULT_HOLD_DELAY_MS),
        auth_path: string(transcription, "codex_auth_path", locale)?.map(PathBuf::from),
        cloud_dictation: boolean(transcription, "cloud_dictation", locale)?,
        cleanup: boolean(transcription, "cleanup", locale)?.unwrap_or(false),
        proxy: ProxySettings {
            url: parse_proxy_url(
                string(proxy, "url", locale)?.unwrap_or(DEFAULT_PROXY),
                locale,
            )?,
            enabled: proxy
                .and_then(|table| table.get("enabled"))
                .map(|item| {
                    item.as_bool().ok_or(locale.text(
                        "proxy.enabled must be true or false.",
                        "proxy.enabled 必须是 true 或 false",
                    ))
                })
                .transpose()?
                .unwrap_or(false),
        },
    };
    if settings
        .auth_path
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(locale
            .text(
                "transcription.codex_auth_path must be an absolute path.",
                "transcription.codex_auth_path 必须是绝对路径",
            )
            .into());
    }
    Ok(settings)
}

fn table<'a>(
    doc: &'a DocumentMut,
    key: &str,
    locale: Locale,
) -> Result<Option<&'a dyn toml_edit::TableLike>, String> {
    doc.get(key)
        .map(|item| {
            item.as_table_like().ok_or_else(|| match locale {
                Locale::English => format!("{key} must be a table."),
                Locale::SimplifiedChinese => format!("{key} 必须是 TOML 表"),
            })
        })
        .transpose()
}

fn boolean(
    table: Option<&dyn toml_edit::TableLike>,
    key: &str,
    locale: Locale,
) -> Result<Option<bool>, String> {
    table
        .and_then(|table| table.get(key))
        .map(|item| {
            item.as_bool().ok_or_else(|| match locale {
                Locale::English => format!("{key} must be true or false."),
                Locale::SimplifiedChinese => format!("{key} 必须是 true 或 false"),
            })
        })
        .transpose()
}

fn string<'a>(
    table: Option<&'a dyn toml_edit::TableLike>,
    key: &str,
    locale: Locale,
) -> Result<Option<&'a str>, String> {
    table
        .and_then(|t| t.get(key))
        .map(|item| {
            item.as_str()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| match locale {
                    Locale::English => format!("{key} must be a nonempty string."),
                    Locale::SimplifiedChinese => format!("{key} 必须是非空字符串"),
                })
        })
        .transpose()
}

fn set_optional(doc: &mut DocumentMut, table: &str, key: &str, text: Option<&str>) {
    if let Some(text) = text {
        if doc.get(table).is_none() {
            doc[table] = Item::Table(Table::new());
        }
        doc[table][key] = value(text);
    } else if let Some(table) = doc.get_mut(table).and_then(Item::as_table_like_mut) {
        table.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrading_requires_consent_and_a_stale_choice_cannot_replace_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        fs::write(&path, "[transcription]\nprovider = \"codex\"\nextra = 1\n").unwrap();
        let mut scratchpad = Config::load(path.clone());
        let mut tray = Config::load(path.clone());
        assert_eq!(scratchpad.settings.cloud_dictation, None);
        assert!(!scratchpad.settings.cleanup);
        let decline = Settings {
            cloud_dictation: Some(false),
            ..scratchpad.settings.clone()
        };
        let accept = Settings {
            cloud_dictation: Some(true),
            ..tray.settings.clone()
        };
        scratchpad.save(decline).unwrap();
        assert!(matches!(
            tray.save(accept.clone()),
            Err(SaveError::ChangedOnDisk)
        ));
        tray = Config::load(path.clone());
        assert_eq!(tray.settings.cloud_dictation, Some(false));
        tray.save(accept).unwrap();
        let scratchpad = Config::load(path.clone());
        assert_eq!(scratchpad.settings.cloud_dictation, Some(true));
        assert!(!scratchpad.settings.cleanup);
        assert!(fs::read_to_string(&path).unwrap().contains("extra = 1"));

        let invalid = "[transcription]\ncloud_dictation = \"yes\"\n";
        fs::write(&path, invalid).unwrap();
        let mut config = Config::load(path.clone());
        assert!(config.error.is_some());
        assert!(config.settings.require_dictation(Locale::English).is_err());
        assert!(config.save(config.settings.clone()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), invalid);
    }

    #[test]
    fn applications_share_config_only_in_the_same_directory() {
        let root = tempfile::tempdir().unwrap();
        let scratchpad = settings_path_for(&root.path().join("dflow.exe")).unwrap();
        let tray = settings_path_for(&root.path().join("dflow-tray.exe")).unwrap();
        let separate =
            settings_path_for(&root.path().join("separate").join("dflow-tray.exe")).unwrap();
        assert_eq!(scratchpad, tray);
        assert_ne!(scratchpad, separate);
        let alternate = "Ctrl+Shift+F2".parse().unwrap();
        let mut tray = Config::load(tray);
        assert_eq!(tray.tray_hotkey().unwrap(), Hotkey::default());
        assert!(tray.tray_launch_preview().unwrap());
        tray.save_tray(tray.settings.clone(), alternate, false)
            .unwrap();
        let mut scratchpad = Config::load(scratchpad);
        let mut settings = scratchpad.settings.clone();
        settings.hold_delay_ms = 300;
        scratchpad.save(settings).unwrap();
        let shared = Config::load(scratchpad.path);
        assert_eq!(shared.settings.hold_delay_ms, 300);
        assert_eq!(shared.tray_hotkey().unwrap(), alternate);
        assert!(!shared.tray_launch_preview().unwrap());
        assert_eq!(
            Config::load(separate).tray_hotkey().unwrap(),
            Hotkey::default()
        );
    }

    #[test]
    fn saves_settings_without_losing_unrelated_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config/dflow.toml");
        let mut config = Config::load(path.clone());
        assert!(config.settings.device.is_none());
        assert_eq!(config.settings.hold_delay_ms, 1000);
        assert!(!config.settings.proxy.enabled);
        config
            .save(Settings {
                device: Some("wasapi:sample".into()),
                hold_delay_ms: 250,
                proxy: ProxySettings {
                    url: parse_proxy_url("socks5://localhost:1080", Locale::English).unwrap(),
                    enabled: true,
                },
                ..Settings::default()
            })
            .unwrap();
        let mut original = fs::read_to_string(&path).unwrap();
        original.push_str("future_key = 7\n");
        fs::write(&path, original).unwrap();
        let mut config = Config::load(path.clone());
        assert_eq!(config.settings.device.as_deref(), Some("wasapi:sample"));
        assert_eq!(config.settings.hold_delay_ms, 250);
        assert!(config.settings.proxy.enabled);
        assert_eq!(
            config.settings.proxy.url.as_str(),
            "socks5://localhost:1080"
        );
        config.save(Settings::default()).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("future_key = 7")
        );
        assert!(Config::load(path).settings.device.is_none());
    }

    #[test]
    fn only_accepts_unauthenticated_proxy_endpoints() {
        for url in [
            DEFAULT_PROXY,
            "https://proxy.example",
            "socks5://[::1]:1080",
        ] {
            assert!(parse_proxy_url(url, Locale::English).is_ok(), "{url}");
        }
        for url in [
            "http://user@host:80",
            "https://user:password@host:443",
            "socks5://:password@host",
            "http://@host",
            "http://host/path",
            "http://host?token=x",
            "ftp://host",
        ] {
            assert!(parse_proxy_url(url, Locale::English).is_err(), "{url}");
        }
    }

    #[test]
    fn rejects_saves_after_another_window_or_editor_changes_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        let mut first = Config::load(path.clone());
        let mut second = Config::load(path.clone());
        first.save(Settings::default()).unwrap();
        assert!(second.save(Settings::default()).is_err());

        let edited = "# Edited externally\n[proxy]\nenabled = true\nextra = 7\n";
        fs::write(&path, edited).unwrap();
        assert!(first.save(Settings::default()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), edited);
        assert!(!first.settings.proxy.enabled);
    }

    #[test]
    fn language_preference_round_trips_between_apps_and_applies_on_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        fs::write(&path, "[ui]\nextra = 7\n").unwrap();
        let mut scratchpad = Config::load_for(path.clone(), Some(Locale::English));
        let mut tray = Config::load_for(path.clone(), Some(Locale::English));
        assert_eq!(scratchpad.settings.language, Language::Auto);
        let chinese = Settings {
            language: Language::SimplifiedChinese,
            ..scratchpad.settings.clone()
        };
        scratchpad.save(chinese).unwrap();
        assert_eq!(scratchpad.locale, Locale::English);
        assert!(matches!(
            tray.save(tray.settings.clone()),
            Err(SaveError::ChangedOnDisk)
        ));

        let mut tray = Config::load(path.clone());
        assert_eq!(tray.locale, Locale::SimplifiedChinese);
        let english = Settings {
            language: Language::English,
            ..tray.settings.clone()
        };
        tray.save_tray(english, Hotkey::default(), true).unwrap();
        assert_eq!(tray.locale, Locale::SimplifiedChinese);
        let mut scratchpad = Config::load(path.clone());
        assert_eq!(scratchpad.locale, Locale::English);
        let automatic = Settings {
            language: Language::Auto,
            ..scratchpad.settings.clone()
        };
        scratchpad.save(automatic).unwrap();
        assert_eq!(Config::load(path.clone()).settings.language, Language::Auto);
        assert!(fs::read_to_string(&path).unwrap().contains("extra = 7"));

        fs::write(&path, "[ui]\nlanguage = \"auto\"\n").unwrap();
        assert_eq!(Config::load(path).locale, Locale::system());
    }

    #[test]
    fn invalid_language_preferences_preserve_the_settings_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dflow.toml");
        for original in [
            "[ui]\nlanguage = \"zh-TW\"\n",
            "[ui]\nlanguage = true\n",
            "[ui\n",
        ] {
            fs::write(&path, original).unwrap();
            let mut config = Config::load_for(path.clone(), Some(Locale::SimplifiedChinese));
            assert!(config.error.as_deref().unwrap().contains("无法加载设置"));
            assert_eq!(config.settings.language, Language::Auto);
            assert!(config.save(Settings::default()).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
    }
}
