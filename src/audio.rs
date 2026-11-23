use crate::{
    codex::{Codex, Transcriber, login_status},
    config::{Config, Settings},
    locale::Locale,
    transcript::{Transcript, clean_text},
};
use cpal::{
    Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const RECORDING: u8 = 0;
const STOPPING: u8 = 1;
const CANCELLED: u8 = 2;
const WAV_RATE: u32 = 16_000;
const MAX_CHUNK_SECONDS: usize = 7;
const MIN_CHUNK_MS: usize = 1_000;
const PAUSE_MS: usize = 350;
const SPEECH_FRAME_MS: usize = 20;
const FINAL_SPEECH_MIN_MS: usize = 60;
const MIN_SPEECH_ENERGY: f32 = 0.00001;
const QUIET_FOR: Duration = Duration::from_secs(5);
const MIN_AUDIO_PEAK: f32 = 0.001;
static REQUEST_SLOT: Mutex<()> = Mutex::new(());

pub struct InputDevice {
    pub id: String,
    pub label: String,
}

pub fn devices(locale: Locale) -> Result<Vec<InputDevice>, String> {
    let host = cpal::default_host();
    let mut devices: Vec<_> = host
        .input_devices()
        .map_err(|_| {
            locale
                .text("Could not list microphone inputs.", "无法获取麦克风列表")
                .to_owned()
        })?
        .filter_map(|device| {
            Some(InputDevice {
                id: device.id().ok()?.to_string(),
                label: device.description().ok()?.name().to_owned(),
            })
        })
        .collect();
    devices.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(devices)
}

#[derive(Debug)]
pub enum Update {
    Live,
    QuietInput(bool),
    Finishing,
    Transcript(String),
    Cleaning,
    Cleaned(String),
    CleanupFailed(String),
    Done,
    Failed(String),
}

pub struct Session {
    pub updates: Receiver<Update>,
    state: Arc<AtomicU8>,
}

impl Session {
    pub fn test_dictation(mut settings: Settings, locale: Locale) -> Self {
        settings.cleanup = false;
        Self::start(settings, locale)
    }

    pub fn start(settings: Settings, locale: Locale) -> Self {
        Self::start_with(move || Ok(settings), locale)
    }

    pub fn start_from_config(path: PathBuf, locale: Locale) -> Self {
        Self::start_with(
            move || {
                let config = Config::load_for(path, Some(locale));
                config.error.map_or(Ok(config.settings), Err)
            },
            locale,
        )
    }

    fn start_with(
        settings: impl FnOnce() -> Result<Settings, String> + Send + 'static,
        locale: Locale,
    ) -> Self {
        let (updates, result) = mpsc::channel();
        let state = Arc::new(AtomicU8::new(RECORDING));
        let worker_state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(error) =
                settings().and_then(|settings| record(settings, &worker_state, &updates, locale))
            {
                report_failure(&worker_state, &updates, error);
            }
        });
        Self {
            updates: result,
            state,
        }
    }

    pub fn stop(&self) {
        let _ =
            self.state
                .compare_exchange(RECORDING, STOPPING, Ordering::AcqRel, Ordering::Acquire);
    }

    #[doc(hidden)]
    pub fn test_channel() -> (Self, Sender<Update>) {
        let (send, updates) = mpsc::channel();
        let state = Arc::new(AtomicU8::new(RECORDING));
        (Self { updates, state }, send)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.state.store(CANCELLED, Ordering::Release);
    }
}

fn record(
    settings: Settings,
    state: &Arc<AtomicU8>,
    updates: &Sender<Update>,
    locale: Locale,
) -> Result<(), String> {
    if state.load(Ordering::Acquire) != RECORDING {
        let _ = updates.send(Update::Done);
        return Ok(());
    }
    login_status(&settings, locale)?;
    let host = cpal::default_host();
    let device = match settings.device.as_deref() {
        None => host.default_input_device().ok_or(locale.text(
            "No default microphone. Choose one in Settings.",
            "没有默认麦克风，请在设置中选择",
        ))?,
        Some(id) => {
            let id = id.parse().map_err(|_| {
                locale.text(
                    "Invalid saved microphone ID. Choose one in Settings.",
                    "保存的麦克风 ID 无效，请在设置中重新选择",
                )
            })?;
            host.device_by_id(&id).ok_or(locale.text(
                "The saved microphone is unavailable. Choose one in Settings.",
                "所选麦克风不可用，请在设置中重新选择",
            ))?
        }
    };
    let supported = device.default_input_config().map_err(|_| {
        locale.text("Could not open the selected microphone. Check microphone access or choose another in Settings.", "无法打开所选麦克风，请检查麦克风权限或在设置中选择其他设备")
    })?;
    let format = supported.sample_format();
    let config = supported.config();
    let (samples_tx, samples_rx) = mpsc::sync_channel(48);
    let stream_failed = Arc::new(AtomicBool::new(false));
    let stream = microphone(
        &device,
        &config,
        format,
        samples_tx,
        Arc::clone(&stream_failed),
        locale,
    )?;

    if state.load(Ordering::Acquire) != RECORDING {
        let _ = updates.send(Update::Done);
        return Ok(());
    }
    stream
        .play()
        .map_err(|_| locale.text("Could not start the microphone.", "无法启动麦克风"))?;
    let _ = updates.send(Update::Live);

    let (chunk_tx, chunk_rx) = mpsc::sync_channel(8);
    let upload_updates = updates.clone();
    let upload_state = Arc::clone(state);
    thread::spawn(move || match Codex::new(settings.clone(), locale) {
        Ok(codex) => upload(
            codex,
            chunk_rx,
            &upload_updates,
            &upload_state,
            locale,
            |raw| {
                complete_recording(settings.cleanup, raw, &upload_updates, |raw| {
                    crate::cleanup::run(&settings, locale, raw, || {
                        upload_state.load(Ordering::Acquire) == CANCELLED
                    })
                })
            },
        ),
        Err(error) => report_failure(&upload_state, &upload_updates, error),
    });

    let capture = || -> Result<(), String> {
        let rate = config.sample_rate as usize;
        let mut chunker = Chunker::new(rate, locale);
        let started = Instant::now();
        let mut monitor = InputMonitor::new(started);
        while state.load(Ordering::Acquire) == RECORDING
            && started.elapsed() < Duration::from_secs(300)
        {
            if stream_failed.load(Ordering::Acquire) {
                return Err(locale
                    .text("Microphone capture was interrupted.", "麦克风录音已中断")
                    .into());
            }
            match samples_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(samples) => {
                    check_input(&mut monitor, &samples, updates);
                    chunker.push(samples, &chunk_tx)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => check_input(&mut monitor, &[], updates),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(locale
                        .text("Microphone capture was interrupted.", "麦克风录音已中断")
                        .into());
                }
            }
        }
        drop(stream);
        if state.load(Ordering::Acquire) == CANCELLED {
            return Ok(());
        }
        let _ = updates.send(Update::Finishing);
        for samples in samples_rx.try_iter() {
            chunker.push(samples, &chunk_tx)?;
        }
        chunker.finish(&chunk_tx)
    };
    if let Err(error) = capture() {
        report_failure(state, updates, error);
    }
    Ok(())
}

fn complete_recording(
    enabled: bool,
    raw: &str,
    updates: &Sender<Update>,
    cleanup: impl FnOnce(&str) -> Result<String, String>,
) -> Update {
    if !enabled || raw.trim().is_empty() || updates.send(Update::Cleaning).is_err() {
        return Update::Done;
    }
    match cleanup(raw) {
        Ok(text) => Update::Cleaned(text),
        Err(error) => Update::CleanupFailed(error),
    }
}

struct InputMonitor {
    last_audible: Instant,
    quiet: bool,
}

impl InputMonitor {
    fn new(now: Instant) -> Self {
        Self {
            last_audible: now,
            quiet: false,
        }
    }

    fn observe(&mut self, now: Instant, samples: &[f32]) -> Option<bool> {
        if samples.iter().any(|sample| sample.abs() >= MIN_AUDIO_PEAK) {
            self.last_audible = now;
        }
        let quiet = now.duration_since(self.last_audible) >= QUIET_FOR;
        if quiet == self.quiet {
            return None;
        }
        self.quiet = quiet;
        Some(quiet)
    }
}

fn check_input(monitor: &mut InputMonitor, samples: &[f32], updates: &Sender<Update>) {
    if let Some(quiet) = monitor.observe(Instant::now(), samples) {
        let _ = updates.send(Update::QuietInput(quiet));
    }
}

fn report_failure(state: &AtomicU8, updates: &Sender<Update>, error: String) {
    if state.swap(CANCELLED, Ordering::AcqRel) != CANCELLED {
        let _ = updates.send(Update::Failed(error));
    }
}

fn upload(
    mut provider: impl Transcriber,
    chunks: Receiver<Vec<i16>>,
    updates: &Sender<Update>,
    state: &AtomicU8,
    locale: Locale,
    on_completed: impl FnOnce(&str) -> Update,
) {
    let mut transcript = Transcript::default();
    let mut previous = Vec::new();
    let mut pending = None;
    while let Some(mut current) = pending.take().or_else(|| chunks.recv().ok()) {
        if state.load(Ordering::Acquire) == CANCELLED {
            return;
        }
        // If the backend was busy, send the waiting audio together up to the
        // normal chunk limit instead of paying for each small request in turn.
        while let Ok(next) = chunks.try_recv() {
            if current.len() + next.len() > WAV_RATE as usize * MAX_CHUNK_SECONDS {
                pending = Some(next);
                break;
            }
            current.extend(next);
        }
        match transcribe_chunk(
            &mut provider,
            &previous,
            &current,
            &mut transcript,
            state,
            locale,
        ) {
            Ok(true) => {
                if updates
                    .send(Update::Transcript(transcript.text().to_owned()))
                    .is_err()
                {
                    return;
                }
            }
            Ok(false) => return,
            Err(error) => {
                report_failure(state, updates, error);
                return;
            }
        }
        previous = current;
    }
    if state.load(Ordering::Acquire) != CANCELLED {
        let completed = on_completed(transcript.text());
        if state.load(Ordering::Acquire) != CANCELLED {
            let _ = updates.send(completed);
        }
    }
}

fn transcribe_chunk(
    provider: &mut impl Transcriber,
    previous: &[i16],
    current: &[i16],
    transcript: &mut Transcript,
    state: &AtomicU8,
    locale: Locale,
) -> Result<bool, String> {
    let combined = if transcript.has_tail() {
        let Some(combined) =
            request_transcript(provider, previous.iter().chain(current), state, locale)?
        else {
            return Ok(false);
        };
        if transcript.merge(&combined, None) {
            return Ok(true);
        }
        Some(combined)
    } else {
        None
    };

    let Some(text) = request_transcript(provider, current.iter(), state, locale)? else {
        return Ok(false);
    };
    if !combined.is_some_and(|combined| transcript.merge(&combined, Some(&text))) {
        transcript.append(&text);
    }
    Ok(true)
}

fn request_transcript<'a>(
    provider: &mut impl Transcriber,
    samples: impl Iterator<Item = &'a i16>,
    state: &AtomicU8,
    locale: Locale,
) -> Result<Option<String>, String> {
    let wav = wav(samples.copied())
        .map_err(|_| locale.text("Could not encode microphone audio.", "无法编码麦克风音频"))?;
    let slot = REQUEST_SLOT.lock().map_err(|_| {
        locale.text(
            "The transcription worker stopped unexpectedly.",
            "转写任务意外中断",
        )
    })?;
    if state.load(Ordering::Acquire) == CANCELLED {
        return Ok(None);
    }
    let result = provider.transcribe(wav);
    drop(slot);
    if state.load(Ordering::Acquire) == CANCELLED {
        return Ok(None);
    }
    result.map(|text| Some(clean_text(&text)))
}

fn microphone(
    device: &Device,
    config: &StreamConfig,
    format: SampleFormat,
    samples: SyncSender<Vec<f32>>,
    failed: Arc<AtomicBool>,
    locale: Locale,
) -> Result<Stream, String> {
    let stream = match format {
        SampleFormat::F32 => input::<f32>(device, config, samples, failed),
        SampleFormat::F64 => input::<f64>(device, config, samples, failed),
        SampleFormat::I16 => input::<i16>(device, config, samples, failed),
        SampleFormat::I24 => input::<cpal::I24>(device, config, samples, failed),
        SampleFormat::I32 => input::<i32>(device, config, samples, failed),
        SampleFormat::U8 => input::<u8>(device, config, samples, failed),
        SampleFormat::U16 => input::<u16>(device, config, samples, failed),
        _ => {
            return Err(locale
                .text(
                    "This microphone's sample format is unsupported. Choose another in Settings.",
                    "不支持此麦克风的采样格式，请在设置中选择其他设备",
                )
                .into());
        }
    };
    stream.map_err(|_| {
        locale
            .text(
                "Could not start microphone capture. Check microphone permissions.",
                "无法开始录音，请检查麦克风权限",
            )
            .into()
    })
}

fn input<T: SizedSample>(
    device: &Device,
    config: &StreamConfig,
    samples: SyncSender<Vec<f32>>,
    failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError>
where
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let overflow = Arc::clone(&failed);
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mono = data
                .chunks_exact(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            if let Err(mpsc::TrySendError::Full(_)) = samples.try_send(mono) {
                overflow.store(true, Ordering::Release);
            }
        },
        move |_| failed.store(true, Ordering::Release),
        None,
    )
}

struct Chunker {
    rate: usize,
    locale: Locale,
    samples: Vec<f32>,
}

impl Chunker {
    fn new(rate: usize, locale: Locale) -> Self {
        Self {
            rate,
            locale,
            samples: Vec::with_capacity(rate * MAX_CHUNK_SECONDS),
        }
    }

    fn push(&mut self, samples: Vec<f32>, queue: &SyncSender<Vec<i16>>) -> Result<(), String> {
        self.samples.extend(samples);
        let limit = self.rate * MAX_CHUNK_SECONDS;
        while self.samples.len() >= limit {
            let chunk: Vec<_> = self.samples.drain(..limit).collect();
            send_chunk(&chunk, self.rate, queue, self.locale)?;
        }
        let pause_samples = self.rate * PAUSE_MS / 1_000;
        if self.samples.len() >= self.rate * MIN_CHUNK_MS / 1_000 {
            let trailing_pause = self.samples.len() - pause_samples;
            if !has_speech(&self.samples[trailing_pause..], self.rate) {
                if has_speech(&self.samples[..trailing_pause], self.rate) {
                    self.flush(queue)?;
                } else {
                    // Keep a little audio before the next onset.
                    self.samples.drain(..trailing_pause);
                }
            }
        }
        Ok(())
    }

    fn flush(&mut self, queue: &SyncSender<Vec<i16>>) -> Result<(), String> {
        let result = send_chunk(&self.samples, self.rate, queue, self.locale);
        self.samples.clear();
        result
    }

    fn finish(&mut self, queue: &SyncSender<Vec<i16>>) -> Result<(), String> {
        // A shortcut click can trip one energy frame in an otherwise quiet tail.
        let required = self.rate * FINAL_SPEECH_MIN_MS / 1_000;
        let mut voiced = 0;
        if speech_frames(&self.samples, self.rate).any(|(active, len)| {
            voiced = if active { voiced + len } else { 0 };
            voiced >= required
        }) {
            self.flush(queue)?;
        }
        Ok(())
    }
}

fn send_chunk(
    samples: &[f32],
    rate: usize,
    queue: &SyncSender<Vec<i16>>,
    locale: Locale,
) -> Result<(), String> {
    if !has_speech(samples, rate) {
        return Ok(());
    }
    let output_count = (samples.len() * WAV_RATE as usize).div_ceil(rate);
    let pcm = (0..output_count)
        .map(|index| {
            let start = index * rate / WAV_RATE as usize;
            let end = ((index + 1) * rate / WAV_RATE as usize).clamp(start + 1, samples.len());
            let sample = samples[start..end].iter().sum::<f32>() / (end - start) as f32;
            (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })
        .collect();
    queue.try_send(pcm).map_err(|_| {
        locale
            .text(
                "Transcription could not keep up. Try a shorter dictation.",
                "转写速度跟不上录音，请缩短单次听写时间",
            )
            .into()
    })
}

fn has_speech(samples: &[f32], rate: usize) -> bool {
    speech_frames(samples, rate).any(|(active, _)| active)
}

fn speech_frames(samples: &[f32], rate: usize) -> impl Iterator<Item = (bool, usize)> {
    let frame_samples = (rate * SPEECH_FRAME_MS / 1_000).max(1);
    samples.chunks(frame_samples).map(|frame| {
        let energy = frame.iter().map(|sample| sample * sample).sum::<f32>();
        (
            energy >= MIN_SPEECH_ENERGY * frame.len() as f32,
            frame.len(),
        )
    })
}

fn wav(samples: impl IntoIterator<Item = i16>) -> Result<Vec<u8>, hound::Error> {
    let mut bytes = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: WAV_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(&mut bytes, spec)?;
    for sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_checks_consent_before_opening_the_login_file() {
        for cloud_dictation in [None, Some(false)] {
            let settings = Settings {
                auth_path: Some(PathBuf::from("missing-auth.json")),
                cloud_dictation,
                ..Settings::default()
            };
            let session = Session::start(settings, Locale::English);
            assert!(matches!(
                session.updates.recv_timeout(Duration::from_secs(2)).unwrap(),
                Update::Failed(error) if error.contains("Cloud dictation is off")
            ));
        }
    }

    #[test]
    fn cleanup_is_called_once_for_successful_nonempty_opted_in_audio() {
        let (updates, received) = mpsc::channel();
        for (enabled, raw) in [(false, "some audio"), (true, " ")] {
            assert!(matches!(
                complete_recording(enabled, raw, &updates, |_| panic!("no turn expected")),
                Update::Done
            ));
            assert!(received.try_recv().is_err());
        }
        let finish = complete_recording(true, "raw", &updates, |text| {
            assert_eq!(text, "raw");
            assert!(matches!(received.try_recv(), Ok(Update::Cleaning)));
            Ok("clean".into())
        });
        assert!(matches!(finish, Update::Cleaned(text) if text == "clean"));
        assert!(
            matches!(complete_recording(true, "raw", &updates, |_| Err("rate limit".into())),
            Update::CleanupFailed(error) if error == "rate limit")
        );
    }

    #[test]
    fn missing_login_reports_an_error_in_the_session_language() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings {
            cloud_dictation: Some(true),
            auth_path: Some(directory.path().join("auth.json")),
            ..Settings::default()
        };
        for (locale, expected) in [
            (Locale::English, "Codex login file not found"),
            (Locale::SimplifiedChinese, "找不到 Codex 登录文件"),
        ] {
            let session = Session::start(settings.clone(), locale);
            let update = session
                .updates
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            assert!(matches!(update, Update::Failed(message) if message.starts_with(expected)));
        }
    }
    use crate::editor::{Editor, Movement};

    #[test]
    fn warns_for_missing_or_low_input_and_clears_on_recovery() {
        let start = Instant::now();
        let mut missing = InputMonitor::new(start);
        assert_eq!(missing.observe(start + QUIET_FOR, &[]), Some(true));
        let mut quiet = InputMonitor::new(start);
        assert_eq!(
            quiet.observe(start + QUIET_FOR - Duration::from_millis(1), &[0.0001]),
            None
        );
        assert_eq!(quiet.observe(start + QUIET_FOR, &[0.0001]), Some(true));
        assert_eq!(
            quiet.observe(start + QUIET_FOR + Duration::from_millis(20), &[-0.3]),
            Some(false)
        );
        let mut soft = InputMonitor::new(start);
        assert_eq!(soft.observe(start + QUIET_FOR, &[0.003]), None);
    }

    #[test]
    fn chunks_wait_for_a_pause_or_the_hard_limit() {
        let rate = WAV_RATE as usize;
        let (queue, output) = mpsc::sync_channel(2);
        let mut chunker = Chunker::new(rate, Locale::English);
        chunker.push(vec![0.5; rate * 650 / 1_000], &queue).unwrap();
        chunker
            .push(vec![0.0; rate * 350 / 1_000 - 1], &queue)
            .unwrap();
        assert!(output.try_recv().is_err());
        chunker.push(vec![0.0], &queue).unwrap();
        assert_eq!(output.try_recv().unwrap().len(), rate);
        chunker.push(vec![0.5; rate * 7 - 1], &queue).unwrap();
        assert!(output.try_recv().is_err());
        chunker.push(vec![0.5], &queue).unwrap();
        assert_eq!(output.try_recv().unwrap().len(), rate * 7);
    }

    #[test]
    fn silence_stays_out_of_the_upload_queue_and_stop_flushes_short_speech() {
        let rate = WAV_RATE as usize;
        let (queue, output) = mpsc::sync_channel(2);
        let mut chunker = Chunker::new(rate, Locale::English);
        chunker.push(vec![0.0001; rate * 8], &queue).unwrap();
        assert_eq!(chunker.samples.len(), rate * 350 / 1_000);
        assert!(output.try_recv().is_err());
        chunker.finish(&queue).unwrap();
        assert!(output.try_recv().is_err());

        let mut clicks = vec![0.0; rate / 2];
        for start_ms in [100, 220, 390] {
            // A loud click can straddle two analysis frames.
            let start = rate * (start_ms - 1) / 1_000;
            clicks[start..start + rate / 100].fill(0.5);
        }
        assert!(has_speech(&clicks, rate));
        chunker.push(clicks, &queue).unwrap();
        chunker.finish(&queue).unwrap();
        assert!(output.try_recv().is_err());

        let mut chunker = Chunker::new(rate, Locale::English);
        chunker.push(vec![0.03; rate / 5], &queue).unwrap();
        chunker.finish(&queue).unwrap();
        assert_eq!(output.try_recv().unwrap().len(), rate / 5);
    }

    #[test]
    fn queued_audio_is_combined_up_to_the_chunk_limit() {
        struct Fake;
        impl Transcriber for Fake {
            fn transcribe(&mut self, audio: Vec<u8>) -> Result<String, String> {
                let wav = hound::WavReader::new(Cursor::new(audio)).unwrap();
                match wav.duration() / WAV_RATE {
                    6 => Ok("First and second.".into()),
                    8 => Ok("First and second, then third.".into()),
                    _ => panic!("unexpected WAV length"),
                }
            }
        }
        let (chunks, pending) = mpsc::channel();
        for (seconds, sample) in [(4, 1), (2, 2), (2, 3)] {
            chunks
                .send(vec![sample; WAV_RATE as usize * seconds])
                .unwrap();
        }
        drop(chunks);
        let (updates, received) = mpsc::channel();
        upload(
            Fake,
            pending,
            &updates,
            &AtomicU8::new(STOPPING),
            Locale::English,
            |_| Update::Done,
        );
        assert!(
            matches!(received.recv().unwrap(), Update::Transcript(text) if text == "First and second.")
        );
        assert!(
            matches!(received.recv().unwrap(), Update::Transcript(text) if text == "First and second, then third.")
        );
        assert!(matches!(received.recv().unwrap(), Update::Done));
    }

    #[test]
    fn cancellation_keeps_the_request_slot_until_the_backend_returns() {
        struct Blocking {
            entered: Sender<()>,
            release: Receiver<()>,
        }
        impl Transcriber for Blocking {
            fn transcribe(&mut self, _: Vec<u8>) -> Result<String, String> {
                self.entered.send(()).unwrap();
                let _ = self.release.recv();
                Ok("Cancelled text".into())
            }
        }
        struct Next(Sender<()>);
        impl Transcriber for Next {
            fn transcribe(&mut self, _: Vec<u8>) -> Result<String, String> {
                self.0.send(()).unwrap();
                Ok("Next session".into())
            }
        }

        let old_state = Arc::new(AtomicU8::new(RECORDING));
        let (entered, old_request) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker_state = Arc::clone(&old_state);
        let old = thread::spawn(move || {
            request_transcript(
                &mut Blocking {
                    entered,
                    release: wait,
                },
                [1].iter(),
                &worker_state,
                Locale::English,
            )
        });
        old_request.recv_timeout(Duration::from_secs(2)).unwrap();
        old_state.store(CANCELLED, Ordering::Release);

        let (entered, new_request) = mpsc::channel();
        let (started, new_worker) = mpsc::channel();
        let new = thread::spawn(move || {
            started.send(()).unwrap();
            request_transcript(
                &mut Next(entered),
                [2].iter(),
                &AtomicU8::new(RECORDING),
                Locale::English,
            )
        });
        new_worker.recv_timeout(Duration::from_secs(2)).unwrap();
        let early = new_request.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(old.join().unwrap().unwrap(), None);
        assert_eq!(
            new.join().unwrap().unwrap().as_deref(),
            Some("Next session")
        );
        assert_eq!(early, Err(mpsc::RecvTimeoutError::Timeout));
    }

    #[test]
    fn transcript_revises_the_tail_during_capture_and_undoes_together() {
        struct Fake;
        impl Transcriber for Fake {
            fn transcribe(&mut self, audio: Vec<u8>) -> Result<String, String> {
                let mut wav = hound::WavReader::new(Cursor::new(audio)).unwrap();
                assert_eq!(wav.spec().sample_rate, WAV_RATE);
                assert_eq!(wav.spec().channels, 1);
                let samples = wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap();
                let text = match samples.as_slice() {
                    [1] => "We should meet.",
                    [1, 2] => "We should meet on Tuesday.",
                    [2, 3] => "On Tuesday, just after launch.",
                    [3, 4] => "Just after lunch, please bring notes.",
                    [4] => "Please bring notes.",
                    [4, 5] => "Please bring coats. Ask first.",
                    [5] => "Ask Jane.",
                    [5, 6] => "Ask Jane.",
                    [6] => "Bring water.",
                    [6, 7] => "Bring water. Thanks.",
                    [7] => "Thanks.",
                    _ => panic!("unexpected audio: {samples:?}"),
                };
                Ok(text.into())
            }
        }
        let (chunks, pending) = mpsc::channel();
        let (updates, received) = mpsc::channel();
        let worker = thread::spawn(move || {
            upload(
                Fake,
                pending,
                &updates,
                &AtomicU8::new(RECORDING),
                Locale::English,
                |_| Update::Done,
            )
        });
        let mut editor = Editor::default();
        editor.insert("Before after");
        editor.move_caret(Movement::Home, false, 80);
        for _ in 0..7 {
            editor.move_caret(Movement::Right, false, 80);
        }
        let mut range = editor.cursor()..editor.cursor();
        editor.begin();

        for (sample, expected) in [
            (1, "We should meet."),
            (2, "We should meet on Tuesday."),
            (3, "We should meet on Tuesday, just after launch."),
            (
                4,
                "We should meet on Tuesday, just after lunch, please bring notes.",
            ),
            (
                5,
                "We should meet on Tuesday, just after lunch, please bring notes. Ask Jane.",
            ),
            (
                6,
                "We should meet on Tuesday, just after lunch, please bring notes. Ask Jane. Bring water.",
            ),
            (
                7,
                "We should meet on Tuesday, just after lunch, please bring notes. Ask Jane. Bring water. Thanks.",
            ),
        ] {
            chunks.send(vec![sample]).unwrap();
            let Update::Transcript(text) = received.recv_timeout(Duration::from_secs(1)).unwrap()
            else {
                panic!("expected transcript")
            };
            assert!(editor.transcript(&mut range, &text));
            assert_eq!(editor.text(), format!("Before {expected} after"));
            assert_eq!(&editor.text()[editor.cursor()..], " after");
        }
        drop(chunks);
        worker.join().unwrap();
        assert!(matches!(received.recv().unwrap(), Update::Done));
        editor.finish();
        assert!(editor.undo());
        assert_eq!(editor.text(), "Before after");
        assert_eq!(editor.cursor(), 7);
    }
}
