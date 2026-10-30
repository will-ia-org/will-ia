//! Cliente HTTP da OpenAI compartilhado pelos módulos de voz: transcrição de
//! áudio (STT), **resumo falável** de uma resposta e síntese de voz (TTS).
//! Usado pelo bot do Telegram (voice notes → `sendVoice`) e pela API local
//! (`POST /api/voice/summary` e `/api/voice/tts`, o "ouvir resumo" do app).
//! Tudo best-effort; nunca loga a chave.

use secrecy::{ExposeSecret, SecretString};
use std::time::{Duration, Instant};

const OPENAI: &str = "https://api.openai.com/v1";
pub const STT_MODEL: &str = "gpt-4o-mini-transcribe";
pub const SUM_MODEL: &str = "gpt-4o-mini";
pub const TTS_MODEL: &str = "gpt-4o-mini-tts";

/// Taxa de amostragem do PCM devolvido por [`OpenAi::synthesize_pcm`].
pub const TTS_PCM_RATE: u32 = 24_000;

/// Preços (USD por 1M tokens) usados só para ESTIMAR o custo do resumo.
const SUM_USD_IN_PER_M: f64 = 0.15;
const SUM_USD_OUT_PER_M: f64 = 0.60;
/// Estimativa do TTS por 1.000 caracteres de entrada (ordem de grandeza do `tts-1`).
const TTS_USD_PER_1K_CHARS: f64 = 0.015;
/// Quantos parâmetros recusados pelo modelo podem ser removidos numa chamada.
const MAX_PARAM_RETRIES: usize = 3;

/// Modelos de raciocínio da OpenAI (gpt-5*, o1/o3/o4*): não aceitam
/// `temperature` fora do padrão e aceitam `reasoning_effort`.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

/// Parâmetro apontado num erro 400 da OpenAI por não ser suportado pelo
/// modelo (`error.param` com `code` `unsupported_parameter`/`unsupported_value`,
/// ou mensagem "Unsupported parameter/value"). Só o nome de primeiro nível
/// (`temperature`, `reasoning_effort`, `instructions`…).
pub fn rejected_param(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = v.get("error")?;
    let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let unsupported = code.starts_with("unsupported_")
        || msg.contains("Unsupported parameter")
        || msg.contains("Unsupported value")
        || msg.contains("is not supported with this model")
        || msg.contains("does not support");
    if !unsupported {
        return None;
    }
    let param = err
        .get("param")
        .and_then(|p| p.as_str())
        .map(str::to_string)
        .or_else(|| {
            // "Unsupported parameter: 'temperature' is not supported…"
            let (_, rest) = msg.split_once('\'')?;
            let (name, _) = rest.split_once('\'')?;
            Some(name.to_string())
        })?;
    let param = param.split('.').next().unwrap_or("").trim().to_string();
    (!param.is_empty()).then_some(param)
}

/// Mensagem legível de um erro da OpenAI (`error.message`), ou o começo do corpo.
fn openai_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| snippet(body))
}

/// Resumo falável + contagem de tokens informada pela OpenAI (para custo).
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub script: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Summary {
    /// Custo estimado em USD a partir dos tokens.
    pub fn usd(&self) -> f64 {
        self.prompt_tokens as f64 / 1e6 * SUM_USD_IN_PER_M
            + self.completion_tokens as f64 / 1e6 * SUM_USD_OUT_PER_M
    }
}

/// Custo estimado em USD de sintetizar `chars` caracteres.
pub fn tts_usd(chars: usize) -> f64 {
    chars as f64 / 1000.0 * TTS_USD_PER_1K_CHARS
}

/// Prompt padrão do resumo falável (editável em Configurações → Voz).
pub fn default_summary_prompt() -> &'static str {
    will_core::settings::default_speak_summary_prompt()
}

const TTS_INSTRUCTIONS: &str =
    "Você é um narrador em português do Brasil. Fale com tom amigável, claro e didático, em ritmo calmo e agradável.";

#[derive(Clone)]
pub struct OpenAi {
    key: SecretString,
    http: reqwest::Client,
}

impl OpenAi {
    pub fn new(key: SecretString) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { key, http }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.key.expose_secret())
    }

    /// Transcreve um áudio (ogg/opus do Telegram) e devolve o texto puro.
    pub async fn transcribe(
        &self,
        bytes: Vec<u8>,
        file_name: &str,
        mime: &str,
    ) -> Result<String, String> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_string())
            .mime_str(if mime.is_empty() { "audio/ogg" } else { mime })
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", STT_MODEL)
            .text("language", "pt")
            .text("response_format", "text");
        let started = Instant::now();
        let res = self
            .http
            .post(format!("{OPENAI}/audio/transcriptions"))
            .header("Authorization", self.auth())
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(what = "stt", model = STT_MODEL, error = %e, "OpenAI: erro de rede");
                format!("transcrição: {e}")
            })?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            tracing::warn!(
                what = "stt",
                model = STT_MODEL,
                http_status = status.as_u16(),
                duration_ms = started.elapsed().as_millis() as u64,
                "OpenAI: transcrição falhou"
            );
            return Err(format!("transcrição HTTP {status}: {}", snippet(&body)));
        }
        tracing::debug!(
            what = "stt",
            model = STT_MODEL,
            http_status = status.as_u16(),
            chars = body.trim().chars().count(),
            duration_ms = started.elapsed().as_millis() as u64,
            "OpenAI: transcrição ok"
        );
        Ok(body.trim().to_string())
    }

    /// Resposta completa → roteiro curto e falável.
    pub async fn audible_summary(&self, text: &str) -> Result<String, String> {
        Ok(self
            .audible_summary_with(text, SUM_MODEL, default_summary_prompt())
            .await?
            .script)
    }

    /// Como [`audible_summary`](Self::audible_summary), com modelo e prompt de
    /// sistema configuráveis e devolvendo também os tokens usados (custo).
    pub async fn audible_summary_with(
        &self,
        text: &str,
        model: &str,
        system: &str,
    ) -> Result<Summary, String> {
        self.chat_summary(text, model, system, 8000).await
    }

    /// Chat completion genérico "sistema + um texto do usuário" → resposta curta
    /// (resumo falável, resumo da thread…). `max_input_chars` corta a entrada.
    pub async fn chat_summary(
        &self,
        text: &str,
        model: &str,
        system: &str,
        max_input_chars: usize,
    ) -> Result<Summary, String> {
        let input: String = text.chars().take(max_input_chars).collect();
        let model = model.trim();
        let model = if model.is_empty() { SUM_MODEL } else { model };
        let system = system.trim();
        let system = if system.is_empty() {
            default_summary_prompt()
        } else {
            system
        };
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": input},
            ],
        });
        // Parâmetros que variam por família: os modelos de raciocínio (gpt-5,
        // o1/o3/o4…) só aceitam `temperature` padrão e ganham esforço mínimo;
        // os demais recebem a temperatura baixa de sempre. Se mesmo assim a
        // API recusar um parâmetro, `post_tolerant` o remove e tenta de novo.
        if is_reasoning_model(model) {
            body["reasoning_effort"] = serde_json::json!("minimal");
        } else {
            body["temperature"] = serde_json::json!(0.4);
        }
        let text = self
            .post_tolerant(&format!("{OPENAI}/chat/completions"), body, "resumo")
            .await?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let tok = |k: &str| v["usage"][k].as_u64().unwrap_or(0);
        Ok(Summary {
            script: v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string(),
            prompt_tokens: tok("prompt_tokens"),
            completion_tokens: tok("completion_tokens"),
        })
    }

    /// Roteiro → áudio Ogg/Opus, pronto para `sendVoice`.
    pub async fn synthesize(
        &self,
        script: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Vec<u8>, String> {
        self.synthesize_format(script, TTS_MODEL, voice, speed, "opus")
            .await
    }

    /// Roteiro → PCM16 LE mono a [`TTS_PCM_RATE`] (sem cabeçalho), para tocar
    /// direto no app. `model` vazio = [`TTS_MODEL`].
    pub async fn synthesize_pcm(
        &self,
        script: &str,
        model: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Vec<u8>, String> {
        self.synthesize_format(script, model, voice, speed, "pcm")
            .await
    }

    /// `POST /audio/speech` no formato pedido (`opus`, `pcm`, `mp3`, `wav`…).
    async fn synthesize_format(
        &self,
        script: &str,
        model: &str,
        voice: &str,
        speed: f32,
        format: &str,
    ) -> Result<Vec<u8>, String> {
        let input: String = script.chars().take(4000).collect();
        let model = model.trim();
        let model = if model.is_empty() { TTS_MODEL } else { model };
        let speed = if speed.is_finite() {
            speed.clamp(0.25, 4.0)
        } else {
            1.0
        };
        let mut body = serde_json::json!({
            "model": model,
            "voice": voice,
            "input": input,
            "response_format": format,
            "speed": speed,
        });
        // `instructions` só existe nos modelos gpt-4o-*-tts; `tts-1`/`tts-1-hd`
        // recusam. Em dúvida, manda e deixa o `post_tolerant` remover.
        if !model.starts_with("tts-1") {
            body["instructions"] = serde_json::json!(TTS_INSTRUCTIONS);
        }
        let bytes = self
            .post_tolerant_bytes(&format!("{OPENAI}/audio/speech"), body, "tts")
            .await?;
        Ok(bytes)
    }

    /// `POST` JSON que **tolera parâmetros não suportados pelo modelo**: quando
    /// a OpenAI responde 400 apontando um parâmetro (`error.param`) com
    /// `unsupported_parameter`/`unsupported_value`, o parâmetro é removido do
    /// corpo e a chamada repetida (até [`MAX_PARAM_RETRIES`] vezes). Assim trocar
    /// de modelo nas configurações (gpt-4o-mini → gpt-5-nano, tts-1 →
    /// gpt-4o-mini-tts…) não estoura erro na cara do usuário. Devolve o texto.
    async fn post_tolerant(
        &self,
        url: &str,
        body: serde_json::Value,
        what: &str,
    ) -> Result<String, String> {
        let bytes = self.post_tolerant_bytes(url, body, what).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post_tolerant_bytes(
        &self,
        url: &str,
        mut body: serde_json::Value,
        what: &str,
    ) -> Result<Vec<u8>, String> {
        let mut dropped: Vec<String> = Vec::new();
        let started = Instant::now();
        loop {
            let res = self
                .http
                .post(url)
                .header("Authorization", self.auth())
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    tracing::warn!(what, model = body["model"].as_str().unwrap_or(""), error = %e, "OpenAI: erro de rede");
                    format!("{what}: {e}")
                })?;
            let status = res.status();
            let bytes = res.bytes().await.map_err(|e| e.to_string())?;
            if status.is_success() {
                tracing::debug!(
                    what,
                    model = body["model"].as_str().unwrap_or(""),
                    http_status = status.as_u16(),
                    bytes = bytes.len(),
                    duration_ms = started.elapsed().as_millis() as u64,
                    "OpenAI: ok"
                );
                if !dropped.is_empty() {
                    tracing::info!(
                        what,
                        model = body["model"].as_str().unwrap_or(""),
                        removed = ?dropped,
                        "OpenAI: parâmetros não suportados pelo modelo foram removidos"
                    );
                }
                return Ok(bytes.to_vec());
            }
            let text = String::from_utf8_lossy(&bytes);
            if status.as_u16() == 400 && dropped.len() < MAX_PARAM_RETRIES {
                if let Some(param) = rejected_param(&text) {
                    if body.get(&param).is_some() && param != "model" {
                        tracing::debug!(what, param = %param, "OpenAI: parâmetro recusado; removendo e repetindo");
                        if let Some(o) = body.as_object_mut() {
                            o.remove(&param);
                        }
                        dropped.push(param);
                        continue;
                    }
                }
            }
            tracing::warn!(
                what,
                model = body["model"].as_str().unwrap_or(""),
                http_status = status.as_u16(),
                rate_limit = status.as_u16() == 429,
                duration_ms = started.elapsed().as_millis() as u64,
                "OpenAI: requisição falhou"
            );
            return Err(format!(
                "{what} HTTP {status}: {}",
                openai_error_message(&text)
            ));
        }
    }

    /// Pipeline completo: resumo + TTS. `None` se não houver o que falar.
    pub async fn voice_summary(
        &self,
        text: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Option<Vec<u8>>, String> {
        let script = self.audible_summary(text).await?;
        if script.is_empty() {
            return Ok(None);
        }
        let ogg = self.synthesize(&script, voice, speed).await?;
        Ok(if ogg.is_empty() { None } else { Some(ogg) })
    }
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_models() {
        assert!(is_reasoning_model("gpt-5-nano"));
        assert!(is_reasoning_model("GPT-5"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(!is_reasoning_model("gpt-4o-mini"));
        assert!(!is_reasoning_model("gpt-4.1-mini"));
    }

    #[test]
    fn rejected_param_from_openai_errors() {
        // temperature fora do padrão num modelo gpt-5
        let b = r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0.4 with this model. Only the default (1) value is supported.","type":"invalid_request_error","param":"temperature","code":"unsupported_value"}}"#;
        assert_eq!(rejected_param(b).as_deref(), Some("temperature"));
        // parâmetro desconhecido pelo modelo
        let b = r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort' is not supported with this model.","type":"invalid_request_error","param":"reasoning_effort","code":"unsupported_parameter"}}"#;
        assert_eq!(rejected_param(b).as_deref(), Some("reasoning_effort"));
        // sem `param`: extrai da mensagem
        let b = r#"{"error":{"message":"Unsupported parameter: 'instructions' is not supported with this model.","type":"invalid_request_error","param":null,"code":null}}"#;
        assert_eq!(rejected_param(b).as_deref(), Some("instructions"));
        // erro de outra natureza: não remove nada
        let b = r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#;
        assert_eq!(rejected_param(b), None);
        assert_eq!(rejected_param("não é json"), None);
        assert_eq!(openai_error_message(b), "Incorrect API key provided");
    }

    #[test]
    fn cost_estimates() {
        let s = Summary {
            script: "x".into(),
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
        };
        assert!((s.usd() - (SUM_USD_IN_PER_M + SUM_USD_OUT_PER_M)).abs() < 1e-9);
        assert!((tts_usd(2000) - 2.0 * TTS_USD_PER_1K_CHARS).abs() < 1e-9);
        assert_eq!(tts_usd(0), 0.0);
    }
}
