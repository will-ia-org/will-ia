# ADR-0008 — Telegram com teloxide (sem dispatcher), modelo do projeto de referência

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
O projeto de referência (Node, zero deps, fetch direto na Bot API) está em produção e define o comportamento desejado: thread por tópico, pilha de status, finalize com reply, comandos, `/chave`, anexos, voice notes. Em Rust, opções: `teloxide` (completo, inclui fórum), `frankenstein` (fino), cliente próprio com `reqwest`.

## Decisão
`teloxide` apenas como cliente tipado da Bot API (`Bot` + requests), com **nosso** loop de long polling e roteamento, reproduzindo a estrutura da referência (`telegram`, `pool`→engine, `conversation`, `state`→storage). Não usar o dispatcher/dialogue framework.

## Consequências
- (+) Tipos corretos para `reply_parameters`, tópicos de fórum, `callback_query`, uploads; manutenção ativa.
- (+) Comportamento idêntico ao que já funciona; divergências documentadas (user allowlist adicional).
- (−) Dependência maior que um cliente mínimo; aceitável.
