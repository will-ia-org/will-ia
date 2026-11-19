//! Gravações de conversa (docs/32): onde ficam em disco e como viram texto.
//!
//! O app grava um WAV estéreo PCM16 16 kHz: canal esquerdo = microfone (quem
//! gravou), canal direito = saída do sistema (com quem falou). A transcrição
//! separa os canais, transcreve cada um em pedaços com marcação de tempo e
//! intercala as falas na ordem em que aconteceram. Depois gera um resumo
//! detalhado e um índice de embeddings (do resumo e de cada trecho), usado
//! pela busca por assunto (`search`, exposta no MCP como `recordings_search`).

use crate::app::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use will_storage::recordings::{status, Recording};

pub const RATE: u32 = 16_000;
const WAV_HEADER: usize = 44;
/// Pedaço enviado por vez à transcrição: 10 min de mono 16 kHz ≈ 19 MB (o
/// limite da API é 25 MB por arquivo).
const CHUNK_SECS: usize = 600;
/// Pico (PCM16) abaixo do qual um pedaço inteiro é tratado como silêncio.
const SILENT_PEAK: i16 = 400;
/// RMS (PCM16) abaixo do qual um trecho transcrito é descartado: em silêncio o
/// modelo inventa frases.
const SILENT_RMS: f64 = 90.0;
/// `no_speech_prob` acima do qual o trecho é descartado.
const NO_SPEECH: f64 = 0.6;
/// Falas seguidas da mesma pessoa com intervalo menor que isto viram uma só.
const JOIN_GAP_SECS: f64 = 1.5;
const MODEL: &str = "whisper-1";
const USD_PER_MIN: f64 = 0.006;

pub const AUDIO_FILE: &str = "audio.wav";
pub const TRANSCRIPT_FILE: &str = "transcript.json";
/// Transcrição legível (quem falou o quê, com o tempo).
pub const TRANSCRIPT_TEXT_FILE: &str = "transcricao.md";
pub const SUMMARY_FILE: &str = "resumo.md";

/// Modelo dos embeddings e preço por milhão de tokens.
pub const EMBED_MODEL: &str = "text-embedding-3-small";
const EMBED_USD_PER_M: f64 = 0.02;
/// Textos por chamada de embedding.
const EMBED_BATCH: usize = 64;
/// Tamanho alvo de um trecho indexado (caracteres), sem partir uma fala.
const CHUNK_CHARS: usize = 1200;
/// Entrada máxima do resumo (caracteres): cerca de 3 h de conversa.
const SUMMARY_INPUT_CHARS: usize = 180_000;
/// Marcadores de quem falou no texto dos trechos indexados. O nome do outro
/// lado pode mudar depois, então só entra na hora de mostrar.
const MARK_ME: &str = "<voce> ";
const MARK_OTHER: &str = "<outro> ";

const SUMMARY_PROMPT: &str = "Você recebe a transcrição automática de uma conversa gravada. \
As falas marcadas \"Você\" são de quem gravou; as outras são da pessoa ou do grupo indicado no cabeçalho. \
Escreva, em português do Brasil, um resumo DETALHADO e fiel da conversa, em texto corrido, cobrindo nesta ordem: \
os assuntos tratados; os fatos, números, valores, datas, nomes e lugares citados; o que cada lado disse ou defendeu; \
o que ficou combinado; e as pendências de cada lado. \
Use as palavras e os termos da própria conversa, porque o resumo será usado para encontrar esta conversa por busca. \
Não invente nada que não esteja na transcrição; se um trecho estiver confuso, diga que está confuso. \
Sem títulos, sem listas e sem marcação. Responda apenas com o resumo.";

/// Quem falou: quem gravou (microfone) ou o outro lado (saída do sistema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Me,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Início e fim, em segundos desde o começo da gravação.
    pub start: f64,
    pub end: f64,
    pub speaker: Speaker,
    pub text: String,
}

/// Pasta de todas as gravações.
pub fn root(state: &AppState) -> PathBuf {
    state.paths.data_dir.join("recordings")
}

/// Nome da pasta de uma gravação nova: data e hora locais do início + o
/// começo do id, para duas gravações no mesmo segundo não colidirem.
pub fn folder_name(started_at: chrono::DateTime<chrono::Utc>, id: &str) -> String {
    let short: String = id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    format!(
        "{}_{short}",
        started_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d_%H-%M-%S")
    )
}

/// Pasta de uma gravação: `folder` (data e hora) ou, nas antigas, o próprio id.
pub fn dir(state: &AppState, rec: &Recording) -> PathBuf {
    root(state).join(rec.folder.as_deref().unwrap_or(&rec.id))
}

pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

/// Lê a transcrição salva (vazia se ainda não há).
pub async fn read_transcript(state: &AppState, rec: &Recording) -> Vec<Segment> {
    match tokio::fs::read(dir(state, rec).join(TRANSCRIPT_FILE)).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// `m:ss` ou `h:mm:ss`.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Nome mostrado nas falas do outro lado.
pub fn other_label(rec: &Recording) -> &str {
    let w = rec.with_whom.trim();
    if w.is_empty() {
        "Outro lado"
    } else {
        w
    }
}

/// Transcrição em texto: `[0:03] Você: …`.
pub fn transcript_text(rec: &Recording, segments: &[Segment]) -> String {
    let other = other_label(rec);
    let mut out = String::new();
    for s in segments {
        let who = match s.speaker {
            Speaker::Me => "Você",
            Speaker::Other => other,
        };
        out.push_str(&format!("[{}] {who}: {}\n", clock(s.start), s.text));
    }
    out
}

/// Separa um WAV estéreo PCM16 em dois canais (esquerdo, direito).
pub fn split_channels(wav: &[u8]) -> Result<(Vec<i16>, Vec<i16>), String> {
    if wav.len() < WAV_HEADER || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err("arquivo não é um WAV".into());
    }
    let channels = u16::from_le_bytes([wav[22], wav[23]]);
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    let bits = u16::from_le_bytes([wav[34], wav[35]]);
    if channels != 2 || rate != RATE || bits != 16 {
        return Err(format!(
            "formato inesperado: {channels} canal(is), {rate} Hz, {bits} bits (esperado 2, {RATE}, 16)"
        ));
    }
    let data = &wav[WAV_HEADER..];
    let frames = data.len() / 4;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for f in data.as_chunks::<4>().0 {
        left.push(i16::from_le_bytes([f[0], f[1]]));
        right.push(i16::from_le_bytes([f[2], f[3]]));
    }
    Ok((left, right))
}

fn wav_mono(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(WAV_HEADER + samples.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

fn peak(samples: &[i16]) -> i16 {
    samples
        .iter()
        .map(|s| s.saturating_abs())
        .max()
        .unwrap_or(0)
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

/// Trechos devolvidos pela API para um pedaço, já filtrados e com o tempo
/// relativo ao começo da gravação.
fn keep_segments(
    api: &serde_json::Value,
    chunk: &[i16],
    offset_secs: f64,
    speaker: Speaker,
) -> Vec<Segment> {
    let Some(list) = api.get("segments").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in list {
        let start = s.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let end = s.get("end").and_then(|v| v.as_f64()).unwrap_or(start);
        let text = s
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        let no_speech = s
            .get("no_speech_prob")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if no_speech > NO_SPEECH {
            continue;
        }
        let a = ((start * RATE as f64) as usize).min(chunk.len());
        let b = ((end * RATE as f64) as usize).clamp(a, chunk.len());
        if rms(&chunk[a..b]) < SILENT_RMS {
            continue;
        }
        out.push(Segment {
            start: offset_secs + start,
            end: offset_secs + end,
            speaker,
            text,
        });
    }
    out
}

/// Ordena por tempo e junta falas seguidas da mesma pessoa.
pub fn merge_segments(mut all: Vec<Segment>) -> Vec<Segment> {
    all.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Segment> = Vec::new();
    for s in all {
        match out.last_mut() {
            Some(last) if last.speaker == s.speaker && s.start - last.end < JOIN_GAP_SECS => {
                last.text.push(' ');
                last.text.push_str(&s.text);
                last.end = last.end.max(s.end);
            }
            _ => out.push(s),
        }
    }
    out
}

async fn transcribe_chunk(
    client: &reqwest::Client,
    key: &str,
    chunk: &[i16],
) -> Result<serde_json::Value, String> {
    let form = reqwest::multipart::Form::new()
        .text("model", MODEL)
        .text("response_format", "verbose_json")
        .text("language", "pt")
        .part(
            "file",
            reqwest::multipart::Part::bytes(wav_mono(chunk))
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| e.to_string())?,
        );
    let resp = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("OpenAI: {e}"))?;
    let http = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("OpenAI: resposta inválida: {e}"))?;
    if !http.is_success() {
        let msg = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("falha na transcrição");
        return Err(format!("OpenAI {http}: {msg}"));
    }
    Ok(v)
}

/// Transcreve um canal em pedaços. Devolve os trechos e os segundos de áudio
/// de fato enviados (pedaços em silêncio não são enviados).
async fn transcribe_channel(
    client: &reqwest::Client,
    key: &str,
    samples: &[i16],
    speaker: Speaker,
) -> Result<(Vec<Segment>, f64), String> {
    let chunk_len = CHUNK_SECS * RATE as usize;
    let mut out = Vec::new();
    let mut sent_secs = 0.0;
    for (i, chunk) in samples.chunks(chunk_len).enumerate() {
        if peak(chunk) < SILENT_PEAK {
            continue;
        }
        let offset = (i * CHUNK_SECS) as f64;
        let v = transcribe_chunk(client, key, chunk).await?;
        sent_secs += chunk.len() as f64 / RATE as f64;
        out.extend(keep_segments(&v, chunk, offset, speaker));
    }
    Ok((out, sent_secs))
}

/// Cabeçalho de uma gravação em texto (transcrição legível, entrada do
/// resumo e dos embeddings).
pub fn header(rec: &Recording) -> String {
    let when = rec
        .started_at
        .with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M");
    let mut out = format!(
        "Conversa gravada em {when}, duração {}, com: {}.",
        clock(rec.duration_secs),
        other_label(rec)
    );
    if !rec.subject.trim().is_empty() {
        out.push_str(&format!(" Assunto: {}.", rec.subject.trim()));
    }
    out
}

/// Um trecho da conversa antes de virar vetor.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    /// Falas, uma por linha, com [`MARK_ME`] / [`MARK_OTHER`].
    pub text: String,
}

/// Agrupa as falas em trechos de cerca de [`CHUNK_CHARS`], sem partir uma fala.
pub fn pieces(segments: &[Segment]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut cur: Option<Piece> = None;
    for s in segments {
        let mark = match s.speaker {
            Speaker::Me => MARK_ME,
            Speaker::Other => MARK_OTHER,
        };
        let line = format!("{mark}{}\n", s.text);
        match &mut cur {
            Some(p) if p.text.chars().count() + line.chars().count() <= CHUNK_CHARS => {
                p.text.push_str(&line);
                p.end = s.end;
            }
            _ => {
                out.extend(cur.take());
                cur = Some(Piece {
                    start: s.start,
                    end: s.end,
                    text: line,
                });
            }
        }
    }
    out.extend(cur);
    out
}

/// Texto de um trecho com os nomes de quem falou no lugar dos marcadores.
pub fn render_piece(text: &str, other: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(MARK_ME) {
            out.push_str(&format!("Você: {rest}\n"));
        } else if let Some(rest) = line.strip_prefix(MARK_OTHER) {
            out.push_str(&format!("{other}: {rest}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Embeddings de uma lista de textos (na mesma ordem) e o total de tokens.
async fn embed(
    client: &reqwest::Client,
    key: &str,
    inputs: &[String],
) -> Result<(Vec<Vec<f32>>, u64), String> {
    let mut vectors = Vec::with_capacity(inputs.len());
    let mut tokens = 0u64;
    for batch in inputs.chunks(EMBED_BATCH) {
        let resp = client
            .post("https://api.openai.com/v1/embeddings")
            .bearer_auth(key)
            .json(&serde_json::json!({ "model": EMBED_MODEL, "input": batch }))
            .send()
            .await
            .map_err(|e| format!("OpenAI: {e}"))?;
        let http = resp.status();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("OpenAI: resposta inválida: {e}"))?;
        if !http.is_success() {
            let msg = v["error"]["message"]
                .as_str()
                .unwrap_or("falha nos embeddings");
            return Err(format!("OpenAI {http}: {msg}"));
        }
        tokens += v["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let data = v["data"].as_array().cloned().unwrap_or_default();
        if data.len() != batch.len() {
            return Err("OpenAI: quantidade de embeddings diferente da pedida".into());
        }
        for d in data {
            let vector: Vec<f32> = d["embedding"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_f64())
                        .map(|x| x as f32)
                        .collect()
                })
                .unwrap_or_default();
            if vector.is_empty() {
                return Err("OpenAI: embedding vazio".into());
            }
            vectors.push(vector);
        }
    }
    Ok((vectors, tokens))
}

fn http_client(secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .map_err(|e| e.to_string())
}

/// Etapa 1: áudio → `transcript.json` + `transcricao.md`.
async fn transcribe(
    state: &Arc<AppState>,
    rec: &Recording,
    key: &str,
) -> Result<Vec<Segment>, String> {
    let folder = dir(state, rec);
    let wav = tokio::fs::read(folder.join(AUDIO_FILE))
        .await
        .map_err(|e| format!("áudio: {e}"))?;
    let (me, other) = tokio::task::spawn_blocking(move || split_channels(&wav))
        .await
        .map_err(|e| e.to_string())??;
    let client = http_client(600)?;
    let (mut segments, me_secs) = transcribe_channel(&client, key, &me, Speaker::Me).await?;
    let (other_segments, other_secs) =
        transcribe_channel(&client, key, &other, Speaker::Other).await?;
    segments.extend(other_segments);
    let segments = merge_segments(segments);
    let json = serde_json::to_vec_pretty(&segments).map_err(|e| e.to_string())?;
    tokio::fs::write(folder.join(TRANSCRIPT_FILE), json)
        .await
        .map_err(|e| format!("gravar transcrição: {e}"))?;
    let secs = me_secs + other_secs;
    let cost = will_storage::costs::NewAiCost {
        source: "recording_stt".into(),
        r#ref: Some(format!("recording:{}", rec.id)),
        model: Some(MODEL.into()),
        units: serde_json::json!({ "audio_secs": secs.round() as i64 }),
        usd: secs / 60.0 * USD_PER_MIN,
        estimated: true,
        duration_secs: Some(secs.round() as i64),
        note: Some(format!("gravação com {}", other_label(rec))),
    };
    let _ = state.storage.add_ai_cost(&cost).await;
    Ok(segments)
}

/// Grava `transcricao.md` (e `resumo.md`, se houver) com os nomes atuais.
pub async fn write_text_files(state: &AppState, rec: &Recording, segments: &[Segment]) {
    let folder = dir(state, rec);
    let mut text = format!("# {}\n\n{}\n\n", other_label(rec), header(rec));
    text.push_str(&transcript_text(rec, segments));
    if let Err(e) = tokio::fs::write(folder.join(TRANSCRIPT_TEXT_FILE), text).await {
        tracing::warn!(recording = %rec.id, error = %e, "transcricao.md não gravada");
    }
    if let Some(summary) = rec.summary.as_deref() {
        let text = format!(
            "# Resumo: {}\n\n{}\n\n{summary}\n",
            other_label(rec),
            header(rec)
        );
        if let Err(e) = tokio::fs::write(folder.join(SUMMARY_FILE), text).await {
            tracing::warn!(recording = %rec.id, error = %e, "resumo.md não gravado");
        }
    }
}

/// Etapa 2: transcrição → resumo detalhado (banco + `resumo.md`).
async fn summarize(
    state: &Arc<AppState>,
    rec: &Recording,
    segments: &[Segment],
    key: &secrecy::SecretString,
) -> Result<String, String> {
    let input = format!("{}\n\n{}", header(rec), transcript_text(rec, segments));
    let model = state.settings().engine.thread_summary.model;
    let ai = will_realtime::openai::OpenAi::new(key.clone());
    let sum = ai
        .chat_summary(&input, &model, SUMMARY_PROMPT, SUMMARY_INPUT_CHARS)
        .await?;
    let summary = sum.script.trim().to_string();
    if summary.is_empty() {
        return Err("o resumo veio vazio".into());
    }
    state
        .storage
        .set_recording_summary(&rec.id, Some(&summary))
        .await
        .map_err(|e| e.to_string())?;
    let cost = will_storage::costs::NewAiCost {
        source: "recording_summary".into(),
        r#ref: Some(format!("recording:{}", rec.id)),
        model: Some(model),
        units: serde_json::json!({
            "prompt_tokens": sum.prompt_tokens,
            "completion_tokens": sum.completion_tokens,
        }),
        usd: sum.usd(),
        estimated: true,
        duration_secs: None,
        note: Some(format!("resumo da gravação com {}", other_label(rec))),
    };
    let _ = state.storage.add_ai_cost(&cost).await;
    Ok(summary)
}

/// Etapa 3: resumo + trechos → vetores no banco (índice da busca).
async fn index(
    state: &Arc<AppState>,
    rec: &Recording,
    segments: &[Segment],
    summary: &str,
    key: &str,
) -> Result<usize, String> {
    use will_storage::recordings::RecordingChunk;
    let other = other_label(rec);
    let head = header(rec);
    let parts = pieces(segments);
    // O cabeçalho entra no texto vetorizado para "com quem" e a data também
    // puxarem a conversa na busca.
    let mut inputs = vec![format!("{head}\n\nResumo: {summary}")];
    inputs.extend(
        parts
            .iter()
            .map(|p| format!("{head}\n\n{}", render_piece(&p.text, other))),
    );
    let (vectors, tokens) = embed(&http_client(120)?, key, &inputs).await?;
    let mut vectors = vectors.into_iter();
    let mut chunks = vec![RecordingChunk {
        recording_id: rec.id.clone(),
        kind: "summary".into(),
        idx: 0,
        start_secs: 0.0,
        end_secs: rec.duration_secs,
        text: summary.to_string(),
        embedding: vectors.next().unwrap_or_default(),
    }];
    for (i, (p, v)) in parts.iter().zip(vectors).enumerate() {
        chunks.push(RecordingChunk {
            recording_id: rec.id.clone(),
            kind: "speech".into(),
            idx: i as i64,
            start_secs: p.start,
            end_secs: p.end,
            text: p.text.clone(),
            embedding: v,
        });
    }
    state
        .storage
        .replace_recording_chunks(&rec.id, EMBED_MODEL, &chunks)
        .await
        .map_err(|e| e.to_string())?;
    let cost = will_storage::costs::NewAiCost {
        source: "recording_embedding".into(),
        r#ref: Some(format!("recording:{}", rec.id)),
        model: Some(EMBED_MODEL.into()),
        units: serde_json::json!({ "tokens": tokens, "chunks": chunks.len() }),
        usd: tokens as f64 / 1e6 * EMBED_USD_PER_M,
        estimated: true,
        duration_secs: None,
        note: Some(format!("índice da gravação com {}", other_label(rec))),
    };
    let _ = state.storage.add_ai_cost(&cost).await;
    Ok(chunks.len())
}

/// Transcreve, resume e indexa. A transcrição fica salva mesmo que uma etapa
/// seguinte falhe (o erro diz qual).
async fn run(state: &Arc<AppState>, rec: &Recording) -> Result<usize, String> {
    use secrecy::ExposeSecret;
    let key = crate::modules::openai_key(state)
        .await
        .ok_or("sem chave da OpenAI (Configurações → Voz)")?;
    let segments = transcribe(state, rec, key.expose_secret()).await?;
    // Índice e resumo antigos não valem para a transcrição nova.
    let _ = state.storage.set_recording_summary(&rec.id, None).await;
    let _ = state
        .storage
        .replace_recording_chunks(&rec.id, EMBED_MODEL, &[])
        .await;
    write_text_files(state, rec, &segments).await;
    if segments.is_empty() {
        return Ok(0);
    }
    let summary = summarize(state, rec, &segments, &key)
        .await
        .map_err(|e| format!("transcrição pronta, mas o resumo falhou: {e}"))?;
    let mut with_summary = rec.clone();
    with_summary.summary = Some(summary.clone());
    write_text_files(state, &with_summary, &segments).await;
    index(state, rec, &segments, &summary, key.expose_secret())
        .await
        .map_err(|e| format!("transcrição e resumo prontos, mas o índice de busca falhou: {e}"))?;
    Ok(segments.len())
}

/// Um resultado da busca por assunto.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub recording_id: String,
    /// Proximidade com a busca (0 a 1; maior = mais próximo).
    pub score: f32,
    /// `summary` (o resumo da conversa) ou `speech` (um trecho das falas).
    pub kind: String,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Ordena os trechos pela proximidade com `query` e devolve os `limit` melhores.
pub fn rank(
    query: &[f32],
    chunks: &[will_storage::recordings::RecordingChunk],
    limit: usize,
) -> Vec<Hit> {
    let mut hits: Vec<Hit> = chunks
        .iter()
        .map(|c| Hit {
            recording_id: c.recording_id.clone(),
            score: cosine(query, &c.embedding),
            kind: c.kind.clone(),
            start_secs: c.start_secs,
            end_secs: c.end_secs,
            text: c.text.clone(),
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    hits
}

/// Busca semântica nas gravações: os trechos (e resumos) mais próximos de `query`.
pub async fn search(state: &Arc<AppState>, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    use secrecy::ExposeSecret;
    let key = crate::modules::openai_key(state)
        .await
        .ok_or("sem chave da OpenAI (Configurações → Voz)")?;
    let chunks = state
        .storage
        .recording_chunks(EMBED_MODEL)
        .await
        .map_err(|e| e.to_string())?;
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let (vectors, tokens) = embed(
        &http_client(60)?,
        key.expose_secret(),
        &[query.trim().to_string()],
    )
    .await?;
    let cost = will_storage::costs::NewAiCost {
        source: "recording_embedding".into(),
        r#ref: None,
        model: Some(EMBED_MODEL.into()),
        units: serde_json::json!({ "tokens": tokens }),
        usd: tokens as f64 / 1e6 * EMBED_USD_PER_M,
        estimated: true,
        duration_secs: None,
        note: Some("busca nas gravações".into()),
    };
    let _ = state.storage.add_ai_cost(&cost).await;
    let query_vector = vectors.into_iter().next().unwrap_or_default();
    Ok(rank(&query_vector, &chunks, limit))
}

/// Transcreve, resume e indexa a gravação em segundo plano; o estado fica na
/// ficha (`transcribing` → `done` ou `error`).
pub async fn start_transcription(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    let rec = state
        .storage
        .get_recording(id)
        .await
        .map_err(|e| e.to_string())?;
    if rec.status == status::TRANSCRIBING {
        return Err("esta gravação já está sendo transcrita".into());
    }
    state
        .storage
        .set_recording_status(id, status::TRANSCRIBING, None)
        .await
        .map_err(|e| e.to_string())?;
    let state = state.clone();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match run(&state, &rec).await {
            Ok(n) => {
                tracing::info!(recording = %rec.id, segments = n, secs = started.elapsed().as_secs(), "gravação transcrita, resumida e indexada");
                let _ = state
                    .storage
                    .set_recording_status(&rec.id, status::DONE, None)
                    .await;
            }
            Err(e) => {
                tracing::warn!(recording = %rec.id, error = %crate::logging::redact(&e), "processamento da gravação falhou");
                let _ = state
                    .storage
                    .set_recording_status(&rec.id, status::ERROR, Some(&e))
                    .await;
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(left: &[i16], right: &[i16]) -> Vec<u8> {
        let mut w = vec![0u8; WAV_HEADER];
        w[0..4].copy_from_slice(b"RIFF");
        w[8..12].copy_from_slice(b"WAVE");
        w[22..24].copy_from_slice(&2u16.to_le_bytes());
        w[24..28].copy_from_slice(&RATE.to_le_bytes());
        w[34..36].copy_from_slice(&16u16.to_le_bytes());
        for (l, r) in left.iter().zip(right) {
            w.extend_from_slice(&l.to_le_bytes());
            w.extend_from_slice(&r.to_le_bytes());
        }
        w
    }

    fn seg(start: f64, end: f64, speaker: Speaker, text: &str) -> Segment {
        Segment {
            start,
            end,
            speaker,
            text: text.into(),
        }
    }

    #[test]
    fn splits_left_and_right() {
        let (l, r) = split_channels(&stereo(&[1, 2, 3], &[-1, -2, -3])).unwrap();
        assert_eq!(l, vec![1, 2, 3]);
        assert_eq!(r, vec![-1, -2, -3]);
        assert!(split_channels(b"nada").is_err());
        let mut mono = stereo(&[1], &[1]);
        mono[22..24].copy_from_slice(&1u16.to_le_bytes());
        assert!(split_channels(&mono).is_err());
    }

    #[test]
    fn segments_are_interleaved_and_same_speaker_runs_are_joined() {
        let out = merge_segments(vec![
            seg(5.0, 6.0, Speaker::Other, "tudo, e você?"),
            seg(0.0, 1.0, Speaker::Me, "oi"),
            seg(1.5, 2.5, Speaker::Me, "tudo bem?"),
            seg(20.0, 21.0, Speaker::Other, "depois eu ligo"),
        ]);
        assert_eq!(
            out,
            vec![
                seg(0.0, 2.5, Speaker::Me, "oi tudo bem?"),
                seg(5.0, 6.0, Speaker::Other, "tudo, e você?"),
                seg(20.0, 21.0, Speaker::Other, "depois eu ligo"),
            ]
        );
    }

    #[test]
    fn silent_or_unlikely_segments_are_dropped() {
        let mut chunk = vec![0i16; RATE as usize * 4];
        // fala só no primeiro segundo
        for s in chunk.iter_mut().take(RATE as usize) {
            *s = 4000;
        }
        let api = serde_json::json!({ "segments": [
            { "start": 0.0, "end": 1.0, "text": " olá ", "no_speech_prob": 0.01 },
            { "start": 2.0, "end": 3.0, "text": "Legendas pela comunidade", "no_speech_prob": 0.1 },
            { "start": 0.0, "end": 1.0, "text": "ruído", "no_speech_prob": 0.9 },
            { "start": 0.0, "end": 1.0, "text": "  ", "no_speech_prob": 0.0 },
        ]});
        let out = keep_segments(&api, &chunk, 600.0, Speaker::Other);
        assert_eq!(out, vec![seg(600.0, 601.0, Speaker::Other, "olá")]);
    }

    #[test]
    fn pieces_keep_whole_lines_and_render_the_current_name() {
        let long = "a".repeat(800);
        let segs = vec![
            seg(0.0, 5.0, Speaker::Me, "oi"),
            seg(5.0, 60.0, Speaker::Other, &long),
            seg(60.0, 120.0, Speaker::Me, &long),
        ];
        let p = pieces(&segs);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].start, p[0].end), (0.0, 60.0));
        assert_eq!((p[1].start, p[1].end), (60.0, 120.0));
        assert!(p[0].text.starts_with("<voce> oi\n<outro> aaa"));
        let shown = render_piece("<voce> oi\n<outro> tudo bem\n", "André");
        assert_eq!(shown, "Você: oi\nAndré: tudo bem\n");
    }

    #[test]
    fn rank_orders_by_cosine_similarity() {
        use will_storage::recordings::RecordingChunk;
        let chunk = |id: &str, v: Vec<f32>| RecordingChunk {
            recording_id: id.into(),
            kind: "speech".into(),
            idx: 0,
            start_secs: 0.0,
            end_secs: 1.0,
            text: id.into(),
            embedding: v,
        };
        let chunks = vec![
            chunk("longe", vec![0.0, 1.0]),
            chunk("perto", vec![0.9, 0.1]),
            chunk("igual", vec![2.0, 0.0]),
            chunk("outro-tamanho", vec![1.0]),
        ];
        let hits = rank(&[1.0, 0.0], &chunks, 2);
        assert_eq!(
            hits.iter()
                .map(|h| h.recording_id.as_str())
                .collect::<Vec<_>>(),
            vec!["igual", "perto"]
        );
        assert!((hits[0].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn folder_name_has_date_time_and_short_id() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-18T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let name = folder_name(at, "a1b2c3d4-0000-4000-8000-000000000000");
        assert!(name.ends_with("_a1b2c3d4"), "{name}");
        assert_eq!(name.len(), "2026-11-18_12-00-00_a1b2c3d4".len());
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(3.9), "0:03");
        assert_eq!(clock(125.0), "2:05");
        assert_eq!(clock(3725.0), "1:02:05");
    }
}
