# ADR-0007 — Vários tokens Claude, um ativo por vez, troca sem downtime

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
O usuário tem mais de uma subscription. Alternativas: round-robin com failover automático; atribuição manual por canal; um ativo por vez.

## Decisão
Catálogo de tokens com exatamente um ativo. Troca manual (UI ou `/chave` no Telegram), precedida de teste em processo descartável. Ao trocar, derrubar todos os processos vivos; threads voltam com `--resume` e o token novo na próxima mensagem.

## Consequências
- (+) Previsível, simples, auditável; herda o fluxo testado da referência; zero perda de contexto na troca.
- (−) Sem failover automático em rate limit (o motor vira `Degraded` e sugere trocar). Pode evoluir para failover depois.
