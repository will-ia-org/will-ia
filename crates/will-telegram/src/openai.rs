//! Chamadas à OpenAI usadas pelo bot: transcrição de voice notes (STT), resumo
//! falável e síntese de voz (TTS). Tudo best-effort; nunca loga a chave.

use secrecy::{ExposeSecret, SecretString};
use std::time::Duration;

const OPENAI: &str = "https://api.openai.com/v1";
const STT_MODEL: &str = "gpt-4o-mini-transcribe";
const SUM_MODEL: &str = "gpt-4o-mini";
const TTS_MODEL: &str = "gpt-4o-mini-tts";

const SUM_SYSTEM: &str = "Você recebe a RESPOSTA de um assistente e a transforma em um RESUMO FALADO, para ser ouvido como áudio (voz), em português do Brasil.\n\
Objetivo: a pessoa entender o essencial só de ouvir, sem precisar ler.\n\
Regras de audibilidade:\n\
- O tamanho pode acompanhar a resposta: se ela for densa, o áudio pode ser mais longo. Não force encurtar a ponto de perder pontos importantes — cubra tudo que importa. Mas seja fluido, sem enrolação nem repetição.\n\
- Linguagem falada e natural, como se estivesse explicando para a pessoa; pode usar \"você\".\n\
- NÃO leia marcação: nada de marcadores de lista, números de item, hashes, asteriscos, crases, URLs, caminhos de arquivo, IDs/hashes longos, blocos de código ou símbolos. Converta listas em frases corridas.\n\
- Diga números e siglas de forma falada e clara; expanda siglas quando ajudar.\n\
- Foque no que foi pedido e na resposta/decisão principal; se houver próximos passos importantes, mencione em uma frase.\n\
- Comece com \"Resumindo,\" e termine de forma natural.\n\
Responda APENAS com o texto do roteiro do áudio, sem aspas e sem comentários.";

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
        let res = self
            .http
            .post(format!("{OPENAI}/audio/transcriptions"))
            .header("Authorization", self.auth())
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("transcrição: {e}"))?;
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("transcrição HTTP {status}: {}", snippet(&body)));
        }
        Ok(body.trim().to_string())
    }

    /// Resposta completa → roteiro curto e falável.
    pub async fn audible_summary(&self, text: &str) -> Result<String, String> {
        let input: String = text.chars().take(8000).collect();
        let body = serde_json::json!({
            "model": SUM_MODEL,
            "temperature": 0.4,
            "messages": [
                {"role": "system", "content": SUM_SYSTEM},
                {"role": "user", "content": input},
            ],
        });
        let res = self
            .http
            .post(format!("{OPENAI}/chat/completions"))
            .header("Authorization", self.auth())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("resumo: {e}"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("resumo HTTP {status}: {}", snippet(&text)));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string())
    }

    /// Roteiro → áudio Ogg/Opus, pronto para `sendVoice`.
    pub async fn synthesize(
        &self,
        script: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Vec<u8>, String> {
        let input: String = script.chars().take(4000).collect();
        let body = serde_json::json!({
            "model": TTS_MODEL,
            "voice": voice,
            "input": input,
            "response_format": "opus",
            "speed": speed,
            "instructions": TTS_INSTRUCTIONS,
        });
        let res = self
            .http
            .post(format!("{OPENAI}/audio/speech"))
            .header("Authorization", self.auth())
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("tts: {e}"))?;
        let status = res.status();
        if !status.is_success() {
            let t = res.text().await.unwrap_or_default();
            return Err(format!("tts HTTP {status}: {}", snippet(&t)));
        }
        let bytes = res.bytes().await.map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
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
