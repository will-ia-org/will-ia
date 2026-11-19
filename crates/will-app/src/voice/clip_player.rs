//! Player de um áudio gravado (PCM16 intercalado), com pausa e salto para
//! qualquer ponto. Toca dentro do app: a gravação de conversa em dois canais
//! sai com você num lado e o computador no outro.

use super::audio::{audio_host, find_device};
use crate::i18n::{tr, trf};
use cpal::traits::{DeviceTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Áudio decodificado de um WAV PCM16.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    /// Amostras intercaladas (`channels` por quadro).
    pub samples: Arc<Vec<i16>>,
    pub channels: usize,
    pub rate: u32,
}

impl Clip {
    /// Lê um WAV PCM16 com cabeçalho de 44 bytes (o formato que o will-ia grava).
    pub fn from_wav(wav: &[u8]) -> Result<Self, String> {
        if wav.len() < 44 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
            return Err(tr("arquivo de áudio inválido").to_string());
        }
        let channels = u16::from_le_bytes([wav[22], wav[23]]) as usize;
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        let bits = u16::from_le_bytes([wav[34], wav[35]]);
        if channels == 0 || channels > 2 || rate == 0 || bits != 16 {
            return Err(tr("formato de áudio não suportado").to_string());
        }
        let samples: Vec<i16> = wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        Ok(Self {
            samples: Arc::new(samples),
            channels,
            rate,
        })
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.rate as f64
    }

    /// Amostra (esquerda, direita) na posição fracionária `pos`, em quadros,
    /// com interpolação linear. Fora do áudio = silêncio.
    fn at(&self, pos: f64) -> (f32, f32) {
        let frames = self.frames();
        if pos < 0.0 || frames == 0 || pos >= frames as f64 {
            return (0.0, 0.0);
        }
        let i = pos.floor() as usize;
        let frac = (pos - i as f64) as f32;
        let frame = |i: usize| -> (f32, f32) {
            let i = i.min(frames - 1) * self.channels;
            let l = self.samples[i] as f32 / 32768.0;
            let r = if self.channels > 1 {
                self.samples[i + 1] as f32 / 32768.0
            } else {
                l
            };
            (l, r)
        };
        let (a, b) = (frame(i), frame(i + 1));
        (a.0 + (b.0 - a.0) * frac, a.1 + (b.1 - a.1) * frac)
    }
}

struct Shared {
    clip: Clip,
    /// Posição atual em quadros do clipe (bits de `f64`).
    pos: AtomicU64,
    playing: AtomicBool,
}

/// Player de um [`Clip`]: mantém o dispositivo de saída aberto enquanto existe.
pub struct ClipPlayer {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
}

impl ClipPlayer {
    /// Abre a saída de som (a escolhida em Configurações → Voz, ou a padrão) e
    /// fica pausado no início.
    pub fn new(clip: Clip, output_name: Option<&str>) -> Result<Self, String> {
        let host = audio_host();
        let dev = find_device(&host, output_name, false)
            .ok_or_else(|| tr("nenhum dispositivo de saída").to_string())?;
        let cfg = dev
            .default_output_config()
            .map_err(|e| trf("saída de som: {}", &[&e]))?;
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            return Err(tr("formato de áudio não suportado").to_string());
        }
        let out_channels = cfg.channels() as usize;
        // Quadros do clipe consumidos por quadro de saída.
        let step = clip.rate as f64 / cfg.sample_rate() as f64;
        let shared = Arc::new(Shared {
            clip,
            pos: AtomicU64::new(0f64.to_bits()),
            playing: AtomicBool::new(false),
        });
        let s = shared.clone();
        let stream = dev
            .build_output_stream(
                cfg.config(),
                move |data: &mut [f32], _| {
                    if !s.playing.load(Ordering::Relaxed) {
                        data.fill(0.0);
                        return;
                    }
                    let end = s.clip.frames() as f64;
                    let mut pos = f64::from_bits(s.pos.load(Ordering::Relaxed));
                    for frame in data.chunks_mut(out_channels) {
                        let (l, r) = s.clip.at(pos);
                        match frame.len() {
                            1 => frame[0] = (l + r) * 0.5,
                            _ => {
                                frame[0] = l;
                                frame[1] = r;
                                for extra in frame.iter_mut().skip(2) {
                                    *extra = 0.0;
                                }
                            }
                        }
                        pos += step;
                    }
                    if pos >= end {
                        // Chegou ao fim: para e volta ao começo.
                        s.playing.store(false, Ordering::Relaxed);
                        pos = 0.0;
                    }
                    s.pos.store(pos.to_bits(), Ordering::Relaxed);
                },
                |e| tracing::warn!(error = %e, "player da gravação"),
                None,
            )
            .map_err(|e| trf("saída de som: {}", &[&e]))?;
        stream.play().map_err(|e| trf("saída de som: {}", &[&e]))?;
        Ok(Self {
            _stream: stream,
            shared,
        })
    }

    pub fn playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn set_playing(&self, on: bool) {
        self.shared.playing.store(on, Ordering::Relaxed);
    }

    pub fn duration_secs(&self) -> f64 {
        self.shared.clip.duration_secs()
    }

    pub fn position_secs(&self) -> f64 {
        f64::from_bits(self.shared.pos.load(Ordering::Relaxed)) / self.shared.clip.rate as f64
    }

    /// Salta para `secs` (limitado ao tamanho do áudio).
    pub fn seek_secs(&self, secs: f64) {
        let max = self.shared.clip.frames().saturating_sub(1) as f64;
        let pos = (secs * self.shared.clip.rate as f64).clamp(0.0, max);
        self.shared.pos.store(pos.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut w = vec![0u8; 44];
        w[0..4].copy_from_slice(b"RIFF");
        w[8..12].copy_from_slice(b"WAVE");
        w[22..24].copy_from_slice(&channels.to_le_bytes());
        w[24..28].copy_from_slice(&rate.to_le_bytes());
        w[34..36].copy_from_slice(&16u16.to_le_bytes());
        for s in samples {
            w.extend_from_slice(&s.to_le_bytes());
        }
        w
    }

    #[test]
    fn reads_stereo_wav_and_reports_duration() {
        let c = Clip::from_wav(&wav(2, 16_000, &[0; 32_000])).unwrap();
        assert_eq!((c.channels, c.rate, c.frames()), (2, 16_000, 16_000));
        assert!((c.duration_secs() - 1.0).abs() < 1e-9);
        assert!(Clip::from_wav(b"x").is_err());
        assert!(Clip::from_wav(&wav(6, 16_000, &[0; 12])).is_err());
    }

    #[test]
    fn interpolates_each_channel_and_is_silent_outside() {
        // esquerda: 0 → 16384; direita: 0 → -16384
        let c = Clip::from_wav(&wav(2, 16_000, &[0, 0, 16_384, -16_384])).unwrap();
        let (l, r) = c.at(0.5);
        assert!(
            (l - 0.25).abs() < 1e-6 && (r + 0.25).abs() < 1e-6,
            "{l} {r}"
        );
        assert_eq!(c.at(5.0), (0.0, 0.0));
        assert_eq!(c.at(-1.0), (0.0, 0.0));
        // mono: os dois lados iguais
        let m = Clip::from_wav(&wav(1, 16_000, &[16_384, 16_384])).unwrap();
        assert_eq!(m.at(0.0), (0.5, 0.5));
    }
}
