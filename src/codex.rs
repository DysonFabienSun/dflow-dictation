use crate::config::Settings;
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

pub fn login_status(path: &Path) -> Result<&'static str, String> {
    let login = read_login(path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if login.expiry.is_some_and(|expiry| expiry <= now) {
        Ok("ChatGPT login may have expired. Run codex login to renew it.")
    } else {
        Ok("ChatGPT login found.")
    }
}

fn read_login(path: &Path) -> Result<Login, String> {
    let file = fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "Codex login file not found. Run codex login or set the path in /provider.".to_owned()
        } else {
            "Could not read the Codex login file. Check its permissions and path.".to_owned()
        }
    })?;
    let auth: Value =
        serde_json::from_slice(&file).map_err(|_| "Invalid Codex login file. Run codex login.")?;
    let tokens = &auth["tokens"];
    let token = nonempty(&tokens["access_token"]).ok_or_else(|| {
        "A ChatGPT sign-in is required. Run codex login with your ChatGPT account.".to_owned()
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
}

impl Codex {
    pub fn new(settings: Settings) -> Result<Self, String> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(35));
        if settings.proxy.enabled {
            builder = builder.proxy(
                reqwest::Proxy::all(settings.proxy.url.as_str())
                    .map_err(|_| "Invalid proxy configuration. Check /proxy.")?,
            );
        }
        let client = builder
            .build()
            .map_err(|_| "Could not initialize the HTTPS client.")?;
        Ok(Self { client, settings })
    }
}

impl Transcriber for Codex {
    fn transcribe(&mut self, wav: Vec<u8>) -> Result<String, String> {
        let login = read_login(&self.settings.auth_file()?)?;
        let part = multipart::Part::bytes(wav)
            .file_name("in.wav")
            .mime_str("audio/wav")
            .map_err(|_| "Could not prepare the WAV upload.")?;
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
                "Codex transcription timed out. Try again."
            } else if self.settings.proxy.enabled {
                "Could not reach Codex transcription. Check /proxy and your connection."
            } else {
                "Could not reach Codex transcription. Check your connection."
            }
        })?;
        match response.status().as_u16() {
            200 => {}
            401 => return Err("Codex rejected the login. Run codex login and try again.".into()),
            403 => {
                return Err(
                    "Codex denied transcription. Check ChatGPT access and your connection.".into(),
                );
            }
            429 => {
                return Err("Codex transcription is rate limited. Try again in a moment.".into());
            }
            status => return Err(format!("Codex transcription failed (HTTP {status}).")),
        }
        let result: Value = serde_json::from_reader(response.take(1024 * 1024))
            .map_err(|_| "Codex returned an invalid transcription response.")?;
        result["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "Codex response did not contain a transcript.".into())
    }
}
