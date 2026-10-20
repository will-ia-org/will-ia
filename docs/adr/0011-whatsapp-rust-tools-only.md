# ADR-0011 — WhatsApp via `whatsapp-rust`, exposto só como ferramentas MCP

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
O dono quer que o agente leia, resuma e envie mensagens no **seu** WhatsApp pessoal. Opções de biblioteca em Rust: `whatsapp-rust` (oxidezap, protocolo multi-device, ativa, MIT), fork `wa-rs`, bindings do `whatsmeow` (Go, imaturos), API Cloud oficial (exige conta business, não acessa a conta pessoal). Opções de papel: canal de entrada (como o Telegram) ou somente ferramentas.

## Decisão
`whatsapp-rust` fixado em versão exata, atrás de uma trait `WhatsAppBackend`. Pareamento por QR/código na UI. **Somente ferramentas MCP** (`whatsapp_*`); mensagens recebidas não criam threads nem acionam o cérebro. Índice local em SQLite para permitir não lidas, busca e resumo. Envios com política `confirmar` e rate limit por padrão.

## Consequências
- (+) O agente ganha visão e ação sobre o WhatsApp do usuário sem expor o número a respostas automáticas a terceiros.
- (+) Fluxo de entrada continua sendo Telegram/voz/UI, mantendo a allowlist simples.
- (−) Risco de suspensão do número (protocolo não oficial); mitigado por aviso, rate limit e confirmação. API 0.x pode quebrar; isolada pela trait.
- Canal de entrada via WhatsApp fica no backlog (ex.: só o chat "Você").
