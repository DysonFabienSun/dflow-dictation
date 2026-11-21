use crate::hotkey::Hotkey;
use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Clone)]
pub struct Settings {
    pub device: Option<String>,
    pub hold_delay_ms: u32,
    pub auth_path: Option<PathBuf>,
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
            device: None,
            hold_delay_ms: DEFAULT_HOLD_DELAY_MS,
            auth_path: None,
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

pub fn parse_proxy_url(input: &str) -> Result<reqwest::Url, String> {
    let input = input.trim();
    let invalid =
        "Enter an http://, https://, or socks5:// proxy URL with a host and optional port.";
    let (_, address) = input.split_once("://").ok_or(invalid)?;
    let authority = address.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err("Proxy URLs must not include usernames or passwords.".into());
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
    pub fn auth_file(&self) -> Result<PathBuf, String> {
        if let Some(path) = &self.auth_path {
            return Ok(path.clone());
        }
        if let Some(home) = env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(home).join("auth.json"));
        }
        env::var_os("USERPROFILE")
            .filter(|s| !s.is_empty())
            .map(|home| PathBuf::from(home).join(".codex").join("auth.json"))
            .ok_or_else(|| "Set the Codex auth file in /provider.".into())
    }
}

pub struct Config {
    pub path: PathBuf,
    pub settings: Settings,
    pub error: Option<String>,
    document: DocumentMut,
    original: Option<String>,
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
        let loaded = read_config(&path)
            .map_err(|error| error.to_string())
            .and_then(|original| {
                let (document, settings) = parse(original.as_deref().unwrap_or_default())?;
                Ok((document, settings, original))
            });
        match loaded {
            Ok((document, settings, original)) => Self {
                path,
                settings,
                error: None,
                document,
                original,
            },
            Err(error) => Self {
                error: Some(format!("{}: {error}", path.display())),
                path,
                settings: Settings::default(),
                document: DocumentMut::new(),
                original: None,
            },
        }
    }

    pub fn save(&mut self, settings: Settings) -> Result<(), String> {
        self.save_with_tray(settings, None)
    }

    pub fn tray_hotkey(&self) -> Result<Hotkey, String> {
        string(table(&self.document, "tray")?, "hotkey")?
            .map(|text| text.parse().map_err(str::to_owned))
            .unwrap_or_else(|| Ok(Hotkey::default()))
    }

    pub fn tray_launch_preview(&self) -> Result<bool, String> {
        table(&self.document, "tray")?
            .and_then(|table| table.get("show_launch_preview"))
            .map(|item| {
                item.as_bool()
                    .ok_or_else(|| "tray.show_launch_preview must be true or false.".into())
            })
            .unwrap_or(Ok(true))
    }

    pub fn save_tray(
        &mut self,
        settings: Settings,
        hotkey: Hotkey,
        show_launch_preview: bool,
    ) -> Result<(), String> {
        self.save_with_tray(settings, Some((hotkey, show_launch_preview)))
    }

    fn save_with_tray(
        &mut self,
        settings: Settings,
        tray: Option<(Hotkey, bool)>,
    ) -> Result<(), String> {
        if self.error.is_some() {
            return Err(format!(
                "Correct {} and restart DFlow before saving.",
                self.path.display()
            ));
        }
        let mut document = self.document.clone();
        if let Some((hotkey, show_launch_preview)) = tray {
            table(&document, "tray")?;
            set_optional(&mut document, "tray", "hotkey", Some(&hotkey.to_string()));
            document["tray"]["show_launch_preview"] = value(show_launch_preview);
        }
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
        document["proxy"]["url"] = value(settings.proxy.url.as_str());
        document["proxy"]["enabled"] = value(settings.proxy.enabled);

        let text = document.to_string();
        let parent = self.path.parent().ok_or("Missing config directory.")?;
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
        if !save().map_err(|e| format!("Could not save {}: {e}", self.path.display()))? {
            return Err(format!(
                "{} changed on disk. Restart DFlow before saving.",
                self.path.display()
            ));
        }
        self.document = document;
        self.original = Some(text);
        self.settings = settings;
        Ok(())
    }
}

fn parse(text: &str) -> Result<(DocumentMut, Settings), String> {
    let document = text
        .parse::<DocumentMut>()
        .map_err(|error| error.message().to_owned())?;
    let audio = table(&document, "audio")?;
    let transcription = table(&document, "transcription")?;
    let proxy = table(&document, "proxy")?;
    if string(transcription, "provider")?.is_some_and(|provider| provider != "codex") {
        return Err("transcription.provider must be codex.".into());
    }
    let settings = Settings {
        device: string(audio, "device_id")?.map(str::to_owned),
        hold_delay_ms: audio
            .and_then(|table| table.get("hold_delay_ms"))
            .map(|item| {
                item.as_integer()
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or("audio.hold_delay_ms must be a positive integer in milliseconds.")
            })
            .transpose()?
            .unwrap_or(DEFAULT_HOLD_DELAY_MS),
        auth_path: string(transcription, "codex_auth_path")?.map(PathBuf::from),
        proxy: ProxySettings {
            url: parse_proxy_url(string(proxy, "url")?.unwrap_or(DEFAULT_PROXY))?,
            enabled: proxy
                .and_then(|table| table.get("enabled"))
                .map(|item| item.as_bool().ok_or("proxy.enabled must be true or false."))
                .transpose()?
                .unwrap_or(false),
        },
    };
    if settings
        .auth_path
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err("transcription.codex_auth_path must be an absolute path.".into());
    }
    Ok((document, settings))
}

fn table<'a>(
    doc: &'a DocumentMut,
    key: &str,
) -> Result<Option<&'a dyn toml_edit::TableLike>, String> {
    doc.get(key)
        .map(|item| {
            item.as_table_like()
                .ok_or_else(|| format!("{key} must be a table."))
        })
        .transpose()
}

fn string<'a>(
    table: Option<&'a dyn toml_edit::TableLike>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    table
        .and_then(|t| t.get(key))
        .map(|item| {
            item.as_str()
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| format!("{key} must be a nonempty string."))
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
                    url: parse_proxy_url("socks5://localhost:1080").unwrap(),
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
            assert!(parse_proxy_url(url).is_ok(), "{url}");
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
            assert!(parse_proxy_url(url).is_err(), "{url}");
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
}
