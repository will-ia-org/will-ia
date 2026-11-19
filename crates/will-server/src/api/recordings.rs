//! Gravações de conversa (docs/32): o app grava e sobe o WAV; o servidor
//! guarda em `<dados>/recordings/<id>/`, transcreve e responde consultas.

use super::{bad, internal, ApiError, ApiResult, App};
use crate::recordings as rec;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use will_core::{Origin, ThreadHost};
use will_storage::recordings::{status, NewRecording, Recording};

/// Tamanho máximo de "com quem" e do assunto.
const LABEL_MAX: usize = 200;

fn check_id(id: &str) -> Result<(), ApiError> {
    if rec::valid_id(id) {
        Ok(())
    } else {
        Err(bad("id de gravação inválido"))
    }
}

fn label(s: &str) -> String {
    s.trim().chars().take(LABEL_MAX).collect()
}

/// Ficha + onde o áudio está no servidor (o app só abre o arquivo quando o
/// servidor é esta máquina).
fn view(state: &App, r: &Recording) -> Value {
    let mut v = json!(r);
    let folder = rec::dir(state, r);
    v["audio_path"] = json!(folder.join(rec::AUDIO_FILE).to_string_lossy());
    v["folder_path"] = json!(folder.to_string_lossy());
    v
}

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

pub async fn list(State(state): State<App>, Query(q): Query<ListQuery>) -> ApiResult<Value> {
    let rows = state
        .storage
        .list_recordings(q.limit.unwrap_or(200).clamp(1, 1000))
        .await?;
    let out: Vec<Value> = rows.iter().map(|r| view(&state, r)).collect();
    Ok(Json(json!(out)))
}

pub async fn get_one(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    check_id(&id)?;
    let r = state.storage.get_recording(&id).await?;
    let segments = rec::read_transcript(&state, &r).await;
    let text = rec::transcript_text(&r, &segments);
    let indexed = state.storage.recording_chunk_count(&id).await.unwrap_or(0);
    Ok(Json(json!({
        "recording": view(&state, &r),
        "indexed_chunks": indexed,
        "other_label": rec::other_label(&r),
        "segments": segments,
        "text": text,
    })))
}

/// `GET /api/recordings/{id}/audio`: o WAV da gravação (para o player do app).
pub async fn audio(
    State(state): State<App>,
    Path(id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    check_id(&id)?;
    let r = state.storage.get_recording(&id).await?;
    let bytes = tokio::fs::read(rec::dir(&state, &r).join(rec::AUDIO_FILE))
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("áudio indisponível: {e}")))?;
    Ok(([(axum::http::header::CONTENT_TYPE, "audio/wav")], bytes).into_response())
}

#[derive(serde::Deserialize, Default)]
pub struct UploadQuery {
    /// Início da gravação (RFC 3339); sem isso, agora menos a duração.
    pub started_at: Option<String>,
    #[serde(default)]
    pub with: String,
    #[serde(default)]
    pub subject: String,
    /// A saída do sistema foi capturada (padrão: sim).
    pub has_system: Option<bool>,
}

/// `POST /api/recordings?with=…&subject=…&started_at=…` com o WAV estéreo
/// (PCM16 16 kHz) no corpo. Grava em disco aos poucos, cria a ficha e já
/// começa a transcrever.
pub async fn upload(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> ApiResult<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    // O nome da pasta leva a data e a hora do início; sem `started_at` válido
    // (cliente antigo), usa agora e o início é corrigido pela duração depois.
    let started_hint = q
        .started_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc));
    let folder_name = rec::folder_name(started_hint.unwrap_or_else(chrono::Utc::now), &id);
    let folder = rec::root(&state).join(&folder_name);
    tokio::fs::create_dir_all(&folder)
        .await
        .map_err(|e| internal(format!("criar pasta da gravação: {e}")))?;
    let path = folder.join(rec::AUDIO_FILE);
    let saved = async {
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| format!("criar arquivo: {e}"))?;
        let mut stream = body.into_data_stream();
        let mut size: u64 = 0;
        let mut head: Vec<u8> = Vec::with_capacity(64);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("receber áudio: {e}"))?;
            if head.len() < 44 {
                let take = (44 - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("gravar áudio: {e}"))?;
            size += chunk.len() as u64;
        }
        file.flush()
            .await
            .map_err(|e| format!("gravar áudio: {e}"))?;
        Ok::<_, String>((size, head))
    }
    .await;
    let (size, head) = match saved {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            return Err(internal(e));
        }
    };
    // Só o cabeçalho é conferido aqui; o formato completo é validado ao transcrever.
    let stereo_wav = head.len() == 44
        && &head[0..4] == b"RIFF"
        && &head[8..12] == b"WAVE"
        && u16::from_le_bytes([head[22], head[23]]) == 2
        && u32::from_le_bytes([head[24], head[25], head[26], head[27]]) == rec::RATE;
    if !stereo_wav || size <= 44 {
        let _ = tokio::fs::remove_dir_all(&folder).await;
        return Err(bad("áudio inválido: esperado WAV estéreo PCM16 16 kHz"));
    }
    let duration_secs = (size - 44) as f64 / (rec::RATE as f64 * 4.0);
    let started_at = started_hint.unwrap_or_else(|| {
        chrono::Utc::now() - chrono::Duration::milliseconds((duration_secs * 1000.0) as i64)
    });
    let author = me.author();
    let row = state
        .storage
        .insert_recording(&NewRecording {
            id: &id,
            started_at,
            duration_secs,
            with_whom: &label(&q.with),
            subject: &label(&q.subject),
            has_system: q.has_system.unwrap_or(true),
            size_bytes: size as i64,
            created_by: Some(&author),
            folder: &folder_name,
        })
        .await;
    let row = match row {
        Ok(r) => r,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            return Err(e.into());
        }
    };
    tracing::info!(recording = %id, duration_secs, bytes = size, "gravação de conversa salva");
    // Sem chave da OpenAI a gravação fica salva; dá para transcrever depois.
    if let Err(e) = rec::start_transcription(&state, &id).await {
        tracing::warn!(recording = %id, error = %e, "transcrição não iniciada");
    }
    let row = state.storage.get_recording(&id).await.unwrap_or(row);
    Ok(Json(view(&state, &row)))
}

#[derive(serde::Deserialize)]
pub struct LabelsBody {
    #[serde(default)]
    pub with_whom: String,
    #[serde(default)]
    pub subject: String,
}

pub async fn set_labels(
    State(state): State<App>,
    Path(id): Path<String>,
    Json(b): Json<LabelsBody>,
) -> ApiResult<Value> {
    check_id(&id)?;
    state
        .storage
        .set_recording_labels(&id, &label(&b.with_whom), &label(&b.subject))
        .await?;
    let r = state.storage.get_recording(&id).await?;
    // Os arquivos de texto levam o nome de quem falou: refaz com o nome novo.
    let segments = rec::read_transcript(&state, &r).await;
    if !segments.is_empty() {
        rec::write_text_files(&state, &r, &segments).await;
    }
    Ok(Json(view(&state, &r)))
}

pub async fn transcribe(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    check_id(&id)?;
    state.storage.get_recording(&id).await?;
    rec::start_transcription(&state, &id).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_one(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    check_id(&id)?;
    let r = state.storage.get_recording(&id).await?;
    if r.status == status::TRANSCRIBING {
        return Err(bad("aguarde a transcrição terminar antes de apagar"));
    }
    state.storage.delete_recording(&id).await?;
    if let Err(e) = tokio::fs::remove_dir_all(rec::dir(&state, &r)).await {
        tracing::warn!(recording = %id, error = %e, "pasta da gravação não removida");
    }
    tracing::info!(recording = %id, "gravação de conversa apagada");
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/recordings/{id}/ask`: abre uma thread nova com a transcrição
/// como primeira mensagem, para o usuário perguntar sobre a conversa.
pub async fn ask(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    check_id(&id)?;
    let r = state.storage.get_recording(&id).await?;
    let segments = rec::read_transcript(&state, &r).await;
    if segments.is_empty() {
        return Err(bad("esta gravação ainda não tem transcrição"));
    }
    let other = rec::other_label(&r);
    let when = r
        .started_at
        .with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M");
    let title = format!(
        "Gravação · {other} · {}",
        when.to_string().get(..5).unwrap_or("")
    );
    let mut text = format!(
        "Esta é a transcrição de uma conversa que eu gravei em {when} (duração {}), com: {other}.\n",
        rec::clock(r.duration_secs)
    );
    if !r.subject.trim().is_empty() {
        text.push_str(&format!("Assunto: {}.\n", r.subject.trim()));
    }
    text.push_str(
        "As falas marcadas \"Você\" são minhas (microfone); as outras vieram do áudio do computador. \
         A transcrição é automática e pode ter erros de palavras e nomes.\n\n<transcricao>\n",
    );
    text.push_str(&rec::transcript_text(&r, &segments));
    text.push_str(
        "</transcricao>\n\nLeia a transcrição e me dê um resumo curto: pontos principais, o que ficou \
         combinado e pendências de cada lado. Depois vou fazer perguntas sobre essa conversa.",
    );
    let host = state.host();
    let t = host
        .ensure_thread(&Origin::Ui, Some(&title))
        .await
        .map_err(internal)?;
    host.send_message_from(t.id, &text, &[], Some(&me.author()))
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "thread_id": t.id })))
}
