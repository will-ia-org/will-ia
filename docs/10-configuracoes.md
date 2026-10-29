# 10 — Configurações (UI e esquema)

Janela nativa (egui/eframe, crate `will-app`, binário `will-ia-app`, cliente do servidor `will-ia`; `docs/26`) com navegação lateral: páginas Dashboard, Threads, Aprovações, Agendamentos, Browser CDP, Logs, Custos, **Conexão** e Configurações (seções abaixo). Cada seção mapeia para um sub-struct de `Settings`. Segredos não ficam em `Settings`: ficam no keyring e `Settings` guarda apenas referências (`secret_ref`).

## Seções e campos

### Geral
| Campo | Tipo | Padrão |
|---|---|---|
| Iniciar com o sistema | bool | true |
| Idioma da UI | `pt-BR` \| `en` | `pt-BR` |
| Nível de log | `error`…`trace` | `info` |
| Notificações nativas | bool | true |
| Tema | `system` \| `light` \| `dark` | `system` |

### Motor Claude
| Campo | Tipo | Padrão |
|---|---|---|
| Caminho do binário `claude` | path (auto-detectado) | `claude` no PATH |
| Tokens de subscription | lista `{ id, rótulo, secret_ref, ativo, status, verificado_em, erro }` | — |
| Modelo | string opcional (alias ou nome) | vazio (padrão do CLI) |
| Modo de permissão | `acceptEdits` \| `bypassPermissions` | `acceptEdits` |
| Ferramentas permitidas (acceptEdits) | lista de strings | allowlist padrão do doc 05 |
| Processos vivos (máx.) | int 1–20 | 5 |
| Timeout de ociosidade | minutos | 10 |
| Orçamento máx. por turno (USD) | float opcional | vazio |
| Effort | `low`…`max` opcional | vazio |
| Incluir mensagens parciais | bool | false |
| Gravar stream bruto em log | bool | false |
| Texto adicional de system prompt | textarea | modelo padrão |

Subseção **Resumo das threads** (`engine.thread_summary`, lida a cada turno concluído, sem reiniciar nada): prévia em itálico sob o título de cada thread (lista e cabeçalho), gerada em segundo plano pelo orquestrador (`maybe_summarize_thread`) com a chave da OpenAI do módulo de voz; custo `thread_summary` em `ai_costs`.

| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | true |
| A cada quantos turnos (`every_turns`) | int 1–50 (também o tamanho da janela resumida) | 3 |
| Modelo (`model`) | string (chat completions) | `gpt-4o-mini` |
| Prompt do resumo (`prompt`) | textarea (prompt de sistema; Restaurar padrão) | `default_thread_summary_prompt()` |
| Tamanho máximo (`max_chars`) | int 40–2000 | 280 |

Ações: **Adicionar token** (abre instrução `claude setup-token`), **Testar**, **Tornar ativo** (testa → aplica → derruba processos vivos → mostra quantas threads voltarão com `--resume`), **Remover**.

### Base de Conhecimento
| Campo | Tipo |
|---|---|
| Pasta | path (diálogo nativo de pasta, `rfd`) |
| Status | existe / gravável / tem `CLAUDE.md` / nuvem sincronizada / `node` e `python` detectados |
| Ações | Abrir no explorador · Criar estrutura inicial · Recarregar comandos (`.claude/commands`) |

### Voz (Realtime)
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| OpenAI API key | secret | — |
| Modelo Realtime | string | `gpt-realtime-mini` |
| Voz | enum | `alloy` |
| Modo de ativação | `toggle` \| `push_to_talk` | `toggle` |
| Atalho global | string em formato próprio (modificadores + tecla, gravador de tecla) | `Ctrl+Shift+Space` (`Cmd` no macOS) |
| Detecção de turno | `server_vad` \| `semantic_vad` | `server_vad` |
| Dispositivo de entrada / saída | enum (dispositivos enumerados pelo `cpal`) | padrão do sistema |
| Instruções da persona | textarea | modelo padrão |
| Transcrever e salvar na thread | bool | true |
| Limite de caracteres falados do cérebro | int | 1500 |
| Limite de minutos/dia | int opcional | vazio |
| Sessões simultâneas (`max_sessions`) | int ≥ 1 | 3 |

Subseção **Ouvir resumo (TTS)** (`realtime.summary`, lida a cada clique, sem reiniciar o módulo): o botão **ouvir** nas respostas da thread (`docs/07`).

| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | true |
| Modelo do resumo (`model`) | string (chat completions) | `gpt-4o-mini` |
| Prompt do resumo (`prompt`) | textarea (prompt de sistema; botão Restaurar padrão) | `default_speak_summary_prompt()` |
| Modelo de voz (`tts_model`) | string | `gpt-4o-mini-tts` |
| Voz da leitura (`voice`) | enum; vazio = mesma da sessão de voz | vazio |
| Velocidade (`speed`) | 0,5–2,0 | 1,0 |

Painel de sessão na mesma seção: **Iniciar/Parar** (a sessão é a do usuário logado), botão **segurar para falar** (PTT), estado (ouvindo / pensando / falando), nível do microfone e transcrições recentes. O indicador flutuante (`docs/07`) acompanha a sessão. Abaixo, **Sessões no servidor** (`GET /api/voice/status`: `sessions[] {user, active, state}`, `active_sessions / max_sessions`): quem está em sessão de voz agora; quando o teto está cheio, o botão Iniciar fica desabilitado com o motivo ("limite de N sessões atingido"). Dispositivos, atalho e PTT são do app; chave, modelo, persona, preços e teto são do servidor (admin).

### Telegram
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Token do bot | secret | — |
| Chats permitidos | lista de ids (int64) | — |
| Usuários permitidos | lista de ids (int64) | — |
| Destino padrão (chat/tópico) | par opcional | primeiro chat |
| Anunciar no boot | bool | true |
| Não autorizados | `ignorar` \| `responder_aviso` | `ignorar` |
| Formato da resposta final | `texto` \| `html` | `texto` |
| Resumo em áudio | bool + mín. chars + voz + velocidade | false / 120 / `alloy` / 1.5 |
| Transcrever voice notes | bool | true (se houver OpenAI key) |
| Pasta de anexos | path relativa à KB | `inbox/telegram` |

Ações: **Testar bot** (`getMe`), **Descobrir IDs** (instrução `/start`).

### MCP
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | true |
| Porta | int (0 = aleatória) | 0 |
| Ferramentas | toggles por ferramenta | `telegram_send_message` on, `notify_user` on, demais off |

### WhatsApp
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Contas (`accounts`) | lista `{ id, label, phone, enabled, created_at }` mantida pelo serviço; cada uma com estado desconectado / aguardando QR ou código / conectado (número, nome) / degradado | `[]` |
| Ações por conta | Parear por QR · Parear por código · Reconectar · Desvincular (opções: apagar índice, remover conta) · Renomear · Ativar/desativar | — |
| Adicionar conta | cria uma conta nova e inicia o pareamento (rótulo opcional; padrão "Conta N") | — |
| Retenção do índice | dias | 90 |
| Rate limit de envio | msgs / minutos | 20 / 10 |
| Permitir números não salvos | bool | false |
| Enviar recibo de leitura em `mark_read` | bool | false |

### Conta Google
| Campo | Tipo |
|---|---|
| `client_id` | texto (valida `*.apps.googleusercontent.com`) |
| `client_secret` | secret |
| Passo a passo | painel recolhível com os 5 passos (inclui "Publicar app") |
| Contas conectadas | lista: e-mail, escopos, status, [Reautorizar] [Desconectar] |
| Ação | Conectar conta Google |

### Agenda
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Agendas por conta | multi-check (nome, cor, papel) + botão Atualizar lista | nenhuma |
| Agenda padrão para criação | select | primeira com escrita |
| Permitir criar/editar | bool | false |
| Permitir excluir | bool | false |
| Enviar convites a participantes | bool | false |
| Fuso padrão | auto / manual | auto |
| Janela máxima por consulta | dias | 62 |

### Gmail
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Contas usadas | multi-check | todas |
| Permitir envio | bool | true (política confirmar) |
| Permitir modificar | bool | false |
| Só destinatários já vistos | bool | false |
| Rate limit de envio | por hora | 30 |
| Tamanho máx. de corpo | KB | 64 |
| Pasta de anexos / limite | path, MB | `inbox/gmail`, 25 |

### Agendamentos (`scheduler`)
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar (`enabled`) | bool | true |
| Notificar ao disparar (`notify_on_fire`) | bool (só threads UI/voz; Telegram responde no tópico) | true |
| Fuso padrão (`timezone`) | IANA opcional (vazio = sistema) | vazio |
| Máximo de agendamentos ativos (`max_schedules`) | int | 100 |
| Intervalo mínimo entre disparos (`min_interval_minutes`) | minutos (0 desliga; 5 rejeita `* * * * *`) | 5 |

Detalhes em [24](24-scheduler.md). A página **Agendamentos** (fora de Configurações) lista e edita os agendamentos.

### Rede (`network`, só admin; vale no próximo início do servidor)
| Campo | Tipo | Padrão |
|---|---|---|
| Endereço de escuta (`bind`) | `127.0.0.1` \| `0.0.0.0` \| IP de uma interface (ex.: IP do Tailscale) | `127.0.0.1` |
| TLS (`tls`) | bool | false |
| Certificado / chave (`cert_path`, `key_path`) | caminhos **no servidor** (PEM); vazios = autoassinado gerado em `<data_dir>/tls/` | vazio |
| Permitir remoto sem TLS (`allow_remote_without_tls`) | bool, com aviso vermelho ("só atrás de VPN/proxy") | false |

Cartão **Certificado em uso** (`GET /api/network/cert`): caminho, impressão digital SHA-256 e o PEM com botão **Copiar** (é o que se cola no perfil dos apps remotos); vazio quando o servidor está em HTTP. Cartão **Estado atual** (`GET /api/health`): esquema (`http`/`https`) e endereço efetivos; aviso quando o `bind` salvo é remoto mas o servidor está em `127.0.0.1` (regra do TLS, `docs/12` regra 16) ou quando TLS foi pedido e o servidor caiu para HTTP (ver `/api/logs`, módulo `network`). Salvar mostra "vale no próximo início do servidor"; `will-ia --bind` sobrepõe o campo. Ver `docs/13` "Servidor headless".

### Browser CDP (`browser`)
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar (`enabled`) | bool (desligado = sem `AGENT_BROWSER_CDP` nem allowlist no `claude`) | true |
| Porta do proxy (`listen_port`) | u16, loopback; é o valor de `AGENT_BROWSER_CDP` | 9223 |
| Navegador real (`upstream_host`, `upstream_port`) | host + porta do CDP; lançamento automático só para loopback | `127.0.0.1` / 9222 |
| Lançar navegador automaticamente (`auto_launch`) | bool | true |
| Executável do navegador (`browser_path`) | path opcional; select entre os detectados (`/api/browser/detect`) ou caminho manual; vazio = detectar (Chrome, Chromium, Brave, Edge, Chromium do agent-browser) | vazio |
| Headless (`headless`) | bool (`--headless=new`) | false |
| Perfil (`user_data_dir`) | path opcional; vazio = `<data_dir>/browser-profile` (Chrome 136+ exige perfil dedicado) | vazio |
| Argumentos extras (`extra_args`) | lista de strings | `[]` |
| Espera após lançar (`launch_wait_secs`) | segundos (mín. efetivo 3) | 20 |
| Capacidade do log de tráfego (`traffic_capacity`) | mensagens em memória (mín. 10) | 500 |
| Fechar navegador ao parar (`close_browser_on_stop`) | bool; só o navegador lançado pelo serviço | false |
| Liberar `Bash(agent-browser:*)` (`allow_agent_browser_tool`) | bool (allowlist do modo `acceptEdits`) | true |

Detalhes em [25](25-browser-cdp.md). A página **Browser CDP** (fora de Configurações) mostra estado, tráfego ao vivo, testes e instalação do agent-browser/Chromium/skill.

### MCPs externos
| Campo | Tipo |
|---|---|
| Lista de servidores | nome, tipo (`stdio`/`http`/`sse`), status do teste, habilitado, [Editar] [Testar] [Remover] |
| Formulário | nome, `command`/`args`/`env`/`cwd` ou `url`/`headers`; cada valor de env/header com flag **segredo** |
| Liberar ferramentas automaticamente | bool por servidor (+ lista opcional de ferramentas nomeadas) |
| Ações | Adicionar servidor · Importar do Claude Code · Colar JSON · **Ver `.mcp.json` gerado** (somente leitura) |
| Herdar MCPs globais do Claude Code | bool (desliga `--strict-mcp-config`) |

### MCP (complemento)
Tabela de ferramentas agrupada por família com coluna **Política** (`auto` / `confirmar` / `desligada`) e timeout de aprovação (min, padrão 5). Painel **Aprovações pendentes** com ✅/❌ e detalhe dos argumentos.

### Dashboard
Status dos módulos, base de conhecimento, motor (processos vivos, token ativo) e aprovações pendentes.

### Threads
Lista com origem, título, status (🟢🟡⚪❌), última atividade, custo acumulado. Ações por thread: abrir, renomear, reiniciar, zerar, interromper, excluir (com opção de apagar transcrição). Chat com streaming e **eco otimista**: ao enviar (Enviar, soltar "Falar" ou anexar+enviar) a mensagem entra na conversa na hora, esmaecida e com "enviando…", acima do cartão de processamento; quando o servidor confirma (`turn_id` do `POST …/messages`, ou `TurnStarted`/`MessageAccepted` recarregam a lista) o eco é trocado pelo turno real sem duplicar (por id, por lista mais nova que a confirmação ou por texto+hora), e a resposta chega embaixo como antes; se o envio falha, o balão fica com o motivo e os botões "Reenviar" (devolve texto e anexos à caixa) e "Descartar". O **rascunho é por thread** (texto e anexos pendentes acompanham a thread aberta; ao trocar de thread o que estava escrito fica guardado e volta quando ela é reaberta) e todo envio é roteado pela **thread de origem**: o "Falar" lembra em que thread foi pressionado, então se o usuário abrir outra thread enquanto o áudio é transcrito, a fala vai para a thread original (junto com o rascunho dela), com o eco e a confirmação do POST reconciliando lá, sem tocar na caixa da thread aberta. Envio mid-turn: as mensagens enviadas durante um turno aparecem agrupadas ao balão inicial (cantos colados, hora só no último) e não geram "resposta" própria; o servidor grava o turno absorvido com `error_kind = "merged"` e o id do turno pai, e a API expõe `merged_into`. Anexos enviados pelo canal (foto/arquivo do Telegram) aparecem no balão do usuário: miniatura clicável para imagens (JPEG/PNG/WebP/GIF, via `egui_extras` com `file://`), chip "anexo: nome · tamanho" para os demais, clique abre no app padrão; a nota textual que o canal acrescenta ao prompt para o cérebro ("[O usuário enviou um arquivo…]") fica oculta no chat. Na própria tela dá para anexar: botão "Anexar" (diálogo nativo, vários arquivos) ou arrastar arquivos para a janela; os pendentes aparecem como chips removíveis acima da caixa e, ao enviar, o serviço copia cada um para `<kb>/inbox/ui/` (mesmo fluxo do Telegram; dá para mandar só anexos, sem texto). Sob o título, na lista (mesma linha do tempo relativo, truncada com reticências; inteira no tooltip) e no cabeçalho da conversa (ao lado de origem · status · custo; inteira no hover), uma linha em itálico com o **resumo automático** da conversa (`threads.summary`, gerado a cada N turnos — ver "Resumo das threads" acima). Cada balão traz um rodapé discreto: tempo relativo ("há 5 min"; o hover mostra a data exata), botão "copiar" e, na resposta, um ícone "i" que abre um popover com modelo, duração, custo, início/fim, sessão e id do turno. No balão do usuário aparece o **autor** (`turns.author`: nome do usuário da sessão; "local" para o token da API; omitido quando `null`, como nos turnos vindos do Telegram, voz ou agendamentos, que já trazem a origem no cabeçalho), já que as threads são compartilhadas por todos os usuários do servidor.

### Aprovações
Pendentes como cartões (ferramenta, contagem regressiva, resumo gerado pelo serviço, quem pediu, argumentos em chave/valor com rótulos legíveis e JSON completo em "Detalhes", botões Aprovar/Negar) e histórico em tabela com filtros por texto e decisão (quando, ferramenta, resumo, decisão, por onde/quem decidiu: `decided_via` é `ui:<username>` quando decidido no app, `telegram`/`notification` nos outros canais). Membros também aprovam: as aprovações chegam a todos os apps conectados.

### Agendamentos (página)
Lista (nome, gatilho + descrição do cron, fuso, próximo disparo e "em quanto tempo", thread dona com aviso se não existe mais, último resultado, contagem, estado ativo/pausado com motivo, criado por agente/usuário) e ações: editar, pausar/retomar, **Disparar agora**, excluir, histórico de disparos. Formulário com prévia da expressão cron (`/api/schedules/preview`: descrição, fuso, próximas ocorrências, erro de intervalo mínimo) ou data/hora única, fuso e thread dona. Ver [24](24-scheduler.md) (página `pages/schedules.rs`; seção `pages/settings/scheduler.rs`).

### Browser CDP (página)
Sem desktop no servidor, um aviso fixo "servidor sem tela: navegador sempre headless" e o toggle `headless` fica desabilitado. Estado do proxy (`cdp_env` com Copiar, upstream, navegador detectado/lançado, último erro), botões **Lançar navegador** / **Fechar navegador** e testes (`/json/version` no navegador, abrir aba pelo proxy, agent-browser de ponta a ponta); **Tráfego** CDP ao vivo (hora, conexão, sentido, tipo, método, id, bytes, prévia; filtro, contadores, limpar, pausar); **Instalação** (status de Node/npm/agent-browser/Chromium/skill, botões de instalar/atualizar o CLI, baixar Chromium e instalar a skill na KB, saída do trabalho em segundo plano). Ver [25](25-browser-cdp.md).

### Conexão (página, `pages/connect.rs`)
A que servidor o app está ligado, **quem está logado** e os **perfis** salvos (`Nav::Connect`; é a página inicial quando não há servidor alcançável ou o perfil não tem sessão válida). Cartões:

- **Servidor atual**: bolinha verde/amarela, nome, URL, "conectado"/"sem conexão", último erro, e o usuário da sessão (`GET /api/auth/me`: nome, `@username`, papel `admin`/`membro`; "Token local (admin)" quando o perfil usa o token da API). Botões **Sair** (`POST /api/auth/logout`, limpa o token do perfil e volta ao formulário) e **Trocar senha** (modal: senha atual + nova + confirmação → `POST /api/auth/password`; mínimo 8 caracteres).
- **Perfis**: "Esta máquina" (descoberto a cada atualização pelo `api-port` + `ui-session.token` do `data_dir` do servidor local; entra como admin implícito, sem login) e os perfis salvos (nome, URL, usuário; Conectar / Editar / Remover).
- **Certificado do servidor** (perfis `https://`): campo **CA/certificado (PEM)** (`ca_pem`, colar o PEM que o admin copiou de Configurações → Rede; o app mostra a impressão digital SHA-256 calculada, `will-client::pem_fingerprint`, para conferir com a de `GET /api/health`) e a opção **Aceitar qualquer certificado (inseguro, só testes)** (`insecure`, com aviso vermelho). Erro de TLS na conexão ("certificado não confiável") aponta para este campo. Em "Servidor atual" aparece o esquema e, com TLS, a impressão digital do certificado recebido e se ela bate com a fixada.
- **Novo perfil / Editar perfil**: nome, URL `http://host:porta` ou `https://…`, **usuário (ou e-mail) + senha**; ao salvar o app faz `POST /api/auth/login` (`client` = `will-ia-app/<versão> <SO>`), guarda **só o token de sessão** no perfil (nunca a senha) e conecta. Erros: 401 "usuário ou senha inválidos", 429 "muitas tentativas; aguarde", sem rota (servidor antigo). Campo alternativo "Token da API" (avançado) para colar o token local de um servidor a que se tem acesso ao disco.
- **Troca de senha obrigatória**: se o login (ou `GET /api/auth/me`) devolve `must_change_password = true`, o app abre um modal bloqueante "Defina sua nova senha" (sem pedir a atual) antes de liberar as outras páginas; é o caso do `admin` criado na primeira subida (senha em `<data_dir>/admin-initial-password.txt` / `will-ia info`).
- Sessão inválida/expirada (401 em qualquer chamada ou na reconexão do WS): o app volta para esta página com o perfil selecionado e o aviso "sessão expirada; entre de novo".

Conectar troca o servidor **sem reiniciar o app**: fecha o WS, limpa o snapshot e reconecta; o perfil escolhido vira o `last` de `profiles.json` (`<config>/will-ia-app/profiles.json`, 0600; ver "Perfis do app" em `docs/26`).

### Usuários (página, só admin; `pages/users.rs`)
Aparece na navegação só quando o `Principal` atual é admin (membros recebem 403 nas rotas e o item some). Tabela de `GET /api/users`: usuário, nome, e-mail, papel, estado (ativo/desativado), último login, "troca de senha pendente". Ações: **Novo usuário** (usuário `[a-z0-9._-]`, nome, e-mail opcional, papel, senha inicial ≥ 8 — gerada aleatoriamente por padrão, com Copiar — e "obrigar troca no primeiro login", ligado por padrão → `POST /api/users`); **Editar** (nome, e-mail, papel → `PATCH`); **Redefinir senha** (`PATCH` com `password`: força troca no próximo login e derruba as sessões); **Desativar / Reativar** (`PATCH active`; `DELETE` também só desativa; a própria conta não pode ser desativada nem rebaixada, o botão fica desabilitado com tooltip); **Sessões** (modal com `GET /api/users/{id}/sessions`: criada em, expira em, último uso, cliente). Usuários desativados ficam listados, em cinza, porque a autoria histórica aponta para eles.

### Navegação por papel
O menu **Configurações** (todas as seções desta página) e **Usuários** só aparecem para `admin`; para membros ficam Dashboard, Threads, Agendamentos, Browser CDP (sem o cartão Instalação), Aprovações, Logs, Custos e Conexão. O app decide pelo `role` de `GET /api/auth/me` (snapshot) e, por garantia, trata qualquer 403 como "só administradores" num toast. "Esta máquina" com o token local é sempre admin.

### Logs
Visor estilo terminal (monoespaçado, uma linha por registro: hora · nível · módulo · mensagem, mesmas cores nos dois temas) com histórico do banco + cauda ao vivo; filtro de texto com destaque, nível mínimo, módulo, seguir, quebrar linhas, data, pausar ao vivo, copiar visíveis, limpar tela e abrir a pasta de logs.

## Esquema (Rust)

```rust
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub schema_version: u32,
    pub general: General,
    pub engine: EngineSettings,
    pub kb: KbSettings,
    pub realtime: RealtimeSettings,
    pub telegram: TelegramSettings,
    pub mcp: McpSettings,
    pub whatsapp: WhatsAppSettings,
    pub google: GoogleSettings,      // client + contas (refs)
    pub calendar: CalendarSettings,
    pub gmail: GmailSettings,
    pub scheduler: SchedulerSettings,
    pub browser: BrowserSettings,     // proxy CDP para o agent-browser (docs/25)
    pub external_mcps: Vec<ExternalMcpServer>,
}
```

- Persistido na tabela `settings` (uma linha JSON + `schema_version`) — mais simples de migrar que colunas.
- Validação com `validator`/manual antes de salvar; erros retornados por campo para a UI.
- Migrações de schema em código (`migrate(v_from) -> Settings`).

## Aplicação a quente

`SettingsChanged { section }` → Orchestrator decide: `general` (sem reinício), `engine` (aplica nos próximos spawns; troca de token derruba vivos), `kb` (derruba todos os processos; nova pasta vale no próximo spawn), `realtime`/`telegram`/`mcp`/`whatsapp`/`google` (stop + start do módulo); `scheduler` (`update_config`: liga/desliga o laço e reaplica limites/fuso sem reiniciar); `browser` (`update_config` reinicia o listener só se porta/`enabled` mudaram; `refresh_engine` para que `AGENT_BROWSER_CDP`/allowlist valham no próximo spawn; mudar `kb` atualiza a pasta da skill); mudanças em `calendar`/`gmail` que exigem escopo novo disparam reautorização incremental.

## Onboarding (primeira execução)

Assistente em 3 passos: (1) token do Claude + teste; (2) pasta da KB + criar estrutura; (3) opcional: Telegram e Voz. Ao final, abre uma thread de exemplo na UI.

## Interface

egui/eframe em `crates/will-app` (ADR-0016, ADR-0019). Modo imediato: cada página lê um snapshot montado por `GET /api/status`, `/api/settings` e `/api/approvals` e mantido pelo WS `/api/events` (reconexão com backoff; banner "desconectado"), e dispara ações no runtime tokio via `will-client`; respostas chegam por canais e `request_repaint` dirigido pelos eventos. Formulários validam espelhando o backend (erros por campo). Sem lógica de negócio na UI. Diálogos nativos só para o que é do app: escolher arquivos a anexar (`rfd`; em loopback também a pasta da KB), QR do WhatsApp desenhado na própria janela. Com servidor remoto, a pasta da KB é escolhida por um modal que navega o disco do servidor (`GET /api/kb/browse`), anexos sobem por `POST /api/uploads` e miniaturas vêm de `GET /api/files`; "Abrir pasta" só aparece quando o servidor é esta máquina.

### Abas nas páginas longas

Página com **duas ou mais seções independentes** não empilha cartões numa rolagem comprida: logo abaixo do título entra uma **barra de abas** (`ui::tabs(ui, "<pagina>", &[ui::Tab::new("<chave>", tr("Rótulo")).tone(..)])`) e só a seção da aba ativa é desenhada. A chave da aba é estável e não traduzida (é o que o deep-link usa); o rótulo passa por `tr`; a bolinha opcional (`tone`) reaproveita o estado do módulo. A aba escolhida fica na memória do egui por página (`ui::selected_tab`/`ui::select_tab`), então trocar de página e voltar reabre a mesma aba; as edições não salvas continuam no rascunho da seção (`SettingsState.draft`), e a barra **Salvar/Descartar** aparece nas abas que editam configuração (a verificação de "alterado" é por seção, não por aba). `ui::tab_bar` é a variante sem estado (a tela de Conexão a usa com o próprio `LandingTab`). Em janela estreita as abas quebram linha.

| Página | `pagina` | Abas (`chave` = rótulo) |
|---|---|---|
| Configurações › Motor Claude | `engine` | `tokens` Tokens · `execution` Execução · `summary` Resumo das threads |
| Configurações › Base de conhecimento | `kb` | `folder` Pasta · `status` Status (inclui o `.mcp.json` gerado) |
| Configurações › Voz | `voice` | `session` Sessão · `config` Configuração · `tts` Ouvir resumo (TTS) · `pricing` Preços |
| Configurações › Telegram | `telegram` | `bot` Bot · `config` Configuração |
| Configurações › Servidor MCP | `mcp` | `server` Servidor · `tools` Ferramentas e políticas · `calls` Chamadas recentes |
| Configurações › WhatsApp | `whatsapp` | `accounts` Contas · `config` Configuração |
| Configurações › Google | `google` | `account` Conta · `calendar` Agenda · `gmail` Gmail (· `drive` Drive) — cada aba com a bolinha do módulo |
| Configurações › Rede | `network` | `listen` Escuta · `cert` Certificado em uso |
| Browser CDP (página) | `browser` | `status` Status · `install` Instalação · `tests` Testes · `traffic` Tráfego CDP (ao vivo) |

Ficam sem abas as páginas curtas de uma seção (Geral, Agendamentos, Browser CDP em Configurações) e as de lista + formulário (MCPs externos, Agendamentos, Usuários, Aprovações, Custos, Início).

**Deep-link.** `will-ia screenshot --nav pagina/aba` (ex.: `google/gmail`, `browser/traffic`) abre a página já na aba; dentro do app, quem navega para uma aba específica chama `ui::select_tab(ctx, "kb", "status")` antes de `*cx.go = Some(Nav::Settings(..))` (é o que "Ver .mcp.json" em MCPs externos faz).

## Ajuda, idioma e estado dos módulos

**Ajuda "?".** Títulos de página (`ui::heading_help`) e cartões de seção (`ui::section_help`) podem levar um ícone "?" que abre um modal com introdução, passo a passo numerado, bloco "No terminal" (com Copiar), notas (dúvidas e erros comuns) e links. O conteúdo é estático em `crates/will-app/src/help.rs` (uma constante `Help` por assunto: tokens e execução do motor, KB, Telegram, voz, WhatsApp, servidor MCP, políticas, MCPs externos, Google). Nada disso ocupa espaço na página fechada.

**Idioma (i18n).** Sem crates externas: os textos da UI ficam em pt-BR no código e são a própria chave. `i18n::tr("Salvar")` devolve o texto no idioma corrente; `i18n::trf("Criados: {}", &[&n])` preenche marcadores `{}` em ordem. A tabela `i18n::TABLE` guarda os pares `(pt-BR, en)`; chave ausente devolve o pt-BR. O idioma vem de Geral › Idioma (`settings.general.language`) e o app chama `set_language` a cada quadro a partir do snapshot, então salvar a seção troca a UI inteira no quadro seguinte (exceções: o menu da bandeja em Windows/macOS é construído na inicialização e só atualiza o item de voz). Testes garantem chaves únicas e mesma quantidade de `{}` nos dois idiomas. Não se traduzem ids, nomes de nível de log, URLs, modelos e textos técnicos copiáveis (ex.: `export …`).

**Estado dos módulos** (`will_core::ModuleStatus`, bolinhas do Início e do menu lateral; rótulo em `ui::module_label`):

| Cor | Estado | Significado |
|---|---|---|
| verde | `Running` → "pronto" | habilitado, configurado e operacional, mesmo ocioso (motor: token ativo + KB ok; voz: habilitada + chave; Google: conta conectada; Telegram: polling no ar; WhatsApp: conta conectada; MCP: ouvindo; Browser CDP: proxy escutando, mesmo sem navegador aberto). O Início acrescenta detalhes ("N processo(s) vivo(s)", "em sessão", "N conta(s)") |
| azul | `Starting` → "iniciando" | transição |
| amarelo | `Degraded(motivo)` → "atenção · motivo" | habilitado, mas falta algo ou algo falha; o motivo é sempre mostrado |
| vermelho | `Error(motivo)` → "erro · motivo" | falhou ao subir ou erro fatal |
| cinza | `Disabled` → "desligado" / `Stopped` → "parado" | desligado nas configurações / parado explicitamente. "Ocioso mas pronto" **não** é cinza: é verde |

### Ditado no campo de mensagem (Threads)

Com o módulo Voz pronto (habilitado + chave da OpenAI), o campo de mensagem ganha o botão **Falar** ao lado de Anexar/Enviar. Segure com o mouse para gravar (o botão fica vermelho com o contador de segundos); ao soltar, o áudio vai ao servidor (`POST /api/voice/transcribe`, Whisper), o texto entra no campo (depois do que já estava digitado) e a mensagem é **enviada na hora** para a thread (desde 2026-10-29: "falar e enviar", sem o segundo clique). Se um envio anterior ainda estiver em andamento, o texto fica no campo para enviar manualmente. Gravação muito curta ou áudio sem fala viram um aviso.
