# ADR-0006 — OpenAI Realtime via WebSocket

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Conversa por voz com baixa latência. Alternativas: WebRTC em Rust (webrtc-rs), WebRTC no WebView do Tauri, WebSocket server-side.

## Decisão
WebSocket direto do serviço (`tokio-tungstenite`), áudio PCM16 24 kHz via `cpal`, VAD do servidor ou PTT.

## Consequências
- (+) Implementação simples e totalmente em Rust; funciona sem janela aberta (atalho global); controle total do áudio.
- (−) Latência um pouco maior que WebRTC e sem controle de jitter nativo; mitigado com chunks de ~100 ms e ring buffers. WebRTC fica como evolução se a latência incomodar.
