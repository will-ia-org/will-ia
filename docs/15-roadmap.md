# 15 — Roadmap

Fases incrementais; cada uma termina com algo usável.

## Fase 0 — Fundação (scaffold)
- Workspace Cargo, crates vazios com traits de `will-core`, app desktop com tray e janela de settings "Geral" (originalmente Tauri; ver Fase 3c).
- SQLite + migrations + keyring; `Settings` persistido; onboarding mínimo (token + pasta KB).
- CI nos três SOs; `fake-claude` para testes.

## Fase 1 — Cérebro + UI de threads (MVP interno)
- `will-engine`: runner por thread (stdin aberto, stream-json, replay, fila de pendências, retire/teto/idle, sessão expirada, troca de token sem downtime).
- Seção Motor Claude (tokens, permissões, allowlist) e Base de Conhecimento (criar esqueleto).
- Seção Threads: criar, conversar com streaming, enviar mensagem mid-turn, reiniciar, zerar, excluir.
- **Critério de pronto**: conversar com uma thread pela UI, mandar uma segunda mensagem durante o processamento e vê-la incorporada; reiniciar o app e continuar a thread.

## Fase 2 — MCP + Telegram
- `will-mcp` com bearer por processo e `telegram_send_message`, `notify_user`.
- Geração do `<kb>/.mcp.json` (união) + `settings.local.json`; seção **MCPs externos** com adicionar/testar/importar e segredos via `${VAR}`.
- `will-telegram` completo conforme doc 08 (tópicos, pilha de status, comandos, anexos, `/chave`).
- **Critério de pronto**: abrir um tópico no supergrupo, conversar com steering mid-turn, e o agente enviar uma mensagem proativa via MCP.

## Fase 2b — Políticas de aprovação + Google (Agenda e Gmail)
- `will-core::approvals`, políticas por ferramenta, UI de aprovações, botões no Telegram.
- `will-google`: OAuth PKCE/loopback com client do usuário, múltiplas contas, escopos incrementais.
- Agenda: multi-check de agendas, `calendar_*` leitura e escrita com `confirmar`.
- Gmail: `gmail_*` leitura, busca, anexos, envio com `confirmar`, rascunhos.
- **Critério de pronto**: pelo Telegram, pedir "o que tenho amanhã e quais e-mails não lidos importam", e pedir para marcar uma reunião que exige aprovação.

## Fase 3 — Voz
- `will-realtime`: WS, áudio cpal, toggle e PTT, `ask_brain` com handoff assíncrono, barge-in, transcrições na thread.
- Indicador flutuante e atalho global com fallback.
- **Critério de pronto**: pedir por voz algo que exige a KB, continuar conversando, ouvir o resultado quando chegar.

## Fase 3b — WhatsApp (ferramentas)
- `will-whatsapp` com `whatsapp-rust`: pareamento por QR/código na UI, índice local com FTS, `whatsapp_*` leitura e envio com `confirmar` e rate limit.
- **Critério de pronto**: "resuma as mensagens não lidas do grupo X" e "responda ao fulano dizendo Y" (com aprovação).

## Fase 3c — UI nativa egui (ADR-0016)
- `crates/will-desktop` (egui/eframe): janela principal com as mesmas páginas da UI web (Dashboard, Threads, Aprovações, Logs, Configurações), bandeja (`tray-icon`/`ksni`), atalho global (`global-hotkey`), seletor de pasta (`rfd`), áudio `cpal` no app, indicador flutuante de voz, QR do WhatsApp na janela.
- Modos `will-ia`, `will-ia --show`, `will-ia serve`; segunda instância via `POST /api/ui/show`.
- Remoção de `ui/` (React) e `apps/will-desktop` (Tauri).
- **Critério de pronto**: onboarding, conversar com uma thread e usar voz sem abrir navegador; `scripts/smoke.sh` continua passando no modo headless.

## Fase 4 — Endurecimento e distribuição
- GitHub Actions (`ci.yml`, `release.yml`) com matriz Linux x64, Windows x64, macOS x64 e arm64; pacotes por SO (`docs/13`); assinatura/notarização quando houver certificado; autostart testado nos três SOs.
- Exportar diagnóstico, retenção de logs, métricas na UI.
- Documentação de usuário final.

## Fase 5 — Separação cliente/servidor (planejada em 2026-10-28; `docs/26`, ADR-0019)

1. Cliente tipado `will-client` e UI desacoplada (ainda um binário).
2. Uploads, arquivos e navegação de pastas pela API.
3. Dois binários: `will-ia` (servidor + bandeja opcional) e `will-ia-app` (egui + perfis).
4. Contas e sessões (`users`, `sessions`, autor em turnos/aprovações/custos), papéis, bootstrap do admin.
5. Rede: `bind`, TLS opcional + TOFU, rate limit, segredos em arquivo sem keyring.
6. Voz multiusuário (sessão por usuário, teto, custo por usuário).
7. Servidor headless de verdade (Chrome headless forçado, notificações para apps, guia de instalação/exposição).

## Backlog (pós-MVP)
- Voice notes no Telegram com transcrição e resumo em áudio (já desenhado, herdado da referência).
- Ferramentas MCP de delegação entre threads.
- WhatsApp como canal de entrada restrito ao chat "Você" do dono.
- Gmail `history.list` para sincronização incremental barata; `gmail_modify` completo.
- IMAP genérico para contas não-Google.
- Modo proxy opcional para MCPs externos (reexpor sob `will_ia` com políticas de aprovação).
- Google Contacts e Tasks como novas famílias de ferramentas.
- Lock por arquivo para escritas concorrentes na KB (hook do Claude Code).
- Versionamento git opcional da KB.
- i18n completo (en).
- Auto-update assinado.
- Perfis múltiplos de KB.
