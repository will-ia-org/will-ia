# 26 — Separação cliente/servidor (fase 5)

**Status:** planejado em 2026-10-28, decisões fechadas com o usuário (ADR-0019). Nada implementado ainda; este documento é o mapa da fase.

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

Crates: `will-desktop` dá origem a dois crates: `will-server-bin` (o `will-ia` de hoje sem egui: serviço + bandeja + `serve`) e `will-app` (egui + cliente). A UI passa a depender de um **cliente tipado** (`will-client`: wrappers `reqwest` sobre `docs/22` + WS) em vez de chamar handlers do `will-server` em processo. `will-core` continua a base de tipos dos dois lados (serde dos payloads).

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
- `POST /api/ui/show` e `will-ia screenshot` saem do servidor; "segunda execução reabre a janela" vira responsabilidade do app (trinco próprio do app).

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

### Perfis do app
- Arquivo local do app (`<config>/will-ia-app/profiles.json`): lista de `{ nome, url, usuário, token de sessão (keyring do app), impressão digital TLS, último uso }`. Perfil "Esta máquina" é criado automaticamente quando há um servidor local (descoberta por `api-port` + `/api/health`).
- Troca de perfil: fecha WS, limpa snapshot, reconecta. Sem reiniciar o app.
- Onboarding do app: tela de conexão (URL, usuário, senha) → salva perfil → segue o onboarding de hoje se o servidor ainda não tem KB/token.

### O que se perde ou piora
- Duas coisas para instalar e manter rodando na máquina pessoal (servidor + app). Mitigação: instalador coloca os dois e o autostart sobe o servidor; o app abre pela bandeja ou atalho.
- Voz com latência de rede quando remoto.
- Diálogos nativos de pasta para a KB (vira navegação pela API).
- "Abrir pasta/arquivo" em modo remoto.
- Mais superfície de segurança (porta, contas, TLS, uploads). Compensa com papéis, rate limit e padrão loopback.

## Etapas (ordem proposta)

1. **Cliente tipado e UI desacoplada (sem mudar binários).** Criar `will-client` com funções `reqwest` para cada rota de `docs/22` e WS de eventos; trocar as chamadas em processo da UI por esse cliente apontando para loopback. Ao final, a UI não importa mais `will_server`. Comportamento idêntico, mesmo binário. *Maior etapa; é o coração da separação.*
2. **Uploads, arquivos e navegação de pastas pela API.** Anexos, miniaturas, `kb/browse`, `files`. Remover `std::fs` da UI (fora do app em si).
3. **Dois binários.** `will-ia` (servidor + bandeja opcional, sem egui) e `will-ia-app` (egui + cliente + perfis). Bandeja lança o app. Trinco e `show` do app. Pacotes/CI para os dois.
4. **Contas e sessões.** Migração (`users`, `sessions`, `user_id` nas tabelas), `/api/auth/login|logout|me`, `/api/users` (admin), bootstrap do admin, papéis nas rotas, autor nos turnos/aprovações/custos, tela de usuários e de login.
5. **Rede.** `bind` configurável, TLS opcional + TOFU no app, rate limit, CORS, `--file-secrets` automático sem keyring.
6. **Voz multiusuário.** `RealtimeManager` por usuário, teto, custo por usuário, voz do app pelo WS (já existe) como único caminho.
7. **Servidor headless de verdade.** Chrome headless forçado, notificações só para apps, documentação de instalação em Linux server (systemd user unit ou system unit com `--file-secrets`), guia de exposição segura (Tailscale/nginx).

Cada etapa termina com `cargo test`, `smoke.sh` e teste manual do app em loopback; a partir da 3, também app remoto contra o servidor nesta máquina.

## Fora de escopo desta fase
Threads privadas por usuário, múltiplas KBs por servidor, federação entre servidores, app web/móvel, streaming da tela do navegador para o app.
