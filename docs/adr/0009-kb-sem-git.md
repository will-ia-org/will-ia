# ADR-0009 — Base de conhecimento não versionada pelo serviço

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Seria possível auto-commitar a KB após cada turno para auditoria e rollback.

## Decisão
Por enquanto o serviço apenas aponta para a pasta e cria o esqueleto inicial. Não executa git nem monitora alterações.

## Consequências
- (+) Menos acoplamento e menos surpresas (repos existentes, git-crypt, submódulos).
- (−) Sem histórico automático; o usuário ou o próprio agente (via `CLAUDE.md`) podem versionar. Revisitar no backlog.
