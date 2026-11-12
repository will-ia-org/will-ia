//! Sinais sonoros curtos tocados pelo **serviço** (sem GUI): sequência de
//! notas senoidais na saída de áudio. Best-effort: sem dispositivo (servidor
//! headless, sessão sem áudio) só registra em `debug`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Nota: (frequência em Hz, duração em ms).
pub type Note = (f32, u32);

/// "Tarefa está chegando": duas notas agudas e rápidas.
pub const CUE_SOON: &[Note] = &[(880.0, 90), (1175.0, 130)];

/// Sinais em voo: um bipe atrasado não empilha threads nem streams.
static IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn find_output(host: &cpal::Host, name: Option<&str>) -> Option<cpal::Device> {
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        if let Ok(list) = host.output_devices() {
            for d in list {
                if d.description().map(|x| x.name() == n).unwrap_or(false) {
                    return Some(d);
                }
            }
        }
        tracing::debug!(device = n, "saída de áudio não encontrada; usando a padrão");
    }
    host.default_output_device()
}

/// Toca `notes` na saída `output` (nome do dispositivo; `None`/vazio/ausente =
/// padrão do sistema), numa thread própria. Nunca bloqueia nem falha.
pub fn play_cue(output: Option<&str>, notes: &'static [Note], volume: f32) {
    use std::sync::atomic::Ordering;
    if IN_FLIGHT.fetch_add(1, Ordering::SeqCst) >= 2 {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let output = output.map(str::to_string);
    let spawned = std::thread::Builder::new()
        .name("will-cue".into())
        .spawn(move || {
            if let Err(e) = play_blocking(output.as_deref(), notes, volume) {
                tracing::debug!(error = %e, "sinal sonoro não tocado");
            }
            IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        });
    if spawned.is_err() {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

fn play_blocking(output: Option<&str>, notes: &'static [Note], volume: f32) -> Result<(), String> {
    let host = cpal::default_host();
    let dev = find_output(&host, output).ok_or("nenhum dispositivo de saída")?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let rate = cfg.sample_rate() as f32;
    let channels = (cfg.channels() as usize).max(1);
    // tabela de (início, fim, frequência) em amostras
    let mut segs: Vec<(usize, usize, f32)> = Vec::new();
    let mut total = 0usize;
    let mut total_ms = 0u64;
    for (f, ms) in notes {
        let len = (rate * (*ms as f32) / 1000.0) as usize;
        segs.push((total, total + len, *f));
        total += len;
        total_ms += u64::from(*ms);
    }
    let mut t = 0usize;
    let stream = dev
        .build_output_stream(
            cfg.config(),
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(channels) {
                    let v = match segs.iter().find(|(a, b, _)| t >= *a && t < *b) {
                        Some((a, b, f)) => {
                            let pos = (t - a) as f32 / (b - a).max(1) as f32;
                            // ataque/decay suaves para não estalar
                            let env = (pos * 12.0).min(1.0) * (1.0 - pos).min(1.0).powf(0.6);
                            (std::f32::consts::TAU * f * t as f32 / rate).sin() * volume * env
                        }
                        None => 0.0,
                    };
                    t += 1;
                    for ch in frame.iter_mut() {
                        *ch = v;
                    }
                }
            },
            |e| tracing::debug!(error = %e, "sinal sonoro"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(total_ms + 250));
    Ok(())
}
