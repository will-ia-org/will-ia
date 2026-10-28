# 26 — Separação cliente/servidor (fase 5)

**Status:** decisões fechadas com o usuário em 2026-10-28 (ADR-0019). **Fase concluída em 2026-10-28 (etapas 1–7)**: dois binários, `will-ia` (servidor) e `will-ia-app` (app); contas por pessoa com sessões; rede com `bind` e TLS embutido; voz por usuário; servidor headless. Pendências menores em "O que ficou de fora". Este documento é o mapa da fase e o registro do que entrou.

## Objetivo

Separar o `will-ia` em dois binários com **comportamento idêntico ao de hoje para quem usa**:

- **`will-ia`** (servidor): o serviço residente de hoje, sem janela. Fica na bandeja quando a máquina tem desktop e roda headless em servidor Linux sem tela. Escuta numa porta (loopback por padrão; IP:porta quando exposto) e é o único dono de motor, KB, módulos, segredos e banco.
- **`will-ia-app`** (frontend): a janela egui de hoje, agora um **cliente** do servidor. É "one-shot": abriu, conecta num servidor, fechou, acabou. Tem **perfis** de conexão (servidor da equipe, instalação local, outro servidor) e troca entre eles sem reiniciar.

Casos de uso: (1) um servidor sempre ativo para a equipe toda falar com o **mesmo agente** sobre a **mesma KB**; (2) instância pessoal em cada máquina (servidor + app em loopback); (3) o mesmo app alternando entre os dois.

## Decisões (resumo da ADR-0019)

| Tema | Decisão |
|---|---|
| Protocolo | **HTTP/2 + WebSocket**, reaproveitando a API local (`docs/22`) e o WS de eventos. Sem gRPC: já há um contrato pronto e rápido o bastante para chat/eventos/áudio. |
| Modo em processo | **Removido.** O app é sempre cliente, inclusive na máquina pessoal (loopback). Um caminho só de código. |
| Usuários | **Contas por pessoa, threads compartilhadas.** Login identifica quem fez o quê (turnos, aprovações, custos, logs) e permite revogar acesso; todas as conversas são visíveis a todos. |
| Login | Usuário + senha (argon2 no SQLite do servidor) → token de sessão guardado no perfil do app. Papéis `admin` e `membro`. |
| Bootstrap | Primeira subida cria `admin` com senha aleatória impressa no log e em `will-ia info`; troca obrigatória no primeiro login. |
| TLS | Opcional, embutido (rustls): certificado próprio ou autoassinado gerado pelo servidor; o app pode fixar a impressão digital. Padrão HTTP puro (loopback/VPN/proxy). |
| Voz | Microfone/alto-falante no **app**, sessão OpenAI no **servidor**, áudio pelo WebSocket. **Uma sessão por usuário** conectado, com teto configurável; custo por usuário. |
| Bandeja/atalho | Bandeja é do servidor (quando há desktop); "Abrir will-ia" lança o app. Atalho PTT é do app e vale enquanto ele estiver aberto. |
| Browser CDP | No servidor. Sem tela → Chrome headless obrigatório (detectado por ausência de `DISPLAY`/sessão gráfica). |

## Arquitetura alvo

```
┌──────────────── will-ia-app (egui) ────────────────┐      HTTP/2 (+TLS opcional)      ┌──────────────── will-ia (servidor) ────────────────┐
│ perfis de conexão · login · páginas de hoje        │ ───────────────────────────────► │ API axum (docs/22 + /api/auth, /api/users, uploads) │
│ cliente HTTP (reqwest) + WS eventos + WS áudio     │ ◄──── WS /api/events (por usuário) │ AppState · módulos · motor · KB · banco · segredos   │
│ cpal (mic/alto-falante) · hotkey PTT · rfd (só     │ ◄──── WS /api/voice/audio ──────► │ RealtimeManager por usuário · bandeja (se desktop)  │
│ para escolher arquivos a enviar) · toasts/notif.   │                                   │ instance.lock · residente · headless em servidor    │
└────────────────────────────────────────────────────┘                                   └─────────────────────────────────────────────────────┘
```

Crates (como ficou): `will-desktop` deu origem a `crates/will-ia` (binário `will-ia`: serviço + bandeja + `serve`, sem egui) e `crates/will-app` (binário `will-ia-app`: egui + cliente + perfis). A UI depende do **cliente tipado** `crates/will-client` (wrappers `reqwest` sobre `docs/22` + WS de eventos + WS de áudio) e não importa `will-server`. `will-core` continua a base de tipos dos dois lados (serde dos payloads; `Event` ganhou `Deserialize`).

## Impactos, item a item

### O que não muda
Motor Claude, KB, Telegram, WhatsApp, Google, Scheduler, servidor MCP, políticas/aprovações, custos, logs, `.mcp.json`, Browser CDP: tudo continua no servidor, com o mesmo código. A API HTTP já cobre todas as páginas da UI (levantamento de 2026-10-28: ~80 handlers, todos já expostos em `docs/22`).

### UI → cliente de rede
- Hoje a UI chama os handlers axum **em processo** (`cx.svc.call(|s| handler(State(s), …))`) e monta o snapshot lendo `AppState` direto (settings, status dos módulos, aprovações pendentes, caminhos). Passa a: `GET /api/status`, `/api/settings`, `/api/approvals`, e o WS `/api/events` para manter o snapshot. Latência de rede aparece pela primeira vez: a UI precisa de estados "carregando" e tolerar erros de conexão (reconexão automática do WS com backoff, banner "desconectado").
- `Loader`/`Task` continuam; só muda o backend das chamadas.

### Arquivos e disco
| Hoje | Depois |
|---|---|
| Seletor nativo de pasta da KB (`rfd`) grava caminho local | Campo de texto com caminho **no servidor** + `GET /api/kb/browse?path=` para navegar pastas do servidor (admin). |
| Anexar arquivo: UI copia para `<kb>/inbox/ui/` | `rfd` só escolhe o arquivo; upload `POST /api/threads/{id}/attachments` (multipart, limite configurável); servidor grava em `inbox/ui/` e gera a nota de anexo. |
| Miniaturas de anexos/capturas lidas do disco | `GET /api/files?path=` restrito à KB (somente leitura, sem `..`). |
| "Abrir pasta" (KB, logs, data_dir) | Some em modo remoto; em loopback continua (o app sabe quando o servidor é local). |
| "Ver .mcp.json" | Já vem pela API. |
| Portátil (pendrive) | Servidor + app no mesmo pendrive; app conecta em loopback. |

### Voz
- Áudio PCM do microfone do app → `WS /api/voice/audio` → servidor → OpenAI; resposta volta pelo mesmo WS (já existe para o headless, `docs/07`). Jitter buffer já existe no app.
- `RealtimeManager` deixa de ser singleton: `HashMap<UserId, Session>`, `max_voice_sessions` nas configurações, thread de voz por usuário ("Voz · Ligia"), custo em `ai_costs.user_id`. Indicador flutuante e PTT continuam no app.
- Latência adicional = rede ida e volta; aceitável em LAN/VPN, perceptível em links ruins.

### Bandeja, atalho, notificações, segunda instância
- Servidor com desktop: bandeja com "Abrir will-ia" (lança `will-ia-app` apontando para o perfil local), "Ligar/Desligar voz" sai da bandeja (voz é do app).
- Servidor headless: sem bandeja; `will-ia serve` continua existindo (é o mesmo binário; a bandeja é ativada só se há sessão gráfica).
- `Event::Notification` passa a ser entregue aos apps conectados (toast + notificação nativa **no app**); no servidor só se houver desktop e ninguém conectado (configurável).
- `POST /api/ui/show` e `/api/ui/screenshot` **ficam** no servidor, mas agora lançam/acionam o app (`will-ia-app` ao lado do executável ou no PATH); `will-ia show` e `will-ia screenshot --nav` delegam ao app. "Segunda execução reabre a janela" é do app (trinco `app.lock` + ipc TCP em loopback em `<config>/will-ia-app/ipc`); a captura vai para `<config>/will-ia-app/screenshot.png`.

### Segurança e multiusuário
- Novo: tabela `users` (id, nome, e-mail, hash argon2, papel, ativo, criado em) e `sessions` (token hash, user_id, expira, último uso, cliente). `Authorization: Bearer` passa a ser token de sessão; o token único atual vira "token de scripts" (já existe para MCP) e deixa de abrir a UI.
- `user_id` opcional em `turns`, `approvals`, `ai_costs`, `events_log`, `voice_transcripts`, `schedules.created_by` (quem pediu). Threads continuam sem dono (compartilhadas), mas cada turno sabe quem escreveu; a UI mostra o autor no balão.
- Aprovações: continuam para todos os conectados (threads compartilhadas); registra quem decidiu.
- Telegram: allowlist de usuários do Telegram pode ser **mapeada** a contas (opcional) para o autor aparecer certo.
- Papéis: `admin` mexe em configurações, segredos, usuários, políticas; `membro` conversa, aprova, agenda, usa voz e vê logs/custos. Toda rota de configuração exige `admin`.
- Segredos continuam só no servidor. Servidor headless costuma não ter keyring: `--file-secrets` (já existe) vira padrão quando não há Secret Service/Credential Manager.
- Porta: `bind` configurável (`127.0.0.1` padrão; `0.0.0.0` só com TLS **ou** confirmação explícita). TLS opcional com `cert.pem`/`key.pem` ou autoassinado gerado; o app fixa a impressão digital no perfil na primeira conexão (TOFU) e avisa se mudar. Limite de tentativas de login por IP.
- CORS fechado (só o app). Rate limit básico nas rotas de login e upload.

### Browser CDP no servidor
- Sem `DISPLAY`/sessão gráfica: `headless` forçado (aviso na página se o usuário desligar). Perfil do Chrome fica no servidor; "copiar meu perfil" só faz sentido quando app e servidor estão na mesma máquina (ou por upload de um zip do perfil, fase posterior).
- Capturas em `<kb>/inbox/browser/` do servidor; a UI vê por `GET /api/files`.
- Chrome visível no servidor com desktop continua possível (equipe vê a janela no servidor, não no app). Streaming da tela do navegador para o app fica para depois (o agent-browser tem `stream enable`).

### Perfis do app (como está na etapa 3)
- Arquivo local do app `<config>/will-ia-app/profiles.json` (`~/.config/will-ia-app/` no Linux, `%APPDATA%\will-ia-app\` no Windows, `~/Library/Application Support/will-ia-app/` no macOS; permissão **0600** no Unix), `crates/will-app/src/profiles.rs`:

  ```json
  { "last": "local", "profiles": [ { "id": "p1761660000000", "name": "servidor da equipe", "url": "http://192.168.0.10:47300", "token": "…" } ] }
  ```

  Desde a etapa 4 o `token` é o **token de sessão** devolvido por `POST /api/auth/login` (usuário + senha na página Conexão), guardado **em texto** nesse arquivo (0600); o keyring do app fica para depois. Para o perfil "Esta máquina" vale também o token local da API (`ui-session.token`, admin implícito), sem login. A etapa 5 acrescenta a impressão digital TLS.
- Perfil **"Esta máquina"** (`id = local`) não é gravado: é descoberto a cada execução lendo `<data_dir>/api-port` e `<data_dir>/ui-session.token` do servidor local (respeita `WILL_IA_DATA_DIR` e o modo portátil). Se não há servidor local, a página Conexão avisa.
- Ordem de escolha no arranque (`profiles::pick`): `--url` (+ `--token`) > `--profile <nome|id|local>` > `last` de `profiles.json` > "Esta máquina" > primeiro perfil salvo. Sem nenhum, o app abre na página **Conexão** (`Nav::Connect`, `docs/10`).
- Troca de perfil (página Conexão › Conectar): fecha o WS, limpa o snapshot, reconecta e grava `last`. Sem reiniciar o app.
- Onboarding do app: página Conexão (URL + usuário + senha, ou token local) → salva perfil → se `must_change_password`, troca de senha obrigatória → segue o onboarding de hoje se o servidor ainda não tem motor/KB.

### O que ficou de fora (depois da fase)
- `user_id` em `ai_costs`/`events_log`/`voice_transcripts`/`schedules.created_by`: hoje a autoria fica em `turns.author`, `approvals.decided_via` e, nos custos da voz, em `ai_costs.note = "user:<u>"`.
- Mapeamento Telegram → conta (autor dos turnos do Telegram continua `NULL`).
- Keyring do app para o token de sessão e o `ca_pem` do perfil (ficam em texto em `profiles.json`, 0600).
- Configurações de rede a quente: `bind`/TLS valem só no próximo início do servidor.
- Rate limit em uploads; `GET /api/settings` continua legível por membros (só `PUT` é de admin).
- Streaming da tela do navegador para o app; "copiar meu perfil" do Chrome para servidor remoto.
- Certificado autoassinado sem renovação automática (sem validade curta; regenerar = apagar `<data_dir>/tls/`).

## O que se perde ou piora
- Duas coisas para instalar e manter rodando na máquina pessoal (servidor + app). Mitigação: instalador coloca os dois e o autostart sobe o servidor; o app abre pela bandeja ou atalho.
- Voz com latência de rede quando remoto.
- Diálogos nativos de pasta para a KB (vira navegação pela API).
- "Abrir pasta/arquivo" em modo remoto.
- Mais superfície de segurança (porta, contas, TLS, uploads). Compensa com papéis, rate limit e padrão loopback.

## Etapas

1. ✅ **Cliente tipado e UI desacoplada (sem mudar binários)** — commit `cb9ca0f`. Entrou: crate `will-client` (um método por rota de `docs/22` + genéricos, WS `/api/events` com reconexão, WS `/api/voice/audio`); a UI deixou de chamar handlers axum em processo e de ler o `AppState` (`Service::call` entrega um `Arc<Client>`; snapshot por `/api/status` + `/api/settings` + `/api/approvals`; eventos pelo WS); voz com microfone/alto-falante no app e sessão no servidor (`VoiceLink`); ações locais (abrir pasta, `rfd` da KB, miniaturas `file://`) só quando o servidor é esta máquina; `Event: Deserialize`; `/api/status` expõe `logs_dir`. Ainda um binário, comportamento idêntico.
2. ✅ **Uploads, arquivos e navegação de pastas pela API** — commit `48ab287`. Entrou: `POST /api/uploads?name=` (corpo bruto → `<kb>/inbox/ui/`, 64 MiB), `GET /api/files?path=` (só dentro da KB; 403 fora), `GET /api/kb/browse?path=` (subpastas do servidor, avisos de pasta perigosa/sem escrita); anexos já no inbox entram na mensagem sem nova cópia; no app remoto anexos sobem por upload antes do envio, miniaturas e "abrir anexo" via `/api/files` (cache em memória, download para pasta temporária) e o seletor da KB vira um modal que navega o disco do servidor; em loopback tudo continua local.
3. ✅ **Dois binários** — 2026-10-28. Entrou: `crates/will-desktop` virou `crates/will-app` (bin `will-ia-app`: egui, cliente via `will-client`, sem `will-server`) e nasceu `crates/will-ia` (bin `will-ia`: servidor residente + bandeja `ksni`/`tray-icon` com o único item "Abrir will-ia", lançador do app ao lado do executável ou no PATH, sem bandeja quando não há `DISPLAY`/`WAYLAND_DISPLAY`; `serve` headless com Ctrl+C limpo; `info`, `show`, `screenshot --nav` delegando ao app; `POST /api/ui/show` e `/api/ui/screenshot` lançam/acionam o app). No app: perfis (`profiles.json`, "Esta máquina" por `api-port` + `ui-session.token`), página **Conexão** (`Nav::Connect`), CLI `[--show|--hidden] [--url --token] [--profile]`, `show`, `screenshot --nav` (→ `<config>/will-ia-app/screenshot.png`), `audio-devices`, trinco `app.lock` + ipc TCP em loopback. Pacotes/CI para os dois (`ci.yml`, `release.yml`, `.deb` com os dois binários, `.app` com `will-ia-app` principal + `will-ia` ao lado, portátil com `bin/will-ia-*` e `bin/will-ia-app-*`; `packaging/linux/will-ia.desktop` é do app, `packaging/systemd/will-ia.service` é `will-ia serve`). Para o usuário nada muda, exceto que na máquina pessoal rodam dois processos.
4. ✅ **Contas e sessões** — 2026-10-28. Entrou: migration `0006_users.sql` (`users`, `sessions`, `turns.author`); `will_core::auth` (`Principal`, `Role`); `will-server::auth` (argon2id em PHC string, token de sessão de 32 bytes guardado por sha256, validade fixa de 30 dias, `LoginLimiter` 10 tentativas/15 min por IP, `bootstrap_admin` cria `admin` com senha aleatória em `<data_dir>/admin-initial-password.txt` 0600, mostrada por `will-ia info` e apagada na primeira troca); rotas `POST /api/auth/login|logout|password`, `GET /api/auth/me`, `GET/POST /api/users`, `PATCH/DELETE /api/users/{id}` (desativar, nunca apagar), `GET /api/users/{id}/sessions`; middleware `auth` (token local = admin implícito `local`; sessão = usuário) + `admin_gate` (403 para membros nas rotas de configuração, lista em `docs/22`); autoria: `turns.author` via `ThreadHost::send_message_from`, `approvals.decided_via = "ui:<username>"`; `will-client` com `login/logout/me/change_password/users/create_user/patch_user/deactivate_user/user_sessions`. App: Conexão com usuário + senha (token no perfil), troca de senha obrigatória, página **Usuários** (admin), autor no balão, menu de configurações só para admin (`docs/10`). Ficou de fora, para depois: `user_id` em `ai_costs`/`events_log`/`voice_transcripts`/`schedules.created_by`, mapeamento Telegram → conta, keyring do app para o token.
   Detalhe original da etapa: Migração (`users`, `sessions`, `user_id` nas tabelas), `/api/auth/login|logout|me`, `/api/users` (admin), bootstrap do admin, papéis nas rotas, autor nos turnos/aprovações/custos, tela de usuários e de login.
5. ✅ **Rede** — 2026-10-28. Entrou: `NetworkSettings { bind, tls, cert_path, key_path, allow_remote_without_tls }` (seção `network`, aplicada no próximo início) e `will-ia --bind` (sobrepõe); regra: `bind` fora do loopback **sem** TLS e sem `allow_remote_without_tls` → o servidor **volta para 127.0.0.1** com erro no log/`/api/logs`; TLS embutido via `axum-server`/rustls com certificado próprio (`cert_path`/`key_path`) ou **autoassinado gerado por `rcgen`** em `<data_dir>/tls/cert.pem|key.pem` (SANs `localhost`, hostname e o IP do `bind`; chave 0600); se o TLS falha, serve HTTP e registra erro; `ServerHandle::url` com `http`/`https`; `AppState.scheme/tls_fingerprint/tls_cert_path`; `GET /api/health` devolve `{ok, version, scheme, tls_fingerprint_sha256}` e `GET /api/network/cert` (admin) `{path, pem, fingerprint_sha256}`; impressão digital = SHA-256 do DER. `will-client`: `TlsOptions { ca_pem, insecure }`, `Client::with_tls`, `connect_ws` com a mesma política no WS, `pem_fingerprint`. CORS **removido** (o app não é navegador; nada de `Origin`). `is_admin_path` ampliado (lista em `docs/22`). App: perfil com `ca_pem`/`insecure`, seção Configurações → Rede, Conexão mostra/fixa o certificado do servidor. Fora: TOFU automático (o admin copia o PEM pela UI; o app fixa a CA, não só a impressão digital), rate limit de upload, `--file-secrets` automático.
6. ✅ **Voz multiusuário** — 2026-10-28. Entrou: `AppState.voice: HashMap<username, Arc<RealtimeManager>>` (`modules::voice_manager_for`), um `RealtimeManager` por usuário com `RealtimeDeps.owner`; `BrainHost` por usuário (`orchestrator::UserBrain`): thread de voz **"Voz · <user>"** (ou "Voz" para o token local) por `Host::voice_thread_for`, autor `voz:<user>` nos turnos; `RealtimeSettings.max_sessions` (padrão 3) conta as sessões **ativas de outros usuários** (`check_capacity` → 400 "limite de N sessão(ões) de voz simultânea(s) atingido"); `POST /api/voice/start|stop|toggle` e `GET /api/voice/status` por `Principal` (`status` devolve `user`, `sessions[] {user, active, state}`, `max_sessions`, `active_sessions`); `WS /api/voice/audio` liga à sessão do usuário do token e é o **único** caminho de áudio; `Event::Voice { state, detail, user }`; custo em `ai_costs` com `note: "user:<u>"`; `start_realtime` reaplica config/chave a todas as sessões; `recompute_realtime_status`. App: Voz com "Sessões simultâneas" e lista de sessões. Detalhe original:
   `RealtimeManager` por usuário, teto, custo por usuário, voz do app pelo WS (já existe) como único caminho.
7. ✅ **Servidor headless de verdade** — 2026-10-28. Entrou: `will_platform::has_desktop()` (`DISPLAY`/`WAYLAND_DISPLAY`; sempre verdadeiro em Windows/macOS) usado por: Browser CDP (**`--headless=new` forçado** sem desktop, mesmo com `headless = false`), notificações (`Event::Notification` só vira notificação nativa no servidor com desktop; os apps recebem pelo WS e notificam na máquina deles quando a janela está oculta), bandeja do `will-ia`. Docs: instalação em Linux server (`docs/13`: systemd user unit + linger, `--file-secrets`, `--bind`/TLS) e exposição segura (Tailscale/WireGuard, nginx com TLS próprio). Detalhe original: Chrome headless forçado, notificações só para apps, documentação de instalação em Linux server (systemd user unit ou system unit com `--file-secrets`), guia de exposição segura (Tailscale/nginx).

Cada etapa termina com `cargo test`, `smoke.sh` e teste manual do app em loopback; a partir da 3, também app remoto contra o servidor nesta máquina.

## Fora de escopo desta fase
Threads privadas por usuário, múltiplas KBs por servidor, federação entre servidores, app web/móvel, streaming da tela do navegador para o app.
