# 16 — Glossário

| Termo | Significado |
|---|---|
| **Base de Conhecimento (KB)** | Pasta local escolhida pelo usuário onde o Claude Code roda (`cwd`). Contém `CLAUDE.md`, `.claude/`, `knowledge/`, `inbox/`, `tools/`. |
| **Cérebro / Motor** | O Claude Code CLI em modo headless, spawnado pelo serviço. |
| **Thread** | Unidade de contexto: um processo `claude` vivo (ou dormente) + uma sessão do Claude Code + origem (UI, Telegram, voz). |
| **Thread key** | Identificador de thread no Telegram: `<chat_id>` ou `<chat_id>:<message_thread_id>`. |
| **Turno** | Um ciclo pergunta → resposta final (`result`) dentro de uma thread. |
| **Steering / mid-turn** | Enviar nova mensagem ao processo enquanto um turno está em andamento; o Claude Code a incorpora. |
| **Pendência** | Entrada na fila de correlação de uma thread: mensagem enviada ainda sem `result`. |
| **Live / Dormant** | Thread com processo vivo / sem processo (sessão salva, volta com `--resume`). |
| **Retire** | Encerrar o processo de uma thread mantendo a sessão. |
| **Reset / Zerar** | Encerrar o processo e esquecer a sessão (começa do zero). |
| **Sessão expirada** | Transcrição apagada pelo Claude Code (`cleanupPeriodDays`); `--resume` falha e a thread recomeça. |
| **stream-json** | Formato NDJSON de entrada/saída do `claude -p`. |
| **Pilha de status** | Mensagem única no Telegram, editada ao vivo, mostrando o progresso do turno. |
| **Handoff** | Delegação do Realtime ao cérebro via função `ask_brain`, com resultado devolvido depois. |
| **PTT** | Push-to-talk: áudio só é enviado enquanto a tecla está pressionada. |
| **MCP** | Model Context Protocol; o serviço expõe um servidor MCP local com ferramentas para o agente. |
| **Bearer efêmero** | Token aleatório por processo `claude` que autentica chamadas MCP e identifica a thread. |
| **Token de subscription** | Token OAuth de longa duração do Claude Code (`claude setup-token`), injetado em `CLAUDE_CODE_OAUTH_TOKEN`. |
| **acceptEdits / bypassPermissions** | Modos de permissão do Claude Code: edita arquivos sem perguntar com allowlist de tools / tudo liberado. |
| **Módulo** | Subsistema ligável: Motor, Telegram, Voz, MCP, WhatsApp, Google. Cada um com `start/stop` e status. |
| **Política de ferramenta** | `auto` / `confirmar` / `desligada`; define se uma ferramenta MCP executa direto, pede aprovação ou fica oculta. |
| **Aprovação** | Pedido gerado por ferramenta em `confirmar`, decidido pelo dono na UI, notificação ou Telegram; expira por timeout. |
| **Módulo de ferramentas** | Módulo que não é canal de entrada (WhatsApp, Google): só expõe ferramentas MCP. |
| **JID** | Identificador de chat/contato no protocolo do WhatsApp (`<número>@s.whatsapp.net`, `<id>@g.us`). |
| **Label** | Marcador do Gmail; substitui pastas. Arquivado = sem `INBOX`. |
| **OAuth Client** | Credencial (`client_id`/`client_secret`) criada pelo usuário no Google Cloud para autorizar o will-ia. |
| **MCP interno / externo** | `will_ia`, servido pelo próprio serviço / servidores de terceiros catalogados e entregues via `.mcp.json`. |
| **`.mcp.json`** | Arquivo de MCPs por projeto do Claude Code, gerado pelo serviço na KB com a união de internos e externos; segredos como `${VAR}`. |
| **Degraded** | Estado de módulo que continua rodando mas com falha conhecida (auth, rate limit). |
| **App desktop / `will-desktop`** | Binário `will-ia`: UI nativa em egui/eframe + bandeja + atalho global + áudio, com o serviço rodando **no mesmo processo** (ADR-0016). |
| **Modo headless (`serve`)** | `will-ia serve`: só serviço + API HTTP local, sem janela nem bandeja; usado em servidores, testes (`smoke.sh`) e systemd sem sessão gráfica. |
| **Indicador flutuante** | Janela secundária do egui (sem decoração, sempre no topo) exibida durante a sessão de voz com estado ouvindo / pensando / falando e nível do microfone. |
| **Token da API local** | `<data_dir>/ui-session.token`: bearer da API HTTP em `127.0.0.1` (headless, automação, segunda instância). A UI nativa não o usa. |
