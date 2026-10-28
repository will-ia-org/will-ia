//! Captura e reprodução de áudio com `cpal`, no processo do app.
//!
//! Entrada: dispositivo (qualquer taxa/canais) → mono f32 → reamostragem linear
//! para 24 kHz → PCM16 LE em quadros de ~100 ms → `RealtimeManager::audio_in`.
//! Saída: `AudioOut::Pcm` (PCM16 24 kHz) → fila → reamostragem para a taxa do
//! dispositivo → todos os canais.

use crate::i18n::{tr, trf};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use will_client::{AudioOut, VoiceLink};

pub const TARGET_RATE: u32 = 24_000;
const FRAME_MS: usize = 100;
const MAX_QUEUE_SAMPLES: usize = TARGET_RATE as usize * 600; // até 10 min em fila (o servidor envia mais rápido que o tempo real)
/// Depois do último áudio reproduzido, o microfone segue silenciado por este tempo
/// (cauda de reverberação) quando o barge-in está desligado.
const PLAYBACK_GUARD_MS: u64 = 350;
/// Reserva mínima antes de começar a tocar uma resposta (absorve jitter da rede).
const PREBUFFER_MS: u64 = 250;

/// Reamostrador linear com posição fracionária persistente entre chamadas.
struct Resampler {
    ratio: f64, // entrada/saída
    pos: f64,
    last: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            ratio: from as f64 / to as f64,
            pos: 0.0,
            last: 0.0,
        }
    }
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if (self.ratio - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        // Sequência virtual: [last, input...]; índice 0 = last.
        let n = input.len();
        let at = |i: usize| -> f32 {
            if i == 0 {
                self.last
            } else {
                input[(i - 1).min(n - 1)]
            }
        };
        let mut pos = self.pos;
        while pos < n as f64 {
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = at(i0);
            let b = at(i0 + 1);
            out.push(a + (b - a) * frac);
            pos += self.ratio;
        }
        self.pos = pos - n as f64;
        self.last = input[n - 1];
    }
}

pub struct AudioPipeline {
    _input: Option<cpal::Stream>,
    _output: Option<cpal::Stream>,
    muted: Arc<AtomicBool>,
    level_bits: Arc<AtomicU32>,
    queue: Arc<Mutex<VecDeque<f32>>>,
    /// Instante (ms desde `epoch`) até o qual o alto-falante está "ocupado".
    playing_until: Arc<AtomicU64>,
    epoch: Instant,
    /// Diagnóstico: quantas vezes a fila esvaziou no meio de uma resposta e quantos
    /// deltas de áudio o consumidor perdeu por atraso (`Lagged`).
    underruns: Arc<AtomicU64>,
    lagged: Arc<AtomicU64>,
    out_task: Option<tokio::task::JoinHandle<()>>,
    pub input_error: Option<String>,
    pub output_error: Option<String>,
}

/// Config de stream com buffer pequeno (~20 ms). Com o buffer padrão do
/// PipeWire/PulseAudio (2 s de fragmento na captura e 2 s de fila na saída) a fala
/// do usuário chega atrasada e a interrupção demora a calar o alto-falante.
fn low_latency(cfg: &cpal::SupportedStreamConfig) -> cpal::StreamConfig {
    low_latency_ms(cfg, 30)
}

fn low_latency_ms(cfg: &cpal::SupportedStreamConfig, ms: u32) -> cpal::StreamConfig {
    let mut c = cfg.config();
    let want = (cfg.sample_rate() * ms / 1000).max(64);
    let frames = match cfg.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => want.clamp(*min.max(&1), (*max).max(1)),
        cpal::SupportedBufferSize::Unknown => want,
    };
    c.buffer_size = cpal::BufferSize::Fixed(frames);
    c
}

pub struct DeviceLists {
    pub host: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Host de áudio: no Linux prefere PulseAudio/PipeWire (lista fones Bluetooth,
/// USB etc. com nomes legíveis); senão o padrão da plataforma.
fn audio_host() -> cpal::Host {
    cpal::default_host()
}

pub fn list_devices() -> DeviceLists {
    let host = audio_host();
    let name = |d: &cpal::Device| {
        d.description()
            .map(|x| x.name().to_string())
            .unwrap_or_else(|_| "?".into())
    };
    let inputs = host
        .input_devices()
        .map(|it| it.map(|d| name(&d)).collect())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|it| it.map(|d| name(&d)).collect())
        .unwrap_or_default();
    DeviceLists {
        host: format!("{:?}", host.id()),
        inputs,
        outputs,
    }
}

fn find_device(host: &cpal::Host, name: Option<&str>, input: bool) -> Option<cpal::Device> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = name {
        let devices = if input {
            host.input_devices().ok().map(|d| d.collect::<Vec<_>>())
        } else {
            host.output_devices().ok().map(|d| d.collect::<Vec<_>>())
        };
        if let Some(list) = devices {
            for d in list {
                if d.description().map(|x| x.name() == n).unwrap_or(false) {
                    return Some(d);
                }
            }
        }
        tracing::warn!(
            device = n,
            "dispositivo de áudio não encontrado; usando o padrão"
        );
    }
    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

impl AudioPipeline {
    /// Abre os dois streams e liga a saída ao `VoiceLink` (WS de áudio do servidor).
    pub fn start(
        rt_handle: &tokio::runtime::Handle,
        manager: Arc<VoiceLink>,
        input_name: Option<&str>,
        output_name: Option<&str>,
        start_muted: bool,
        barge_in: bool,
    ) -> Self {
        let host = audio_host();
        let muted = Arc::new(AtomicBool::new(start_muted));
        let playing_until = Arc::new(AtomicU64::new(0));
        let last_push = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        let lagged = Arc::new(AtomicU64::new(0));
        let epoch = Instant::now();
        let level_bits = Arc::new(AtomicU32::new(0));
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let mut input_error = None;
        let mut output_error = None;

        // ----- entrada -----
        let input = match find_device(&host, input_name, true) {
            Some(dev) => match dev.default_input_config() {
                Ok(cfg) => {
                    let rate = cfg.sample_rate();
                    let channels = cfg.channels() as usize;
                    let mut rs = Resampler::new(rate, TARGET_RATE);
                    let frame_samples = TARGET_RATE as usize * FRAME_MS / 1000;
                    let mut acc: Vec<f32> = Vec::with_capacity(frame_samples * 2);
                    let mut mono: Vec<f32> = Vec::new();
                    let mut resampled: Vec<f32> = Vec::new();
                    let muted2 = muted.clone();
                    let level2 = level_bits.clone();
                    let playing2 = playing_until.clone();
                    let mgr = manager.clone();
                    let stream_cfg: cpal::StreamConfig = low_latency(&cfg);
                    let err_fn = |e| tracing::warn!(error = %e, "stream de entrada");
                    let build = dev.build_input_stream(
                        stream_cfg,
                        move |data: &[f32], _| {
                            mono.clear();
                            if channels <= 1 {
                                mono.extend_from_slice(data);
                            } else {
                                for frame in data.chunks(channels) {
                                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                                }
                            }
                            let mut sum = 0.0f32;
                            for s in &mono {
                                sum += s * s;
                            }
                            let rms = (sum / mono.len().max(1) as f32).sqrt();
                            level2.store(rms.to_bits(), Ordering::Relaxed);
                            if muted2.load(Ordering::Relaxed) {
                                acc.clear();
                                return;
                            }
                            // Sem cancelamento de eco, o microfone ouve o alto-falante: enquanto o
                            // assistente fala, não mandamos áudio (a menos que barge-in esteja ligado).
                            if !barge_in
                                && epoch.elapsed().as_millis() as u64
                                    <= playing2.load(Ordering::Relaxed)
                            {
                                acc.clear();
                                return;
                            }
                            resampled.clear();
                            rs.process(&mono, &mut resampled);
                            acc.extend_from_slice(&resampled);
                            while acc.len() >= frame_samples {
                                let frame: Vec<f32> = acc.drain(..frame_samples).collect();
                                let mut bytes = Vec::with_capacity(frame.len() * 2);
                                for s in frame {
                                    let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                                    bytes.extend_from_slice(&v.to_le_bytes());
                                }
                                mgr.audio_in(Bytes::from(bytes));
                            }
                        },
                        err_fn,
                        None,
                    );
                    match build {
                        Ok(s) => {
                            if let Err(e) = s.play() {
                                input_error = Some(trf("microfone: {}", &[&e]));
                            }
                            Some(s)
                        }
                        Err(e) => {
                            input_error = Some(trf("microfone: {}", &[&e]));
                            None
                        }
                    }
                }
                Err(e) => {
                    input_error = Some(trf("microfone: {}", &[&e]));
                    None
                }
            },
            None => {
                input_error = Some(tr("nenhum dispositivo de entrada de áudio").into());
                None
            }
        };

        // ----- saída -----
        let output = match find_device(&host, output_name, false) {
            Some(dev) => match dev.default_output_config() {
                Ok(cfg) => {
                    let rate = cfg.sample_rate();
                    let channels = cfg.channels() as usize;
                    let mut rs = Resampler::new(TARGET_RATE, rate);
                    let q = queue.clone();
                    let playing_out = playing_until.clone();
                    let last_push_out = last_push.clone();
                    let underruns_out = underruns.clone();
                    let mut src: Vec<f32> = Vec::new();
                    let mut dst: Vec<f32> = Vec::new();
                    // Amostras já reamostradas e ainda não tocadas (sobra do callback anterior).
                    let mut carry: VecDeque<f32> = VecDeque::new();
                    // Jitter buffer: só começa a tocar com ~PREBUFFER_MS acumulados (ou quando a
                    // resposta acabou de chegar); ao esvaziar, espera encher de novo.
                    let mut rolling = false;
                    let stream_cfg: cpal::StreamConfig = low_latency_ms(&cfg, 80);
                    let err_fn = |e| tracing::warn!(error = %e, "stream de saída");
                    let build = dev.build_output_stream(
                        stream_cfg,
                        move |data: &mut [f32], _| {
                            let ch = channels.max(1);
                            let frames = data.len() / ch;
                            let now_ms = epoch.elapsed().as_millis() as u64;
                            {
                                let mut g = q.lock();
                                let queued_ms = (g.len() as u64) * 1000 / TARGET_RATE as u64;
                                let idle_ms =
                                    now_ms.saturating_sub(last_push_out.load(Ordering::Relaxed));
                                if !rolling {
                                    // começa quando há reserva suficiente ou quando a resposta já terminou de chegar
                                    rolling = queued_ms >= PREBUFFER_MS
                                        || (!g.is_empty() && idle_ms > 150);
                                }
                                if rolling {
                                    let missing = frames.saturating_sub(carry.len());
                                    let need = ((missing as f64)
                                        * (TARGET_RATE as f64 / rate as f64))
                                        .ceil()
                                        as usize;
                                    src.clear();
                                    for _ in 0..need {
                                        match g.pop_front() {
                                            Some(x) => src.push(x),
                                            None => break,
                                        }
                                    }
                                    if g.is_empty() && src.len() < need {
                                        // esvaziou: toca o que há e volta a acumular
                                        rolling = false;
                                        if idle_ms < 150 {
                                            // a resposta ainda estava chegando: isso é um underrun real
                                            underruns_out.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                } else {
                                    src.clear();
                                }
                            }
                            if !src.is_empty() || !carry.is_empty() {
                                playing_out.store(now_ms + PLAYBACK_GUARD_MS, Ordering::Relaxed);
                            }
                            dst.clear();
                            rs.process(&src, &mut dst);
                            carry.extend(dst.drain(..));
                            for frame in data.chunks_mut(ch) {
                                let v = carry.pop_front().unwrap_or(0.0);
                                for c in frame.iter_mut() {
                                    *c = v;
                                }
                            }
                        },
                        err_fn,
                        None,
                    );
                    match build {
                        Ok(s) => {
                            if let Err(e) = s.play() {
                                output_error = Some(trf("alto-falante: {}", &[&e]));
                            }
                            Some(s)
                        }
                        Err(e) => {
                            output_error = Some(trf("alto-falante: {}", &[&e]));
                            None
                        }
                    }
                }
                Err(e) => {
                    output_error = Some(trf("alto-falante: {}", &[&e]));
                    None
                }
            },
            None => {
                output_error = Some(tr("nenhum dispositivo de saída de áudio").into());
                None
            }
        };

        // ----- áudio do assistente → fila -----
        let mut rx = manager.subscribe_audio_out();
        let q = queue.clone();
        let playing_ctl = playing_until.clone();
        let last_push_task = last_push.clone();
        let lagged_task = lagged.clone();
        let out_task = rt_handle.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(AudioOut::Pcm(b)) => {
                        last_push_task.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                        let mut g = q.lock();
                        for s in b.as_chunks::<2>().0 {
                            g.push_back(i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0);
                        }
                        while g.len() > MAX_QUEUE_SAMPLES {
                            g.pop_front();
                        }
                    }
                    Ok(AudioOut::Control(v)) => {
                        if v.get("type").and_then(|t| t.as_str()) == Some("stop_playback") {
                            q.lock().clear();
                            playing_ctl.store(0, Ordering::Relaxed);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        lagged_task.fetch_add(n, Ordering::Relaxed);
                        tracing::warn!(n, "áudio: consumidor atrasou e perdeu deltas");
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            _input: input,
            _output: output,
            muted,
            level_bits,
            queue,
            playing_until,
            epoch,
            underruns,
            lagged,
            out_task: Some(out_task),
            input_error,
            output_error,
        }
    }

    /// O alto-falante está reproduzindo fala do assistente (ou acabou de reproduzir)?
    pub fn playing(&self) -> bool {
        self.epoch.elapsed().as_millis() as u64 <= self.playing_until.load(Ordering::Relaxed)
    }

    /// (underruns, deltas perdidos) desde o início da sessão.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.underruns.load(Ordering::Relaxed),
            self.lagged.load(Ordering::Relaxed),
        )
    }

    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }

    /// RMS do microfone (0..1), suavizado pelo chamador.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
}

/// Monitor de microfone para teste (sem sessão): só mede o nível.
pub struct InputMonitor {
    _stream: Option<cpal::Stream>,
    level_bits: Arc<AtomicU32>,
    pub error: Option<String>,
    pub device_name: String,
}

impl InputMonitor {
    pub fn start(input_name: Option<&str>) -> Self {
        let host = audio_host();
        let level_bits = Arc::new(AtomicU32::new(0));
        let Some(dev) = find_device(&host, input_name, true) else {
            return Self {
                _stream: None,
                level_bits,
                error: Some(tr("nenhum dispositivo de entrada").into()),
                device_name: String::new(),
            };
        };
        let device_name = dev
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_default();
        let cfg = match dev.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                return Self {
                    _stream: None,
                    level_bits,
                    error: Some(trf("microfone: {}", &[&e])),
                    device_name,
                }
            }
        };
        let channels = cfg.channels() as usize;
        let l2 = level_bits.clone();
        let stream = dev.build_input_stream(
            low_latency(&cfg),
            move |data: &[f32], _| {
                let mut sum = 0.0f32;
                let mut n = 0usize;
                for frame in data.chunks(channels.max(1)) {
                    let v = frame.iter().sum::<f32>() / channels.max(1) as f32;
                    sum += v * v;
                    n += 1;
                }
                let rms = (sum / n.max(1) as f32).sqrt();
                l2.store(rms.to_bits(), Ordering::Relaxed);
            },
            |e| tracing::warn!(error = %e, "monitor de microfone"),
            None,
        );
        match stream {
            Ok(s) => {
                let err = s.play().err().map(|e| trf("microfone: {}", &[&e]));
                Self {
                    _stream: Some(s),
                    level_bits,
                    error: err,
                    device_name,
                }
            }
            Err(e) => Self {
                _stream: None,
                level_bits,
                error: Some(trf("microfone: {}", &[&e])),
                device_name,
            },
        }
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed))
    }
}

/// Sinais sonoros curtos (sequência de notas: frequência em Hz, duração em ms).
pub const CUE_ON: &[(f32, u32)] = &[(523.0, 110), (784.0, 160)];
pub const CUE_OFF: &[(f32, u32)] = &[(784.0, 110), (523.0, 160)];
pub const CUE_ERROR: &[(f32, u32)] = &[(330.0, 160), (262.0, 220)];
pub const CUE_TEST: &[(f32, u32)] = &[(440.0, 500), (660.0, 500)];

/// Toca uma sequência de notas no dispositivo de saída escolhido, numa thread própria
/// (não bloqueia a GUI). Devolve o nome do dispositivo usado ou o erro ao abrir.
pub fn play_cue(
    output_name: Option<&str>,
    notes: &'static [(f32, u32)],
    volume: f32,
) -> Result<String, String> {
    let host = audio_host();
    let dev = find_device(&host, output_name, false)
        .ok_or_else(|| tr("nenhum dispositivo de saída").to_string())?;
    let device_name = dev
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_default();
    let cfg = dev
        .default_output_config()
        .map_err(|e| trf("saída: {}", &[&e]))?;
    let rate = cfg.sample_rate() as f32;
    let channels = cfg.channels() as usize;
    let total_ms: u32 = notes.iter().map(|n| n.1).sum();
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("will-cue".into())
        .spawn(move || {
            // Tabela de (início, fim, freq) em amostras.
            let mut segs: Vec<(usize, usize, f32)> = Vec::new();
            let mut acc = 0usize;
            for (f, ms) in notes {
                let len = (rate * (*ms as f32) / 1000.0) as usize;
                segs.push((acc, acc + len, *f));
                acc += len;
            }
            let total = acc;
            let mut t = 0usize;
            let stream = dev.build_output_stream(
                low_latency_ms(&cfg, 40),
                move |data: &mut [f32], _| {
                    for frame in data.chunks_mut(channels.max(1)) {
                        let v = if t < total {
                            match segs.iter().find(|(a, b, _)| t >= *a && t < *b) {
                                Some((a, b, f)) => {
                                    let len = (b - a).max(1) as f32;
                                    let pos = (t - a) as f32 / len;
                                    // ataque/decay suaves para não estalar
                                    let env =
                                        (pos * 12.0).min(1.0) * (1.0 - pos).min(1.0).powf(0.6);
                                    (2.0 * std::f32::consts::PI * f * t as f32 / rate).sin()
                                        * volume
                                        * env
                                }
                                None => 0.0,
                            }
                        } else {
                            0.0
                        };
                        t += 1;
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                |e| tracing::warn!(error = %e, "sinal sonoro"),
                None,
            );
            match stream {
                Ok(s) => {
                    if let Err(e) = s.play() {
                        let _ = tx.send(Err(trf("saída: {}", &[&e])));
                        return;
                    }
                    let _ = tx.send(Ok(()));
                    std::thread::sleep(std::time::Duration::from_millis(total_ms as u64 + 250));
                    drop(s);
                }
                Err(e) => {
                    let _ = tx.send(Err(trf("saída: {}", &[&e])));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(device_name),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(tr("tempo esgotado ao abrir a saída").into()),
    }
}

/// Tom de teste da saída (duas notas longas).
pub fn play_test_tone(output_name: Option<&str>) -> Result<String, String> {
    play_cue(output_name, CUE_TEST, 0.25)
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        if let Some(t) = self.out_task.take() {
            t.abort();
        }
        self.queue.lock().clear();
    }
}

/// Taxa do ditado (Whisper aceita 16 kHz mono PCM16 sem perda relevante).
pub const DICTATION_RATE: u32 = 16_000;

/// Gravador do botão "Falar" (push-to-talk de ditado): captura o microfone
/// enquanto existir e devolve um WAV PCM16 mono 16 kHz em `stop`.
pub struct Recorder {
    _stream: Option<cpal::Stream>,
    buf: Arc<Mutex<Vec<i16>>>,
    pub error: Option<String>,
    started: Instant,
}

impl Recorder {
    pub fn start(input_name: Option<&str>) -> Self {
        let host = audio_host();
        let buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
        let Some(dev) = find_device(&host, input_name, true) else {
            return Self {
                _stream: None,
                buf,
                error: Some(tr("nenhum dispositivo de entrada").into()),
                started: Instant::now(),
            };
        };
        let cfg = match dev.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                return Self {
                    _stream: None,
                    buf,
                    error: Some(trf("microfone: {}", &[&e])),
                    started: Instant::now(),
                }
            }
        };
        let channels = cfg.channels() as usize;
        let mut rs = Resampler::new(cfg.sample_rate(), DICTATION_RATE);
        let mut mono: Vec<f32> = Vec::new();
        let mut out: Vec<f32> = Vec::new();
        let b2 = buf.clone();
        let stream = dev.build_input_stream(
            low_latency(&cfg),
            move |data: &[f32], _| {
                mono.clear();
                if channels <= 1 {
                    mono.extend_from_slice(data);
                } else {
                    for frame in data.chunks(channels) {
                        mono.push(frame.iter().sum::<f32>() / channels as f32);
                    }
                }
                out.clear();
                rs.process(&mono, &mut out);
                let mut g = b2.lock();
                for s in &out {
                    g.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
            },
            |e| tracing::warn!(error = %e, "gravador de ditado"),
            None,
        );
        match stream {
            Ok(s) => {
                let err = s.play().err().map(|e| trf("microfone: {}", &[&e]));
                Self {
                    _stream: Some(s),
                    buf,
                    error: err,
                    started: Instant::now(),
                }
            }
            Err(e) => Self {
                _stream: None,
                buf,
                error: Some(trf("microfone: {}", &[&e])),
                started: Instant::now(),
            },
        }
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    /// Para a captura e devolve o WAV (ou `None` se gravou quase nada).
    pub fn stop(self) -> Option<Vec<u8>> {
        drop(self._stream);
        let samples = std::mem::take(&mut *self.buf.lock());
        if samples.len() < (DICTATION_RATE as usize) / 4 {
            return None;
        }
        Some(wav_pcm16_mono(&samples, DICTATION_RATE))
    }
}

/// Cabeçalho WAV + amostras PCM16 little-endian.
pub fn wav_pcm16_mono(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_halves_rate() {
        let mut r = Resampler::new(48_000, 24_000);
        let input: Vec<f32> = (0..96).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!((out.len() as i64 - 48).abs() <= 1, "{}", out.len());
        // monotônico crescente
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn resampler_keeps_phase_between_calls() {
        let mut r = Resampler::new(44_100, 24_000);
        let mut total = 0;
        for _ in 0..100 {
            let input = vec![0.5f32; 441];
            let mut out = Vec::new();
            r.process(&input, &mut out);
            total += out.len();
        }
        assert!((total as i64 - 24_000).abs() <= 2, "{total}");
    }
}
