# 14 — Boas Práticas e Padrões de Engenharia

## Rust

- **Edition 2021**, toolchain estável fixada em `rust-toolchain.toml`. MSRV = a exigida por `eframe`/`egui` e `cpal`.
- **Workspace** com `[workspace.dependencies]` centralizando versões; `[workspace.lints]` com `clippy::all`, `clippy::pedantic` (seletivo), `unsafe_code = "forbid"` fora de `will-platform`.
- **Erros**: `thiserror` nos crates de biblioteca (enums por domínio: `EngineError`, `TelegramError`…), `anyhow` apenas no binário. Nunca `unwrap()` fora de testes; `expect()` só com invariantes documentadas.
- **Async**: `tokio` em todo lugar; nenhum `std::sync::Mutex` segurado através de `.await`; estado compartilhado por **atores** (tasks com `mpsc`) e `broadcast` para eventos. I/O bloqueante (keyring, SQLite síncrono se houver) em `spawn_blocking`.
- **Cancelamento**: `tokio_util::sync::CancellationToken` propagado para toda task de módulo; shutdown gracioso com timeout.
- **Segredos**: `secrecy::SecretString`; `zeroize` onde fizer sentido.
- **Logs**: `tracing` com spans (`thread_id`, `turn_id`, `module`); `tracing-subscriber` com camada de arquivo rotativo + camada de UI (canal para a aba Logs) + redator de segredos. Sem `println!`.
- **Tempo**: `chrono` com `Utc` no banco e conversão para local só na UI.
- **IDs**: `uuid` v7 (ordenáveis) para threads e turnos.
- **Serialização**: `serde` com `#[serde(deny_unknown_fields)]` em entradas externas (stream-json do Claude é a exceção: tolerante, com `#[serde(other)]`).
- **Plataforma**: diferenças isoladas em `will-platform` com `cfg(target_os)`; demais crates são portáveis e testáveis em qualquer SO.
- **Testes**: unitários junto do código; integração em `tests/` com `fake-claude` e mocks HTTP (`wiremock`); testes de parser com fixtures reais anonimizadas; `cargo nextest` no CI.
- **Documentação**: `//!` em cada crate explicando papel e limites; `cargo doc --no-deps` sem warnings.

## Interface (egui/eframe, `will-desktop`)

- Modo imediato: cada página lê um **snapshot** do estado (`Arc<AppState>` em processo) e dispara ações assíncronas no runtime tokio; respostas voltam por canais + `request_repaint`. Nunca bloquear a thread da UI com `.await` ou I/O.
- Sem lógica de negócio: apenas formulários, listas e streaming de eventos; validação espelha a do backend (erros por campo).
- Repaint dirigido por eventos do `EventBus` (streaming de turnos, status de módulos), não por polling.
- Textos em pt-BR; tema claro/escuro seguindo o sistema; acessibilidade básica: navegação por teclado nas seções, labels em todos os campos.

## Git e versionamento

- **Conventional Commits** (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, com escopo do crate: `feat(engine): …`).
- Branch principal `main` protegida; trabalho em branches curtas; PR com CI verde.
- **SemVer**; changelog gerado por `git-cliff`.
- Não commitar: `telegram-bot.zip`, `*.db`, logs, `tokens*.json`, `.env`. (Já no `.gitignore`.)

## Estrutura de uma feature

1. Atualizar/registrar requisito em `docs/02` ou `03`.
2. Se muda arquitetura: ADR em `docs/adr/`.
3. Tipos e trait em `will-core` → implementação no crate do módulo → wiring em `will-server` (e página/ação em `will-desktop` se houver UI).
4. Testes no crate; teste de integração se cruza módulos.
5. Atualizar `CLAUDE.md` se mudou convenção ou comando.

## Padrões específicos do domínio

- **Nunca bloquear a entrada do usuário**: `Engine::send` retorna imediatamente após escrever no stdin; feedback vem por eventos.
- **Idempotência de módulos**: `start()`/`stop()` podem ser chamados repetidamente sem efeito colateral.
- **Degradação graciosa**: módulo em erro vira `Degraded` e mostra motivo; nunca derruba o app.
- **Tudo que o agente faz fora da KB passa pelo MCP** e fica registrado.
- **Texto para humanos é configurável**: mensagens de bot, persona de voz e system prompt adicional ficam em settings com padrão sensato, não hardcoded.

## Revisão de código (checklist)

- [ ] Segredo pode aparecer em log, erro ou UI?
- [ ] Task nova tem `CancellationToken` e é encerrada no shutdown?
- [ ] Processo filho tem kill em cascata (grupo/job)?
- [ ] Erro novo está classificado e tem mensagem acionável?
- [ ] Funciona com Telegram/Realtime desabilitados?
- [ ] Teste cobre o caminho de erro, não só o feliz?
