use crate::{codex, config::Settings, locale::Locale};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, time::Duration};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/dictation/custom_dictionary";

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct Dictionary {
    pub words: Vec<Entry>,
    pub max_words: usize,
    pub max_entry_utf16_units: usize,
    #[serde(skip)]
    account_id: Option<String>,
}

pub enum Change {
    Add(String),
    Remove(String),
}

impl Dictionary {
    pub fn load(settings: &Settings, locale: Locale, timeout: Duration) -> Result<Self, String> {
        let login = codex::settings_login(settings, locale)?;
        Self::load_with_login(settings, locale, timeout, &login)
    }

    pub(crate) fn load_with_login(
        settings: &Settings,
        locale: Locale,
        timeout: Duration,
        login: &codex::Login,
    ) -> Result<Self, String> {
        settings.require_dictation(locale)?;
        let response = request(
            settings,
            locale,
            timeout,
            login,
            Method::GET,
            ENDPOINT.parse().unwrap(),
            None,
        )?;
        let mut dictionary: Self =
            serde_json::from_reader(response.take(1024 * 1024)).map_err(|_| {
                locale.text(
                    "Voice Dictionary returned an invalid response.",
                    "语音词典返回了无效的响应",
                )
            })?;
        dictionary.account_id.clone_from(&login.account_id);
        Ok(dictionary)
    }

    pub fn apply(&self, settings: &Settings, locale: Locale, change: Change) -> Result<(), String> {
        let login = codex::settings_login(settings, locale)?;
        if self.account_id != login.account_id {
            return Err(locale
                .text(
                    "ChatGPT account changed. Review the reloaded dictionary before editing.",
                    "ChatGPT 账号已更改，请在编辑前查看重新加载的词典",
                )
                .into());
        }
        let (method, url, text) = self.mutation(locale, change)?;
        request(
            settings,
            locale,
            Duration::from_secs(10),
            &login,
            method,
            url,
            text.as_deref(),
        )
        .map(|_| ())
    }

    fn mutation(
        &self,
        locale: Locale,
        change: Change,
    ) -> Result<(Method, Url, Option<String>), String> {
        let mut url = Url::parse(&format!("{ENDPOINT}/words")).unwrap();
        let (method, text) = match change {
            Change::Add(text) => {
                let text = text.trim();
                if text.is_empty() || text.encode_utf16().count() > self.max_entry_utf16_units {
                    return Err(match locale {
                        Locale::English => format!(
                            "Enter a word or phrase of 1–{} UTF-16 units.",
                            self.max_entry_utf16_units
                        ),
                        Locale::SimplifiedChinese => format!(
                            "请输入占 1–{} 个 UTF-16 单元的词或短语",
                            self.max_entry_utf16_units
                        ),
                    });
                }
                if self.words.len() >= self.max_words {
                    return Err(locale
                        .text(
                            "Voice Dictionary is full. Remove an entry first.",
                            "语音词典已满，请先移除一个词条",
                        )
                        .into());
                }
                if self.words.iter().any(|word| word.text == text) {
                    return Err(locale
                        .text(
                            "This entry is already in your Voice Dictionary.",
                            "语音词典中已有该词条",
                        )
                        .into());
                }
                (Method::POST, Some(text.to_owned()))
            }
            Change::Remove(id) => {
                if !self.words.iter().any(|entry| entry.id == id) {
                    return Err(locale
                        .text(
                            "Reload the dictionary and select the entry again.",
                            "请重新加载词典并重新选择词条",
                        )
                        .into());
                }
                url.path_segments_mut().unwrap().push(&id);
                (Method::DELETE, None)
            }
        };
        Ok((method, url, text))
    }
}

fn request(
    settings: &Settings,
    locale: Locale,
    timeout: Duration,
    login: &codex::Login,
    method: Method,
    url: Url,
    text: Option<&str>,
) -> Result<reqwest::blocking::Response, String> {
    let mut request = codex::client(&settings.proxy, locale, timeout)?
        .request(method, url)
        .bearer_auth(&login.access_token);
    if let Some(account_id) = &login.account_id {
        request = request.header("chatgpt-account-id", account_id);
    }
    if let Some(text) = text {
        request = request.json(&json!({ "text": text }));
    }
    let response = request.send().map_err(|error| {
        if error.is_timeout() {
            locale.text("Voice Dictionary timed out.", "语音词典请求超时")
        } else {
            locale.text(
                "Could not reach Voice Dictionary. Check the connection and proxy.",
                "无法连接语音词典，请检查网络连接和代理设置",
            )
        }
    })?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        401 | 403 => Err(locale.text("Voice Dictionary access was denied. Check your ChatGPT account and run codex login.", "语音词典访问被拒绝，请检查 ChatGPT 账号并运行 codex login").into()),
        status => Err(match locale {
            Locale::English => format!("Voice Dictionary request failed (HTTP {status})."),
            Locale::SimplifiedChinese => format!("语音词典请求失败（HTTP {status}）"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_mutations_use_server_limits_and_entry_ids() {
        let mut dictionary: Dictionary = serde_json::from_value(json!({
            "max_words": 2, "max_entry_utf16_units": 4,
            "words": [{ "id": "server/id", "text": "Test" }]
        }))
        .unwrap();
        let locale = Locale::English;
        let (method, url, text) = dictionary
            .mutation(locale, Change::Add("  🌲大法  ".into()))
            .unwrap();
        assert_eq!(method, Method::POST);
        assert_eq!(url.as_str(), format!("{ENDPOINT}/words"));
        assert_eq!(text.as_deref(), Some("🌲大法"));
        assert!(
            dictionary
                .mutation(locale, Change::Add("🌲🌲🌲".into()))
                .is_err()
        );
        assert!(
            dictionary
                .mutation(locale, Change::Add("Test".into()))
                .is_err()
        );
        assert!(
            dictionary
                .mutation(locale, Change::Remove("Test".into()))
                .is_err()
        );
        let (method, url, text) = dictionary
            .mutation(locale, Change::Remove("server/id".into()))
            .unwrap();
        assert_eq!(method, Method::DELETE);
        assert!(url.path().ends_with("/words/server%2Fid"));
        assert!(text.is_none());
        dictionary.max_words = 1;
        assert!(
            dictionary
                .mutation(locale, Change::Add("New".into()))
                .is_err()
        );
    }
}
