//! Gravador de conversas: microfone e saída do sistema em dois canais de um
//! único WAV (esquerdo = você, direito = o que sai nas caixas/fone: WhatsApp,
//! Meet, qualquer programa).
//!
//! Os dois dispositivos têm relógios próprios e entregam áudio em rajadas, e a
//! saída do sistema nem sempre entrega algo em silêncio. Por isso as duas
//! faixas são alinhadas ao **relógio da gravação**: cada bloco que chega é
//! colocado na posição de tempo em que chegou (com tolerância de
//! [`SYNC_TOLERANCE`]), e o que falta vira silêncio. O gravador escreve no
//! disco aos poucos, então gravações longas não ficam na memória.

use super::audio::{audio_host, find_device, Resampler};
use crate::i18n::{tr, trf};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

/// Taxa das duas faixas gravadas (a mesma que a transcrição usa).
pub const REC_RATE: u32 = 16_000;
/// Quanto uma faixa pode se afastar do relógio da gravação antes de ser
/// corrigida (silêncio inserido ou bloco descartado): 250 ms.
const SYNC_TOLERANCE: u64 = REC_RATE as u64 / 4;
/// O gravador escreve com este atraso em relação ao relógio, para dar tempo de
/// os blocos atrasados de um dispositivo chegarem: 500 ms.
const WRITE_MARGIN: u64 = REC_RATE as u64 / 2;
/// Intervalo entre duas escritas no disco.
const WRITE_EVERY: Duration = Duration::from_millis(200);
/// Quanto a captura do sistema pode ficar sem entregar nada antes de ser
/// reaberta (dobra a cada tentativa, até o teto).
/// De quanto em quanto tempo a saída de som é conferida.
const FOLLOW_EVERY: Duration = Duration::from_millis(200);
const STALL_PATIENCE: Duration = Duration::from_millis(1200);
const STALL_PATIENCE_MAX: Duration = Duration::from_secs(10);
const WAV_HEADER: u64 = 44;

/// Uma faixa (canal) à espera de ser escrita. `base` é a posição, em amostras
/// desde o início da gravação, da primeira amostra de `buf`.
#[derive(Default)]
struct Track {
    buf: VecDeque<i16>,
    base: u64,
}

impl Track {
    /// Posição logo depois da última amostra guardada.
    fn end(&self) -> u64 {
        self.base + self.buf.len() as u64
    }

    /// Recebe um bloco que acabou de chegar; `now` é a posição do relógio da
    /// gravação neste instante (o fim do bloco).
    fn push(&mut self, samples: &[i16], now: u64) {
        let n = samples.len() as u64;
        let end = self.end();
        if end > now + SYNC_TOLERANCE {
            // Faixa adiantada (relógio do dispositivo mais rápido): descarta.
            return;
        }
        let start = now.saturating_sub(n);
        if start > end + SYNC_TOLERANCE {
            // Buraco (dispositivo ficou sem entregar): preenche com silêncio.
            let gap = (start - end) as usize;
            self.buf.extend(std::iter::repeat_n(0, gap));
        }
        self.buf.extend(samples.iter().copied());
    }

    /// Tira as amostras até a posição `until`, completando com silêncio o que
    /// não chegou. Depois disso a faixa começa em `until`.
    fn drain_until(&mut self, until: u64, out: &mut Vec<i16>) {
        out.clear();
        let want = until.saturating_sub(self.base) as usize;
        let have = want.min(self.buf.len());
        out.extend(self.buf.drain(..have));
        out.resize(want, 0);
        self.base = self.base.max(until);
    }
}

struct Shared {
    start: Instant,
    mic: Mutex<Track>,
    sys: Mutex<Track>,
    mic_level: AtomicU32,
    sys_level: AtomicU32,
    /// Diagnóstico: blocos recebidos e maior pico (bits de `f32`) de cada faixa.
    mic_blocks: AtomicU64,
    sys_blocks: AtomicU64,
    mic_peak: AtomicU32,
    sys_peak: AtomicU32,
    stop: AtomicBool,
    /// Erro surgido durante a captura ou a escrita.
    error: Mutex<Option<String>>,
    /// Por que o som do sistema não está sendo captado agora (`None` = aberto).
    system_error: Mutex<Option<String>>,
}

impl Shared {
    fn now(&self) -> u64 {
        (self.start.elapsed().as_secs_f64() * REC_RATE as f64) as u64
    }
}

/// Resultado de uma gravação encerrada.
#[derive(Debug, Clone)]
pub struct Finished {
    pub path: PathBuf,
    pub duration_secs: f64,
    /// A faixa do sistema foi capturada (senão o canal direito é silêncio).
    pub has_system: bool,
}

pub struct CallRecorder {
    _mic: Option<cpal::Stream>,
    shared: Arc<Shared>,
    writer: Option<std::thread::JoinHandle<Result<u64, String>>>,
    /// Linha que abre e vigia a captura do som do sistema ([`system_loop`]).
    system: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
    pub started: Instant,
}

impl CallRecorder {
    /// Abre microfone e saída do sistema e começa a gravar em `path`. Falha só
    /// se o microfone ou o arquivo não abrirem; sem a saída do sistema a
    /// gravação segue com o canal direito em silêncio (`system_error` diz por quê).
    pub fn start(path: &Path, input_name: Option<&str>) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let shared = Arc::new(Shared {
            start: Instant::now(),
            mic: Mutex::new(Track::default()),
            sys: Mutex::new(Track::default()),
            mic_level: AtomicU32::new(0),
            sys_level: AtomicU32::new(0),
            mic_blocks: AtomicU64::new(0),
            sys_blocks: AtomicU64::new(0),
            mic_peak: AtomicU32::new(0),
            sys_peak: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            error: Mutex::new(None),
            system_error: Mutex::new(None),
        });
        let host = audio_host();
        let mic_dev = find_device(&host, input_name, true)
            .ok_or_else(|| tr("nenhum dispositivo de entrada").to_string())?;
        let mic_cfg = mic_dev
            .default_input_config()
            .map_err(|e| trf("microfone: {}", &[&e]))?;
        let mic = open_stream(&mic_dev, &mic_cfg, shared.clone(), Source::Mic)
            .map_err(|e| trf("microfone: {}", &[&e]))?;

        tracing::info!(
            path = %path.display(),
            microphone = %device_name(&mic_dev),
            mic_rate = mic_cfg.sample_rate(),
            mic_channels = mic_cfg.channels(),
            "gravação de conversa iniciada"
        );
        let writer = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("call-recorder".into())
                .spawn(move || write_loop(file, shared))
                .map_err(|e| e.to_string())?
        };
        // O som do sistema abre numa linha própria: abrir pode demorar (fone
        // Bluetooth trocando de modo) e não pode travar a tela nem o microfone.
        let system = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("call-recorder-system".into())
                .spawn(move || system_loop(shared))
                .map_err(|e| e.to_string())?
        };
        Ok(Self {
            _mic: Some(mic),
            started: shared.start,
            shared,
            writer: Some(writer),
            system: Some(system),
            path: path.to_path_buf(),
        })
    }

    /// Níveis recentes (pico, 0..1) do microfone e da saída do sistema.
    pub fn levels(&self) -> (f32, f32) {
        (
            f32::from_bits(self.shared.mic_level.load(Ordering::Relaxed)),
            f32::from_bits(self.shared.sys_level.load(Ordering::Relaxed)),
        )
    }

    /// Algum som já chegou pela saída do sistema nesta gravação? (Silêncio
    /// total desde o início costuma ser permissão negada ou nada tocando.)
    pub fn system_heard(&self) -> bool {
        f32::from_bits(self.shared.sys_peak.load(Ordering::Relaxed)) > 0.0
    }

    /// Por que o som do sistema não está sendo gravado agora (a gravação
    /// segue só com o microfone), ou `None` se a captura está aberta.
    pub fn system_error(&self) -> Option<String> {
        self.shared.system_error.lock().clone()
    }

    /// Erro surgido durante a gravação (dispositivo sumiu, disco cheio).
    pub fn runtime_error(&self) -> Option<String> {
        self.shared.error.lock().clone()
    }

    /// Fecha os dispositivos, termina de escrever e devolve o arquivo.
    pub fn stop(mut self) -> Result<Finished, String> {
        self._mic = None;
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.system.take() {
            let _ = t.join();
        }
        let has_system = self.shared.sys_blocks.load(Ordering::Relaxed) > 0;
        let frames = self
            .writer
            .take()
            .ok_or("gravador já encerrado")?
            .join()
            .map_err(|_| "o gravador terminou com erro interno".to_string())??;
        let duration_secs = frames as f64 / REC_RATE as f64;
        let peak = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        tracing::info!(
            path = %self.path.display(),
            duration_secs,
            has_system,
            mic_blocks = self.shared.mic_blocks.load(Ordering::Relaxed),
            mic_peak = peak(&self.shared.mic_peak),
            sys_blocks = self.shared.sys_blocks.load(Ordering::Relaxed),
            sys_peak = peak(&self.shared.sys_peak),
            "gravação de conversa encerrada"
        );
        Ok(Finished {
            path: self.path.clone(),
            duration_secs,
            has_system,
        })
    }
}

impl Drop for CallRecorder {
    fn drop(&mut self) {
        // Abandonado sem `stop` (app fechando): o gravador ainda fecha o arquivo.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
enum Source {
    Mic,
    System,
}

/// Abre um stream de entrada que converte para mono 16 kHz e entrega à faixa.
fn open_stream(
    dev: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    shared: Arc<Shared>,
    source: Source,
) -> Result<cpal::Stream, String> {
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "formato de áudio não suportado: {:?}",
            cfg.sample_format()
        ));
    }
    let channels = cfg.channels() as usize;
    let mut rs = Resampler::new(cfg.sample_rate(), REC_RATE);
    let mut mono: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    let mut pcm: Vec<i16> = Vec::new();
    let data_shared = shared.clone();
    let stream = dev
        .build_input_stream(
            cfg.config(),
            move |data: &[f32], _| {
                mono.clear();
                if channels <= 1 {
                    mono.extend_from_slice(data);
                } else {
                    for frame in data.chunks(channels) {
                        mono.push(frame.iter().sum::<f32>() / channels as f32);
                    }
                }
                resampled.clear();
                rs.process(&mono, &mut resampled);
                pcm.clear();
                let mut peak = 0.0f32;
                for s in &resampled {
                    peak = peak.max(s.abs());
                    pcm.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
                let (level, track, blocks, top) = match source {
                    Source::Mic => (
                        &data_shared.mic_level,
                        &data_shared.mic,
                        &data_shared.mic_blocks,
                        &data_shared.mic_peak,
                    ),
                    Source::System => (
                        &data_shared.sys_level,
                        &data_shared.sys,
                        &data_shared.sys_blocks,
                        &data_shared.sys_peak,
                    ),
                };
                let old = f32::from_bits(level.load(Ordering::Relaxed));
                level.store(peak.max(old * 0.8).to_bits(), Ordering::Relaxed);
                blocks.fetch_add(1, Ordering::Relaxed);
                if peak > f32::from_bits(top.load(Ordering::Relaxed)) {
                    top.store(peak.to_bits(), Ordering::Relaxed);
                }
                let now = data_shared.now();
                track.lock().push(&pcm, now);
            },
            move |e| {
                tracing::warn!(error = %e, "gravação de conversa: stream");
                // Só o microfone derruba a gravação; sem a saída do sistema o
                // canal direito segue em silêncio.
                if matches!(source, Source::Mic) {
                    *shared.error.lock() = Some(e.to_string());
                }
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|x| x.name().to_string())
        .unwrap_or_else(|_| "?".into())
}

/// Dispositivo de onde sai o som do sistema para a captura: no Linux, a fonte
/// "monitor" do PulseAudio/PipeWire; nos demais, a saída padrão.
fn system_device(host: &cpal::Host) -> Result<cpal::Device, String> {
    #[cfg(target_os = "linux")]
    {
        host.input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| device_name(d).to_lowercase().contains("monitor"))
            .ok_or_else(|| {
                tr("nenhuma fonte \"monitor\" da saída de som (PulseAudio/PipeWire)").to_string()
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        host.default_output_device()
            .ok_or_else(|| tr("nenhum dispositivo de saída").to_string())
    }
}

fn system_config(dev: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
    #[cfg(target_os = "linux")]
    {
        dev.default_input_config().map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        dev.default_output_config().map_err(|e| e.to_string())
    }
}

fn signature_of(dev: &cpal::Device, cfg: &cpal::SupportedStreamConfig) -> String {
    format!(
        "{} · {} Hz · {} canal(is)",
        device_name(dev),
        cfg.sample_rate(),
        cfg.channels()
    )
}

/// Identifica a saída de som atual (dispositivo + formato), ou `None` sem saída.
fn system_signature(host: &cpal::Host) -> Option<String> {
    let dev = system_device(host).ok()?;
    let cfg = system_config(&dev).ok()?;
    Some(signature_of(&dev, &cfg))
}

/// Mantém a captura do som do sistema aberta enquanto a gravação durar.
///
/// Abre na saída padrão e reabre quando ela muda (outro dispositivo ou outro
/// formato: o fone Bluetooth cai para o modo de chamada quando o microfone
/// dele abre) ou quando a captura trava sem entregar nada. Só abre depois de
/// ver a mesma saída em duas conferências seguidas, para não abrir no meio de
/// uma troca.
fn system_loop(shared: Arc<Shared>) {
    let host = audio_host();
    let mut stream: Option<cpal::Stream> = None;
    let mut signature: Option<String> = None;
    let mut last_seen = system_signature(&host);
    let mut opened = Instant::now();
    let mut blocks = 0u64;
    let mut patience = STALL_PATIENCE;
    if last_seen.is_none() {
        *shared.system_error.lock() = Some(tr("nenhum dispositivo de saída").to_string());
    }
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(FOLLOW_EVERY);
        let current = system_signature(&host);
        let settled = current == last_seen;
        last_seen = current.clone();
        if !settled {
            continue;
        }
        let now_blocks = shared.sys_blocks.load(Ordering::Relaxed);
        if now_blocks != blocks {
            if blocks == 0 || patience != STALL_PATIENCE {
                tracing::info!(
                    at_ms = shared.start.elapsed().as_millis() as u64,
                    "gravação: o som do sistema está chegando"
                );
            }
            blocks = now_blocks;
            opened = Instant::now();
            patience = STALL_PATIENCE;
        }
        // No macOS a captura entrega blocos o tempo todo (silêncio incluso):
        // ficar sem nenhum é captura travada. Nos outros sistemas ela pode
        // ficar calada enquanto nada toca, então não dá para concluir nada.
        let stalled = cfg!(target_os = "macos") && stream.is_some() && opened.elapsed() >= patience;
        if current == signature && !stalled {
            continue;
        }
        if current == signature {
            tracing::info!(
                waited_ms = opened.elapsed().as_millis() as u64,
                "gravação: a captura do sistema não entregou som; reabrindo"
            );
            // Cada nova tentativa espera mais, para não ficar reabrindo sem parar.
            patience = (patience * 2).min(STALL_PATIENCE_MAX);
        } else {
            tracing::info!(
                from = signature.as_deref().unwrap_or("nenhuma"),
                to = current.as_deref().unwrap_or("nenhuma"),
                "gravação: abrindo a captura do som do sistema"
            );
            patience = STALL_PATIENCE;
        }
        // Fecha a captura antiga antes de abrir a nova.
        drop(stream.take());
        signature = current;
        let result = open_system(&host, shared.clone());
        opened = Instant::now();
        match result {
            Ok((s, _)) => {
                stream = Some(s);
                *shared.system_error.lock() = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "gravação: saída do sistema indisponível; só microfone");
                *shared.system_error.lock() = Some(e);
            }
        }
    }
}

/// Abre a captura da saída do sistema (o que toca nas caixas/fone) e devolve
/// também a assinatura da saída capturada.
///
/// - macOS (14.6+): "process tap" do Core Audio sobre a saída padrão; pede a
///   permissão de gravação de áudio do sistema na primeira vez.
/// - Windows: loopback do WASAPI sobre a saída padrão.
/// - Linux: a fonte "monitor" do PulseAudio/PipeWire.
fn open_system(host: &cpal::Host, shared: Arc<Shared>) -> Result<(cpal::Stream, String), String> {
    let dev = system_device(host)?;
    // No macOS a cpal só faz a captura da saída quando o dispositivo não tem
    // entrada própria. Num fone USB com microfone (um dispositivo só), ela
    // abriria o microfone do fone no lugar da saída: melhor avisar.
    #[cfg(target_os = "macos")]
    if dev
        .supported_input_configs()
        .map(|mut c| c.next().is_some())
        .unwrap_or(false)
    {
        return Err(tr(
            "a saída de som atual também é um microfone (fone com microfone num dispositivo só); escolha outra saída para gravar o áudio do sistema",
        )
        .to_string());
    }
    let cfg = system_config(&dev)?;
    let signature = signature_of(&dev, &cfg);
    let stream = open_stream(&dev, &cfg, shared, Source::System)?;
    Ok((stream, signature))
}

/// Cabeçalho de um WAV PCM16 estéreo com `frames` quadros.
fn wav_header(frames: u64) -> [u8; WAV_HEADER as usize] {
    let data_len = (frames * 4).min(u32::MAX as u64 - 36) as u32;
    let mut h = [0u8; WAV_HEADER as usize];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // estéreo
    h[24..28].copy_from_slice(&REC_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(REC_RATE * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// Intercala as duas faixas (esquerdo = microfone, direito = sistema) em bytes.
fn interleave(mic: &[i16], sys: &[i16], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(mic.len() * 4);
    for (m, s) in mic.iter().zip(sys) {
        out.extend_from_slice(&m.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
}

/// Laço do gravador: escreve no disco até `stop`, depois fecha o cabeçalho.
/// Devolve o total de quadros escritos.
fn write_loop(file: std::fs::File, shared: Arc<Shared>) -> Result<u64, String> {
    let r = write_frames(file, &shared);
    if let Err(e) = &r {
        tracing::error!(error = %e, "gravação de conversa: falha ao escrever");
        *shared.error.lock() = Some(e.clone());
    }
    r
}

fn write_frames(file: std::fs::File, shared: &Shared) -> Result<u64, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut w = std::io::BufWriter::new(file);
    w.write_all(&wav_header(0)).map_err(io)?;
    let mut written: u64 = 0;
    let (mut mic, mut sys, mut bytes) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        let stopping = shared.stop.load(Ordering::Relaxed);
        // Ao parar, escreve tudo que chegou; antes disso, guarda a margem.
        let until = if stopping {
            shared.now()
        } else {
            shared.now().saturating_sub(WRITE_MARGIN)
        };
        if until > written {
            shared.mic.lock().drain_until(until, &mut mic);
            shared.sys.lock().drain_until(until, &mut sys);
            interleave(&mic, &sys, &mut bytes);
            w.write_all(&bytes).map_err(io)?;
            written = until;
        }
        if stopping {
            break;
        }
        std::thread::sleep(WRITE_EVERY);
    }
    w.flush().map_err(io)?;
    let mut file = w.into_inner().map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    file.write_all(&wav_header(written)).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_in_time_are_appended_without_padding() {
        let mut t = Track::default();
        t.push(&[1; 160], 160);
        t.push(&[2; 160], 320);
        assert_eq!(t.end(), 320);
        let mut out = Vec::new();
        t.drain_until(320, &mut out);
        assert_eq!(out.len(), 320);
        assert_eq!((out[0], out[319]), (1, 2));
    }

    #[test]
    fn a_silent_gap_is_filled_so_the_track_stays_on_the_clock() {
        let mut t = Track::default();
        t.push(&[1; 160], 160);
        // 2 s sem entregar nada (saída do sistema em silêncio), depois volta.
        let later = 160 + 2 * REC_RATE as u64;
        t.push(&[2; 160], later);
        assert_eq!(t.end(), later);
        let mut out = Vec::new();
        t.drain_until(later, &mut out);
        assert_eq!(out[100], 1);
        assert_eq!(out[1000], 0);
        assert_eq!(out[out.len() - 1], 2);
    }

    #[test]
    fn small_jitter_is_not_padded() {
        let mut t = Track::default();
        t.push(&[1; 160], 160);
        // chegou 100 ms atrasado: dentro da tolerância, entra colado
        t.push(&[2; 160], 320 + 1600);
        assert_eq!(t.end(), 320);
    }

    #[test]
    fn a_track_running_ahead_drops_the_block() {
        let mut t = Track::default();
        t.push(&[1; 16_000], 16_000);
        // o relógio diz 0,5 s, a faixa já tem 1 s: mais de 250 ms adiantada
        t.push(&[2; 160], 8_000);
        assert_eq!(t.end(), 16_000);
    }

    #[test]
    fn draining_past_the_data_pads_and_moves_the_base() {
        let mut t = Track::default();
        t.push(&[7; 100], 100);
        let mut out = Vec::new();
        t.drain_until(300, &mut out);
        assert_eq!(out.len(), 300);
        assert_eq!((out[99], out[100]), (7, 0));
        assert_eq!((t.base, t.end()), (300, 300));
        // o que chega depois continua no relógio
        t.push(&[9; 100], 400);
        assert_eq!(t.end(), 400);
    }

    #[test]
    fn header_and_interleave_describe_stereo_pcm16() {
        let h = wav_header(REC_RATE as u64);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([h[22], h[23]]), 2);
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), REC_RATE);
        assert_eq!(
            u32::from_le_bytes([h[40], h[41], h[42], h[43]]),
            REC_RATE * 4
        );
        let mut out = Vec::new();
        interleave(&[1, 2], &[-1, -2], &mut out);
        assert_eq!(out, vec![1, 0, 0xff, 0xff, 2, 0, 0xfe, 0xff]);
    }
}
