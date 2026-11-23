use crate::{
    audio::{Session, Update},
    codex,
    config::{ProxySettings, Settings},
    dictionary::{Change, Dictionary},
    locale::Locale,
};
use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

pub fn consent_text(locale: Locale) -> &'static str {
    locale.text(
        "Dictate with a keyboard shortcut. Cloud dictation uses your Codex sign-in and sends microphone audio to OpenAI at chatgpt.com.",
        "按快捷键开始听写。云端听写使用现有的 Codex 登录信息，将麦克风录音发送到 OpenAI 的 chatgpt.com。",
    )
}

pub fn availability_hint(locale: Locale) -> &'static str {
    locale.text(
        "Check the connection and Codex CLI here. To verify account access, enable dictation and run Test dictation.",
        "启用听写前可以检查连接和 Codex CLI。要确认账号权限，请启用听写并运行「测试听写」。",
    )
}

#[derive(Clone, Copy)]
pub enum NetworkTarget {
    ChatGpt,
    Proxy,
}

impl NetworkTarget {
    pub fn label(self, locale: Locale) -> &'static str {
        match self {
            Self::ChatGpt => locale.text("Check ChatGPT availability", "检查 ChatGPT 可用性"),
            Self::Proxy => locale.text("Check proxy", "检查代理"),
        }
    }
}

#[derive(Default)]
pub struct NetworkCheck {
    pub status: String,
    worker: Option<Receiver<String>>,
}

impl NetworkCheck {
    pub fn checking(&self) -> bool {
        self.worker.is_some()
    }

    pub fn start(&mut self, mut proxy: ProxySettings, target: NetworkTarget, locale: Locale) {
        if self.checking() {
            return;
        }
        proxy.enabled |= matches!(target, NetworkTarget::Proxy);
        self.status = locale
            .text("Checking chatgpt.com…", "正在检查 chatgpt.com…")
            .into();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        thread::spawn(move || {
            let route = if proxy.enabled {
                locale.text("Via proxy", "通过代理")
            } else {
                locale.text("Direct", "直连")
            };
            let result = match codex::client(&proxy, locale, Duration::from_secs(6)) {
                Ok(client) => match client.head("https://chatgpt.com/").send() {
                    Ok(response)
                        if response.status().is_success() || response.status().is_redirection() =>
                    {
                        locale
                            .text("chatgpt.com responded.", "chatgpt.com 已响应。")
                            .into()
                    }
                    Ok(response) => match locale {
                        Locale::English => {
                            format!("chatgpt.com returned HTTP {}.", response.status().as_u16())
                        }
                        Locale::SimplifiedChinese => {
                            format!("chatgpt.com 返回 HTTP {}。", response.status().as_u16())
                        }
                    },
                    Err(error) if error.is_timeout() => locale
                        .text(
                            "Connection to chatgpt.com timed out.",
                            "连接 chatgpt.com 超时。",
                        )
                        .into(),
                    Err(_) => locale
                        .text(
                            "Could not reach chatgpt.com. Check the connection or proxy address.",
                            "无法连接 chatgpt.com，请检查网络连接或代理地址。",
                        )
                        .into(),
                },
                Err(error) => error,
            };
            let mut status = format!("{route} · {result}");
            if matches!(target, NetworkTarget::ChatGpt) {
                status.push(' ');
                status.push_str(if codex::find_codex().is_some() {
                    locale.text("Codex CLI found.", "已找到 Codex CLI。")
                } else {
                    locale.text(
                        "Install Codex CLI and add it to PATH.",
                        "请安装 Codex CLI 并将其加入 PATH。",
                    )
                });
            }
            let _ = send.send(status);
        });
    }

    pub fn poll(&mut self, locale: Locale) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        self.status = match worker.try_recv() {
            Ok(status) => status,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => locale
                .text("Connection check ended unexpectedly.", "连接检查意外中断。")
                .into(),
        };
        self.worker = None;
        true
    }
}

pub fn cleanup_disclosure(locale: Locale) -> &'static str {
    locale.text(
        "Each cleanup runs one GPT-6 Luna request and uses additional tokens from your ChatGPT plan’s Codex allowance. The amount varies with the transcript and vocabulary. Applicable credits or billing may apply.",
        "每次整理会发起一次 GPT-6 Luna 请求，额外消耗 ChatGPT 套餐中 Codex 的 token 额度。用量取决于转写文本和词表，可能消耗积分或产生费用。",
    )
}

pub fn dictionary_disclosure(locale: Locale) -> &'static str {
    locale.text(
        "Changes to your ChatGPT Voice Dictionary also affect dictation in ChatGPT.",
        "修改 ChatGPT 语音词典也会影响 ChatGPT 中的听写。",
    )
}

fn login_hint(locale: Locale) -> &'static str {
    locale.text(
        "Install Codex CLI and run codex login with your ChatGPT account.",
        "请安装 Codex CLI，并运行 codex login 使用 ChatGPT 账号登录。",
    )
}

enum ResultOfWork {
    Login(String),
    Dictionary(Result<Dictionary, String>, Option<String>),
}

#[derive(Default)]
pub struct Setup {
    pub login: String,
    pub dictionary: Option<Dictionary>,
    pub dictionary_status: String,
    pub test_result: String,
    test: Option<Session>,
    work: Option<Receiver<ResultOfWork>>,
    test_text: String,
    stopping: bool,
}

impl Setup {
    pub fn busy(&self) -> bool {
        self.work.is_some() || self.testing()
    }

    pub fn testing(&self) -> bool {
        self.test.is_some()
    }

    pub fn stopping(&self) -> bool {
        self.stopping
    }

    fn worker(&mut self, work: impl FnOnce() -> ResultOfWork + Send + 'static) {
        let (send, receive) = mpsc::channel();
        self.work = Some(receive);
        thread::spawn(move || {
            let _ = send.send(work());
        });
    }

    pub fn check_login(&mut self, settings: Settings, locale: Locale) {
        if self.busy() {
            return;
        }
        self.login = locale.text("Checking sign-in…", "正在检查登录…").into();
        self.worker(move || {
            ResultOfWork::Login(match codex::login_status(&settings, locale) {
                Ok(status) => status.into(),
                Err(error) => format!("{error} {}", login_hint(locale)),
            })
        });
    }

    pub fn dictionary(&mut self, settings: Settings, locale: Locale, change: Option<Change>) {
        if self.busy() || (change.is_some() && self.dictionary.is_none()) {
            return;
        }
        let previous = self.dictionary.take();
        self.dictionary_status = locale
            .text("Loading Voice Dictionary…", "正在加载语音词典…")
            .into();
        self.worker(move || {
            let timeout = Duration::from_secs(10);
            // Always reload after a write. A connection failure can occur after the
            // server commits it, so retrying against the old list could repeat it.
            let write_error = match (previous, change) {
                (Some(dictionary), Some(change)) => {
                    dictionary.apply(&settings, locale, change).err()
                }
                _ => None,
            };
            ResultOfWork::Dictionary(Dictionary::load(&settings, locale, timeout), write_error)
        });
    }

    pub fn test(&mut self, settings: Settings, locale: Locale) {
        if let Some(session) = &self.test {
            session.stop();
            self.stopping = true;
            self.test_result = locale.text("Finishing test…", "正在完成测试…").into();
        } else if !self.busy() {
            self.test_text.clear();
            self.test = Some(Session::test_dictation(settings, locale));
            self.test_result = locale
                .text("Starting microphone…", "正在启动麦克风…")
                .into();
        }
    }

    pub fn poll(&mut self, locale: Locale) -> bool {
        let mut changed = false;
        if let Some(worker) = &self.work {
            match worker.try_recv() {
                Ok(ResultOfWork::Login(status)) => {
                    self.login = status;
                    self.work = None;
                    changed = true;
                }
                Ok(ResultOfWork::Dictionary(result, write_error)) => {
                    self.dictionary_status = match result {
                        Ok(list) => {
                            let count = format!("{}/{}", list.words.len(), list.max_words);
                            self.dictionary = Some(list);
                            write_error.map_or(count.clone(), |error| format!("{error} · {count}"))
                        }
                        Err(error) => write_error
                            .map_or_else(|| error.clone(), |write| format!("{write} {error}")),
                    };
                    self.work = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let error = locale.text("Setup task ended unexpectedly.", "设置任务意外中断");
                    self.login = error.into();
                    self.dictionary_status = error.into();
                    self.work = None;
                    changed = true;
                }
            }
        }
        while let Some(session) = &self.test {
            let update = match session.updates.try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => Update::Failed(
                    locale
                        .text("Test dictation ended unexpectedly.", "听写测试意外中断")
                        .into(),
                ),
            };
            changed = true;
            match update {
                Update::Live if !self.stopping => {
                    self.test_result = locale
                        .text("Recording test audio…", "正在录制测试音频…")
                        .into()
                }
                Update::Transcript(text) => self.test_text = text,
                Update::Finishing => {
                    self.stopping = true;
                    self.test_result = locale.text("Finishing test…", "正在完成测试…").into();
                }
                Update::Done => {
                    self.test = None;
                    self.stopping = false;
                    self.test_result = if self.test_text.trim().is_empty() {
                        locale
                            .text(
                                "No speech detected. Try the test again.",
                                "未检测到语音，请重新测试",
                            )
                            .into()
                    } else {
                        std::mem::take(&mut self.test_text)
                    };
                }
                Update::Failed(error) => {
                    self.test = None;
                    self.stopping = false;
                    self.test_result = error;
                }
                _ => {}
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    #[test]
    fn proxy_check_uses_the_configured_route_even_when_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = String::new();
            BufReader::new(&mut connection)
                .read_line(&mut request)
                .unwrap();
            connection
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            send.send(request).unwrap();
        });
        let proxy = ProxySettings {
            url: format!("http://{address}").parse().unwrap(),
            enabled: false,
        };
        let mut check = NetworkCheck::default();
        check.start(proxy, NetworkTarget::Proxy, Locale::English);
        let request = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(request.trim_end(), "CONNECT chatgpt.com:443 HTTP/1.1");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !check.poll(Locale::English) {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            check
                .status
                .starts_with("Via proxy · Could not reach chatgpt.com")
        );
    }

    #[test]
    fn test_transcript_is_published_in_setup_only_after_completion() {
        let locale = Locale::English;
        let (session, send) = Session::test_channel();
        let mut setup = Setup {
            test: Some(session),
            ..Setup::default()
        };
        send.send(Update::Transcript("A partial.".into())).unwrap();
        setup.poll(locale);
        assert!(setup.test_result.is_empty());
        send.send(Update::Transcript("The completed test.".into()))
            .unwrap();
        send.send(Update::Done).unwrap();
        assert!(setup.poll(locale));
        assert_eq!(setup.test_result, "The completed test.");
        assert!(!setup.busy());
        assert!(setup.test_text.is_empty());
        assert!(setup.dictionary.is_none());
    }
}
