# ADR-0003 — Claude Code CLI headless como motor, processo vivo por thread

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Queremos o harness completo do Claude Code (tools de arquivo, Bash, subagentes, MCP, memória via CLAUDE.md, sessões persistentes) operando sobre uma pasta local, usando tokens de **subscription**. Chamar a API da Anthropic diretamente exigiria reimplementar o harness e não usaria a subscription. É essencial poder **acrescentar contexto no meio de um turno** (voz e Telegram continuam falando enquanto o cérebro trabalha).

Alternativas: (a) um processo `claude -p` por turno com `--resume`; (b) **um processo vivo por thread** com stdin aberto em stream-json; (c) Claude Agent SDK (Node/Python) embutido.

## Decisão
(b), igual ao projeto de referência `telegram-bot`: um processo `claude -p --input-format stream-json --output-format stream-json` por thread, stdin aberto, mensagens escritas na hora (inclusive mid-turn), fila de pendências para correlacionar `result`s, `session_id` guardado só para `--resume` quando o processo morre (ociosidade, teto, troca de token, crash, reinício). Nunca chamar a API da Anthropic diretamente.

## Consequências
- (+) Steering mid-turn nativo; sem custo de boot a cada turno; paralelismo por N processos.
- (+) Recuperação robusta: sessão expirada → recomeça e reenvia; troca de token sem downtime.
- (−) Memória por processo vivo → teto + idle timeout; supervisão de processos (grupos/Job Objects) é obrigatória.
- (−) Dependência do formato stream-json e das flags do CLI; parser tolerante e checagem de versão.
- (c) descartada: traria runtime Node/Python para dentro do serviço Rust sem ganho sobre o CLI.
