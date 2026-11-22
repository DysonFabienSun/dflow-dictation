use crate::{config::Settings, locale::Locale};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    blocking::{Client, multipart},
    redirect,
};
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/transcribe";
const ACCOUNT_CLAIM: &str = "https://api.openai.com/auth";

struct Login {
    access_token: String,
    account_id: Option<String>,
    expiry: Option<u64>,
}

pub fn login_status(path: &Path, locale: Locale) -> Result<&'static str, String> {
    let login = read_login(path, locale)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if login.expiry.is_some_and(|expiry| expiry <= now) {
        Ok(locale.text(
            "ChatGPT login may have expired. Run codex login to renew it.",
            "ChatGPT 登录可能已过期，请运行 codex login 重新登录",
        ))
    } else {
        Ok(locale.text("ChatGPT login found.", "已找到 ChatGPT 登录信息"))
    }
}

fn read_login(path: &Path, locale: Locale) -> Result<Login, String> {
    let file = fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            locale
                .text(
                    "Codex login file not found. Run codex login or set the path in /provider.",
                    "找不到 Codex 登录文件，请运行 codex login 或在设置中指定路径",
                )
                .to_owned()
        } else {
            locale
                .text(
                    "Could not read the Codex login file. Check its permissions and path.",
                    "无法读取 Codex 登录文件，请检查路径和文件权限",
                )
                .to_owned()
        }
    })?;
    let auth: Value = serde_json::from_slice(&file).map_err(|_| {
        locale.text(
            "Invalid Codex login file. Run codex login.",
            "Codex 登录文件无效，请运行 codex login",
        )
    })?;
    let tokens = &auth["tokens"];
    let token = nonempty(&tokens["access_token"]).ok_or_else(|| {
        locale
            .text(
                "A ChatGPT sign-in is required. Run codex login with your ChatGPT account.",
                "需要登录 ChatGPT，请运行 codex login 并使用 ChatGPT 账号登录",
            )
            .to_owned()
    })?;
    let claims = jwt_claims(token);
    let id_claims = nonempty(&tokens["id_token"]).map_or(Value::Null, jwt_claims);
    let account_id = nonempty(&tokens["account_id"])
        .or_else(|| nonempty(&claims[ACCOUNT_CLAIM]["chatgpt_account_id"]))
        .or_else(|| nonempty(&id_claims[ACCOUNT_CLAIM]["chatgpt_account_id"]));
    Ok(Login {
        access_token: token.to_owned(),
        account_id: account_id.map(str::to_owned),
        expiry: claims["exp"].as_u64(),
    })
}

fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or(Value::Null)
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

pub trait Transcriber {
    fn transcribe(&mut self, wav: Vec<u8>) -> Result<String, String>;
}

pub struct Codex {
    client: Client,
    settings: Settings,
    locale: Locale,
}

impl Codex {
    pub fn new(settings: Settings, locale: Locale) -> Result<Self, String> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(35));
        if settings.proxy.enabled {
            builder = builder.proxy(reqwest::Proxy::all(settings.proxy.url.as_str()).map_err(
                |_| {
                    locale.text(
                        "Invalid proxy configuration. Check /proxy.",
                        "代理配置无效，请检查网络代理设置",
                    )
                },
            )?);
        }
        let client = builder.build().map_err(|_| {
            locale.text(
                "Could not initialize the HTTPS client.",
                "无法初始化 HTTPS 客户端",
            )
        })?;
        Ok(Self {
            client,
            settings,
            locale,
        })
    }
}

impl Transcriber for Codex {
    fn transcribe(&mut self, wav: Vec<u8>) -> Result<String, String> {
        let locale = self.locale;
        let login = read_login(&self.settings.auth_file(locale)?, locale)?;
        let part = multipart::Part::bytes(wav)
            .file_name("in.wav")
            .mime_str("audio/wav")
            .map_err(|_| {
                locale.text("Could not prepare the WAV upload.", "无法准备 WAV 音频上传")
            })?;
        let mut request = self
            .client
            .post(ENDPOINT)
            .bearer_auth(login.access_token)
            .header("originator", "dsh-voice")
            .header("User-Agent", "dsh-voice")
            .multipart(multipart::Form::new().part("file", part));
        if let Some(account_id) = login.account_id {
            request = request.header("chatgpt-account-id", account_id);
        }
        let response = request.send().map_err(|error| {
            if error.is_timeout() {
                locale.text(
                    "Codex transcription timed out. Try again.",
                    "转写请求超时，请重试",
                )
            } else if self.settings.proxy.enabled {
                locale.text(
                    "Could not reach Codex transcription. Check /proxy and your connection.",
                    "无法连接 ChatGPT 转写服务，请检查网络连接和代理设置",
                )
            } else {
                locale.text(
                    "Could not reach Codex transcription. Check your connection.",
                    "无法连接 ChatGPT 转写服务，请检查网络连接",
                )
            }
        })?;
        match response.status().as_u16() {
            200 => {}
            401 => {
                return Err(locale
                    .text(
                        "Codex rejected the login. Run codex login and try again.",
                        "登录信息无效，请运行 codex login 后重试",
                    )
                    .into());
            }
            403 => {
                return Err(locale
                    .text(
                        "Codex denied transcription. Check ChatGPT access and your connection.",
                        "转写请求被拒绝，请检查 ChatGPT 访问权限和网络连接",
                    )
                    .into());
            }
            429 => {
                return Err(locale
                    .text(
                        "Codex transcription is rate limited. Try again in a moment.",
                        "转写请求过于频繁，请稍后重试",
                    )
                    .into());
            }
            status => {
                return Err(match locale {
                    Locale::English => format!("Codex transcription failed (HTTP {status})."),
                    Locale::SimplifiedChinese => format!("转写失败（HTTP {status}）"),
                });
            }
        }
        let result: Value = serde_json::from_reader(response.take(1024 * 1024)).map_err(|_| {
            locale.text(
                "Codex returned an invalid transcription response.",
                "转写服务返回了无效的响应",
            )
        })?;
        result["text"].as_str().map(str::to_owned).ok_or_else(|| {
            locale
                .text(
                    "Codex response did not contain a transcript.",
                    "转写服务未返回转写结果",
                )
                .into()
        })
    }
}
