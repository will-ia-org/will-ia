# 02 — Requisitos Funcionais

Convenção de identificação: `RF-<módulo>-<nº>`. Prioridade: **M** (Must, MVP), **S** (Should), **C** (Could).

## RF-APP — Aplicação de bandeja

| ID | Requisito | Prio |
|---|---|---|
| RF-APP-01 | O serviço deve iniciar como ícone na bandeja do sistema em Windows, Linux (StatusNotifierItem) e macOS (menu bar), sem janela principal aberta (exceto na primeira execução ou com `--show`). Fechar a janela apenas a esconde. **Não há "Sair"** na janela nem na bandeja: o serviço é residente e só termina por kill forçado (`kill -9`, Gerenciador de Tarefas); no modo GUI, SIGINT/SIGTERM/SIGHUP são ignorados com aviso no log (`docs/13`). | M |
| RF-APP-02 | O menu da bandeja deve oferecer: Abrir will-ia e Ligar/Desligar voz (sem "Sair"). | M |
| RF-APP-03 | O ícone deve refletir estado: ocioso, processando (alguma thread ativa), gravando voz, erro. | S |
| RF-APP-04 | Deve existir opção de iniciar com o sistema (autostart), configurável. | M |
| RF-APP-05 | Apenas uma instância do serviço pode rodar por diretório de dados: trinco `instance.lock` tomado pelo SO (`flock` / abertura exclusiva), solto automaticamente quando o processo morre. Uma segunda invocação pede à instância ativa que mostre a janela (`POST /api/ui/show`), insiste alguns segundos se ela ainda está subindo, e sai. | M |
| RF-APP-08 | Os processos filhos (`claude`) devem morrer junto com o serviço mesmo em kill forçado: `PR_SET_PDEATHSIG` no Linux, Job Object com `KILL_ON_JOB_CLOSE` no Windows. | M |
| RF-APP-06 | O serviço deve exibir notificações nativas em eventos relevantes (resposta pronta de thread em background, erro de autenticação, etc.), com opção de desligar. | S |
| RF-APP-07 | O mesmo binário deve rodar em modo headless (`will-ia serve [--port --data-dir --file-secrets]`): só serviço + API local, sem bandeja nem janela, para servidores, testes e systemd sem sessão gráfica. | S |

## RF-CFG — Configurações

| ID | Requisito | Prio |
|---|---|---|
| RF-CFG-01 | A janela principal (nativa, egui) deve ter navegação lateral com páginas Dashboard, Threads, Aprovações, Agendamentos, Logs e Configurações com seções: Geral, Motor Claude, Base de Conhecimento, Voz (Realtime), Telegram, MCP, MCPs externos, WhatsApp, Google (Conta, Agenda, Gmail), Agendamentos, Browser CDP. | M |
| RF-CFG-02 | **Geral:** autostart, idioma da UI, nível de log, notificações. | M |
| RF-CFG-03 | **Motor Claude:** lista de tokens de subscription (apelido + token), com exatamente um marcado como *ativo*; modelo padrão; modo de permissão (`acceptEdits` ou `bypassPermissions`); limite de processos simultâneos; orçamento máximo por turno (USD, opcional); timeout por turno. | M |
| RF-CFG-04 | **Base de Conhecimento:** caminho da pasta; validação de existência e permissão de escrita; botão para abrir no explorador de arquivos; botão para criar a estrutura inicial (`.claude/`, `tools/`, etc.) se não existir. | M |
| RF-CFG-05 | **Voz:** API key da OpenAI; habilitar/desabilitar; modelo Realtime; voz; modo de ativação (toggle por atalho ou push-to-talk); atalho global; dispositivos de entrada/saída de áudio (enumerados pelo `cpal`); instruções do sistema para a persona de voz; painel de sessão com iniciar/parar, botão segurar-para-falar e transcrições. | M |
| RF-CFG-06 | **Telegram:** token do bot; habilitar/desabilitar; ID do supergrupo; allowlist de user IDs; comportamento para não autorizados (ignorar / responder aviso); modo de entrega da resposta (final única / edições progressivas). | M |
| RF-CFG-07 | **MCP:** habilitar/desabilitar; porta (padrão aleatória livre, fixável); lista de ferramentas expostas com toggle e **política** (`auto` / `confirmar` / `desligada`) individual. | M |
| RF-CFG-11 | **WhatsApp:** várias contas; parear por QR (exibido na própria janela) ou por código; estado da sessão; reconectar; desvincular; retenção do índice; rate limit de envio; permitir números não salvos. Aviso de risco informativo, não bloqueante. | S |
| RF-CFG-12 | **Conta Google:** `client_id`/`client_secret` do OAuth Client do usuário com passo a passo; lista de contas conectadas; conectar/reautorizar/desconectar. | S |
| RF-CFG-13 | **Agenda:** multi-check de agendas por conta; agenda padrão para criação; permitir criar/editar; permitir excluir; fuso padrão. | S |
| RF-CFG-14 | **Gmail:** contas usadas; permitir envio (padrão ligado, política confirmar); permitir modificar (padrão desligado); limites de corpo e anexo; pasta de anexos. | S |
| RF-CFG-08 | Toda alteração deve ser validada antes de salvar; tokens são testados com um botão "Testar conexão". | S |
| RF-CFG-09 | Segredos (tokens, API keys) devem ser exibidos mascarados, com botão revelar. | M |
| RF-CFG-10 | Alterações em módulos (Telegram, Realtime, MCP) devem ser aplicadas a quente, reiniciando apenas o módulo afetado. | S |

## RF-THR — Threads (sessões do cérebro)

| ID | Requisito | Prio |
|---|---|---|
| RF-THR-01 | Uma thread é uma sessão persistente do Claude Code (`session_id`) executada sobre a pasta da base de conhecimento. | M |
| RF-THR-02 | Threads podem ser criadas por: UI de configurações, Telegram (novo tópico), Realtime (função `ask_brain` sem thread ativa) e pela API interna. | M |
| RF-THR-03 | Cada thread tem: id interno, origem (`ui`, `telegram`, `realtime`), título, `claude_session_id`, status (`idle`, `running`, `error`, `archived`), data de criação e última atividade. | M |
| RF-THR-04 | O usuário pode listar, renomear, arquivar e **excluir** threads pela UI. Excluir remove o mapeamento local; a sessão do Claude Code pode opcionalmente ser removida do disco. | M |
| RF-THR-05 | Cada thread é um **processo `claude -p` vivo** com stdin aberto. Mensagens recebidas enquanto a thread está processando são **entregues imediatamente ao processo** como contexto adicional (steering), não enfileiradas. O Claude Code decide como incorporá-las ao turno em andamento. | M |
| RF-THR-06 | O número global de processos `claude` vivos é limitado pela configuração (padrão: 5). Threads ociosas além de um tempo configurável (padrão: 10 min) têm o processo encerrado e são retomadas com `--resume` na próxima mensagem. Ao atingir o limite, a thread ociosa há mais tempo é suspensa primeiro. | M |
| RF-THR-07 | O usuário pode cancelar um turno em andamento (mata o processo com sinal gracioso, depois forçado). | M |
| RF-THR-08 | A UI deve exibir o histórico de turnos de uma thread (prompt, resposta final, custo, duração, ferramentas usadas) com streaming ao vivo do turno atual. | S |
| RF-THR-09 | Threads sem atividade por N dias (configurável) podem ser arquivadas automaticamente. | C |

## RF-ENG — Motor Claude Code

| ID | Requisito | Prio |
|---|---|---|
| RF-ENG-01 | O motor deve invocar `claude -p` com `--output-format stream-json` e `--input-format stream-json`, mantendo **stdin aberto durante toda a vida da thread**, com diretório de trabalho igual à pasta da base de conhecimento. | M |
| RF-ENG-02 | O token de subscription ativo deve ser injetado via variável de ambiente `CLAUDE_CODE_OAUTH_TOKEN` no processo filho, nunca gravado em arquivo. | M |
| RF-ENG-03 | A primeira execução usa `--session-id <uuid>` gerado pelo serviço. Se o processo for encerrado (ocioso, crash, reinício do serviço), a continuidade é feita com `--resume <claude_session_id>` na próxima mensagem. | M |
| RF-ENG-10 | O motor deve suportar **múltiplas mensagens de usuário no mesmo stdin**, inclusive enquanto um turno está em execução, e correlacionar cada `result` emitido com o turno lógico correspondente. `--replay-user-messages` deve ser usado para confirmar a aceitação de cada mensagem. | M |
| RF-ENG-04 | O motor deve passar `--mcp-config <kb>/.mcp.json` (arquivo gerado pelo serviço com `will_ia` + MCPs externos), com `--strict-mcp-config` opcional, injetando no ambiente o bearer efêmero da thread (`WILL_IA_MCP_TOKEN`) e os segredos dos externos (`WILL_MCP_*`). | M |
| RF-ENG-05 | O motor deve aplicar o modo de permissão configurado (`--permission-mode acceptEdits` + `--allowedTools` ou `--dangerously-skip-permissions`). | M |
| RF-ENG-06 | O motor deve fazer parse incremental do stream JSON e emitir eventos tipados internos (início, texto parcial, uso de ferramenta, resultado, erro, custo). | M |
| RF-ENG-07 | Erros de autenticação, rate limit e saída anormal do processo devem ser classificados e reportados com mensagem acionável na UI e no canal de origem. | M |
| RF-ENG-08 | Deve ser possível passar `--max-budget-usd` e um timeout de parede por turno. | S |
| RF-ENG-09 | O motor deve detectar a ausência do binário `claude` no PATH e orientar a instalação. Caminho do binário deve ser configurável. | M |

## RF-KB — Base de Conhecimento

| ID | Requisito | Prio |
|---|---|---|
| RF-KB-01 | A pasta da base de conhecimento é escolhida pelo usuário e é o `cwd` de todo processo `claude` spawnado. | M |
| RF-KB-02 | O serviço deve oferecer a criação de um esqueleto inicial: `CLAUDE.md`, `.claude/settings.json`, `.claude/agents/`, `tools/`, `knowledge/`, `inbox/`. | M |
| RF-KB-03 | O serviço **não** versiona a pasta com git nem toca em seu conteúdo além do esqueleto inicial. | M |
| RF-KB-04 | A pasta `tools/` é reservada para scripts interpretados (Node.js, Python) criados pelos agentes ou pelo usuário; o serviço deve liberar, via `--allowedTools`, a execução de `Bash(node tools/*)` e `Bash(python tools/*)` (ou equivalentes por SO). | M |
| RF-KB-05 | O serviço deve exibir um aviso se a pasta estiver em um local sincronizado por nuvem com bloqueio de arquivos (OneDrive, iCloud) ou se não tiver permissão de escrita. | C |

## RF-RT — Voz (OpenAI Realtime)

| ID | Requisito | Prio |
|---|---|---|
| RF-RT-01 | Ao pressionar o atalho global, o serviço deve abrir uma sessão WebSocket com a Realtime API, capturar áudio do microfone e reproduzir a resposta pelo áudio do próprio app (`cpal`, PCM16 24 kHz). | M |
| RF-RT-02 | Dois modos de ativação: **toggle** (atalho liga/desliga a sessão; detecção de voz do servidor controla turnos) e **push-to-talk** (áudio só é enviado enquanto o atalho está pressionado). | M |
| RF-RT-03 | A sessão Realtime deve registrar a função `ask_brain(prompt, thread_hint?)`. Quando o modelo a invoca, o serviço roteia o prompt para uma thread do Claude Code e executa em paralelo. | M |
| RF-RT-04 | Enquanto o cérebro processa, a voz continua disponível. Se o usuário acrescentar informações relevantes, o modelo Realtime pode chamar `ask_brain` novamente e o texto é **entregue à thread em andamento** como contexto adicional. Ao concluir, o resultado é injetado como `function_call_output` e uma nova resposta é solicitada para que a voz comunique o resultado. | M |
| RF-RT-05 | Deve haver uma thread padrão "voz" reutilizada entre sessões; o usuário pode pedir por voz para iniciar uma nova thread. | S |
| RF-RT-06 | O ícone da bandeja deve mostrar quando o microfone está ativo. | M |
| RF-RT-07 | Transcrições (entrada e saída) devem ser salvas no histórico da thread de voz. | S |
| RF-RT-08 | Interrupção (barge-in): quando o usuário fala sobre a resposta, a reprodução deve ser cancelada e o buffer do servidor truncado. | S |
| RF-RT-09 | Enquanto a sessão de voz está ativa, um **indicador flutuante** (janela sem decoração, sempre no topo) deve mostrar o estado (ouvindo / pensando / falando) e o nível do microfone. | M |

## RF-TG — Telegram

| ID | Requisito | Prio |
|---|---|---|
| RF-TG-01 | O serviço deve conectar ao bot via long polling (sem necessidade de URL pública). | M |
| RF-TG-00 | O módulo Telegram deve ser modelado a partir do projeto de referência fornecido pelo usuário (a anexar em `docs/referencias/`), reaproveitando sua estrutura de tratamento de updates, tópicos e formatação. | M |
| RF-TG-02 | Apenas mensagens do supergrupo configurado e de usuários na allowlist são processadas. Demais mensagens são ignoradas ou respondidas com aviso, conforme configuração. | M |
| RF-TG-03 | Cada tópico do fórum (`message_thread_id`) mapeia para exatamente uma thread do Claude Code. Primeira mensagem em um tópico novo cria a thread. | M |
| RF-TG-04 | Mensagens no "General" (sem tópico) mapeiam para uma thread padrão do chat. | S |
| RF-TG-05 | Comandos suportados: `/new` (reinicia a thread do tópico com nova sessão), `/status`, `/stop` (cancela turno em andamento), `/help`. | M |
| RF-TG-10 | Mensagens enviadas ao tópico enquanto a thread está processando são encaminhadas **imediatamente** ao processo `claude` vivo como contexto adicional, sem esperar o fim do turno. O bot reage à mensagem (👀) para confirmar o recebimento. | M |
| RF-TG-06 | Enquanto processa, o bot envia ação "digitando". A resposta final é enviada ao tópico de origem, dividida em partes de até 4096 caracteres, com formatação Markdown convertida para HTML do Telegram. | M |
| RF-TG-07 | Mensagens de texto e legendas de mídia são suportadas; anexos (documentos, imagens) são baixados para a pasta `inbox/` da base de conhecimento e referenciados no prompt. | S |
| RF-TG-08 | Mensagens de voz são transcritas (via OpenAI) e tratadas como texto. | C |
| RF-TG-09 | Fechamento/exclusão de tópico no Telegram arquiva a thread correspondente. | C |

## RF-MCP — Servidor MCP

| ID | Requisito | Prio |
|---|---|---|
| RF-MCP-01 | O serviço deve expor um servidor MCP com transporte Streamable HTTP em `127.0.0.1:<porta>/mcp`. | M |
| RF-MCP-02 | Toda requisição deve carregar `Authorization: Bearer <token>`, onde o token é emitido por spawn e identifica a thread chamadora. Tokens expiram quando o processo encerra. | M |
| RF-MCP-03 | Ferramenta `telegram_send_message(text, chat_id?, topic_id?)`: envia mensagem pelo bot. Sem `chat_id`/`topic_id`, usa o tópico de origem da thread (se originada no Telegram) ou o tópico padrão configurado. | M |
| RF-MCP-04 | Ferramenta `notify_user(title, body)`: notificação nativa no desktop. | S |
| RF-MCP-05 | Ferramentas `list_threads()` e `create_thread(title, initial_prompt)`: permitem a um agente delegar trabalho a outra thread. | C |
| RF-MCP-06 | Cada ferramenta pode ser habilitada/desabilitada individualmente nas configurações; ferramentas desabilitadas não aparecem na listagem `tools/list`. | M |
| RF-MCP-07 | Todas as chamadas de ferramenta são registradas (thread, ferramenta, argumentos resumidos, resultado, timestamp) e visíveis na seção Logs. | M |
| RF-MCP-08 | Cada ferramenta tem política `auto` / `confirmar` / `desligada`. Em `confirmar`, a chamada aguarda aprovação do dono (UI, notificação e, se ativo, Telegram com botões) com timeout configurável (padrão 5 min → negada). Decisões são registradas. | M |
| RF-MCP-09 | Ferramentas de efeito externo (`*_send*`, `calendar_create/update/delete_event`, `gmail_modify`) nascem com política `confirmar`. | M |
| RF-MCPX-01 | Seção **MCPs externos**: adicionar/editar/remover/habilitar servidores dos tipos `stdio`, `http` e `sse`, com nome único, comando/args/env ou url/headers. | M |
| RF-MCPX-02 | Valores de `env`/`headers` podem ser marcados como segredo: ficam no keyring e aparecem no arquivo como `${WILL_MCP_<NOME>_<CHAVE>}`; o serviço injeta as variáveis no ambiente de cada processo `claude`. | M |
| RF-MCPX-03 | Botão **Testar** executa `initialize` + `tools/list` contra o servidor e exibe ferramentas e erros. | S |
| RF-MCPX-04 | O serviço gera e mantém `<kb>/.mcp.json` com a **união** do servidor interno `will_ia` e dos externos habilitados, regravando na inicialização e a cada mudança; passa o arquivo em `--mcp-config` e grava `enableAllProjectMcpServers` em `<kb>/.claude/settings.local.json`. | M |
| RF-MCPX-05 | Edições manuais do `.mcp.json` são detectadas e importadas para o catálogo (segredos literais migram para o keyring), nunca sobrescritas em silêncio. | S |
| RF-MCPX-06 | Botões **Importar do Claude Code** (`~/.claude.json`) e **Colar JSON** (bloco `mcpServers`). | S |
| RF-MCPX-07 | Por servidor externo: toggle "liberar ferramentas automaticamente" que adiciona `mcp__<nome>__*` à allowlist do modo `acceptEdits`, com opção de liberar apenas ferramentas nomeadas. | M |
| RF-MCP-10 | O servidor expõe as famílias `telegram_*`, `whatsapp_*`, `calendar_*`, `gmail_*`, `schedule_*` e `notify_user`, cada família visível apenas se o módulo correspondente estiver ativo e autorizado. | M |

## RF-WA — WhatsApp (ferramentas; não é canal de entrada)

| ID | Requisito | Prio |
|---|---|---|
| RF-WA-01 | O usuário pareia o **seu** WhatsApp ao serviço pela UI, por QR code (mostrado na própria janela) ou código de pareamento; o aviso de risco de suspensão é informativo. A sessão persiste entre reinícios. | S |
| RF-WA-02 | Mensagens recebidas **não** criam threads nem acionam o cérebro. O WhatsApp é acessado apenas por ferramentas MCP chamadas por agentes já em execução. | S |
| RF-WA-03 | O módulo mantém um índice local (chats, mensagens, contatos, grupos, não lidas) alimentado pelos eventos do protocolo, com busca de texto e retenção configurável. | S |
| RF-WA-04 | Ferramentas de leitura: listar chats (todas/não lidas/grupos/arquivadas), obter mensagens de um chat, buscar, listar contatos, detalhes de grupo, baixar mídia para `inbox/whatsapp/`, marcar como lida. | S |
| RF-WA-05 | Ferramentas de envio: texto e mídia para chats/contatos existentes; números não salvos só com toggle. Política `confirmar` e rate limit por padrão. | S |
| RF-WA-06 | Desvincular faz logout remoto e apaga a sessão e, opcionalmente, o índice local. | S |
| RF-WA-07 | Várias contas WhatsApp podem ser pareadas ao mesmo tempo; cada ferramenta `whatsapp_*` identifica a conta e a UI permite parear, reconectar e desvincular cada uma. | S |

## RF-GOOG — Conta Google

| ID | Requisito | Prio |
|---|---|---|
| RF-GOOG-01 | O usuário informa `client_id`/`client_secret` de um OAuth Client "App para computador" criado no próprio Google Cloud; a UI traz passo a passo e valida. | S |
| RF-GOOG-02 | Autorização por Authorization Code + PKCE com redirect em loopback e navegador do sistema; refresh token no keyring; renovação automática. | S |
| RF-GOOG-03 | Várias contas Google podem ser conectadas; cada uma mostra e-mail, escopos e status, com reautorizar/desconectar (revogação). | S |
| RF-GOOG-04 | Escopos solicitados incrementalmente conforme os recursos habilitados (ler agenda, escrever agenda, ler Gmail, enviar, modificar). | S |

## RF-CAL — Google Agenda

| ID | Requisito | Prio |
|---|---|---|
| RF-CAL-01 | Após conectar a conta, a UI lista as agendas e o usuário habilita por **multi-check** quais ficam visíveis ao agente, e qual é a padrão para criação. | S |
| RF-CAL-02 | Ferramentas: listar agendas habilitadas, listar eventos por janela/consulta, obter evento, consultar livre/ocupado. | S |
| RF-CAL-03 | Ferramentas de escrita (criar, atualizar, excluir), cada uma com toggle; política `confirmar`; excluir desligado por padrão. | S |
| RF-CAL-04 | Datas sempre com fuso explícito; criação detecta e informa conflitos sem bloquear. | S |

## RF-MAIL — Gmail

| ID | Requisito | Prio |
|---|---|---|
| RF-MAIL-01 | Leitura via Gmail API: labels (tags) com contagens, busca com sintaxe do Gmail, mensagem (resumo/texto/completo), thread, anexos para `inbox/gmail/`. | S |
| RF-MAIL-02 | Cada mensagem retornada traz `archived` (sem `INBOX`) e `unread` (com `UNREAD`) já derivados, além das labels. | S |
| RF-MAIL-03 | Envio habilitado (`gmail_send`, com resposta em thread e anexos) e rascunhos; envio com política `confirmar` e rate limit. | S |
| RF-MAIL-04 | Modificação (marcar lida, arquivar, rotular, lixeira) opcional, desligada por padrão, exige escopo adicional. | C |
| RF-MAIL-05 | Sem IMAP: apenas contas Gmail/Workspace. | — |

## RF-SCH — Agendamentos (scheduler)

| ID | Requisito | Prio |
|---|---|---|
| RF-SCH-01 | O agente (via `schedule_*`) e o usuário (UI/API) podem criar agendamentos **recorrentes** (cron de 5 campos, segundos opcionais à frente, fuso IANA) ou **únicos** (`run_at`), com nome (≤ 80) e mensagem (≤ 8000), ligados à thread que os criou. | S |
| RF-SCH-02 | No horário, o serviço entrega à thread dona a mensagem precedida de um cabeçalho gerado pelo serviço (`[Agendamento "…" disparado em …]`), **pelo stdin do processo na hora**, inclusive durante um turno (steering); thread dormente volta com `--resume`. | S |
| RF-SCH-03 | O agendamento guarda `origin` e `claude_session_id` da thread; se a thread dona foi apagada, o disparo **recria** uma thread com a mesma origem (UI / mesmo tópico do Telegram / voz) e tenta `--resume`; se a sessão também expirou, recomeça do zero sem perder a mensagem. É a única exceção à regra "só canais criam threads" (ADR-0017). | S |
| RF-SCH-04 | Disparos perdidos com o app fechado (ou módulo desligado) são **ignorados**: na partida, cron recalcula a próxima ocorrência a partir de agora e agendamento único no passado é pausado com `paused_reason`; ambos registrados em log. | S |
| RF-SCH-05 | Um laço único em processo dorme até o próximo `next_run_at` (máx. 60 s) e é acordado em qualquer alteração. Limites configuráveis: `max_schedules` (100), `min_interval_minutes` (5; rejeita `* * * * *`). Fuso: do agendamento, senão `SchedulerSettings.timezone`, senão o do sistema. | S |
| RF-SCH-06 | Ferramentas `schedule_create`, `schedule_list`, `schedule_get`, `schedule_update` (inclui pausar/retomar) e `schedule_delete`, família `scheduler`, política padrão `auto`; `schedule_create` só a partir de uma thread (o disparo volta para ela). O que o agente fizer no disparo segue as políticas das demais ferramentas. | S |
| RF-SCH-07 | Página **Agendamentos** na UI: lista com próximo disparo, thread dona, estado e histórico; formulário com prévia da expressão cron (próximas ocorrências e descrição); pausar/retomar, **Disparar agora** e excluir. Seção **Configurações → Agendamentos** com os campos de `SchedulerSettings`. | S |
| RF-SCH-08 | API local: `GET/POST /api/schedules`, `GET /api/schedules/preview`, `GET/PUT/DELETE /api/schedules/{id}`, `POST /api/schedules/{id}/run`. Cada disparo registra um `ScheduleRun` (últimos 50) e emite `ScheduleFired`/`SchedulesChanged`. | S |
| RF-SCH-09 | Ao disparar em thread de origem UI/voz, notificação nativa (`notify_on_fire`, padrão ligado); em thread do Telegram não, pois a resposta já vai para o tópico. | S |

## RF-BRW — Browser CDP (proxy para o agent-browser; não é canal)

| ID | Requisito | Prio |
|---|---|---|
| RF-BRW-01 | O serviço deve subir um **proxy CDP** em `127.0.0.1:<listen_port>` (padrão 9223) que repassa ao navegador real em `<upstream_host>:<upstream_port>` (padrão `127.0.0.1:9222`): HTTP de descoberta (`/json*`) com `Host` em loopback e reescrita de `host:porta` nas URLs devolvidas, e WebSocket bidirecional. | S |
| RF-BRW-02 | Todo processo `claude` recebe `AGENT_BROWSER_CDP=http://127.0.0.1:<listen_port>` enquanto o módulo está habilitado e, se `allow_agent_browser_tool`, `Bash(agent-browser:*)` na allowlist de `acceptEdits`; o agente não configura porta nem `--cdp`. | S |
| RF-BRW-03 | Se ninguém responde no upstream e `auto_launch` está ligado (só upstream loopback), o proxy **lança o navegador** (`--remote-debugging-port`, perfil dedicado em `<data_dir>/browser-profile`, `--no-first-run`, `--no-default-browser-check`, `--remote-allow-origins=*`, `--headless=new` opcional, `extra_args`, `about:blank`), espera até `launch_wait_secs` e só então repassa. Um trinco garante um único lançamento sob concorrência; falhas voltam como `503` JSON com motivo em pt-BR. | S |
| RF-BRW-03a | Descoberta em camadas (ADR-0020): HTTP `/json/version` em cada host candidato (`localhost` = IPv4 e IPv6); porta aberta sem `/json/*` (Chrome sem `--remote-allow-origins`) → `DevToolsActivePort` do perfil dedicado e, com `attach_existing`, dos perfis do usuário, validado por `Browser.getVersion`; nesse modo o proxy sintetiza `/json/version|list|new|activate|close` por CDP. Porta ocupada por processo não reconhecido → erro acionável, **sem** lançar outro navegador. Erros de `/json/*` em JSON. | S |
| RF-BRW-04 | Detecção do navegador por SO (Chrome, Chromium, Brave, Edge e o Chromium do `agent-browser doctor --json`), com `browser_path` manual opcional; visível ou headless. | S |
| RF-BRW-05 | Log de tráfego CDP em memória (`traffic_capacity`, padrão 500) classificando `command`/`response`/`event`/`error`, com contadores e prévia de 400 caracteres, exibido ao vivo na página Browser CDP (`GET /api/browser/traffic?after=`). | S |
| RF-BRW-06 | Página Browser CDP → Instalação: status de Node/npm/agent-browser/Chromium/skill; botões `npm install -g agent-browser@latest`, `agent-browser install` e "Instalar skill na base de conhecimento" (copia o `SKILL.md` de `agent-browser skills path` para `<kb>/.claude/skills/agent-browser/`; fallback `npx -y skills add vercel-labs/agent-browser -a claude-code -y` na KB). Trabalhos em segundo plano com saída acompanhada; ferramentas do Node achadas no PATH e em nvm/volta/fnm/npm global. | S |
| RF-BRW-07 | Testes na página: `/json/version` direto no navegador; abrir aba pelo proxy (`PUT /json/new`); agent-browser de ponta a ponta (`--cdp http://127.0.0.1:<listen_port> --session will-ia-test open about:blank`); Lançar/Fechar navegador (só o lançado pelo serviço). | S |
| RF-BRW-08 | Processos lançados (navegador, instalações) seguem a regra do serviço residente (`hide_console`, `die_with_parent`, `attach_to_job`, grupo de processos); `close_browser_on_stop` opcional. O módulo não cria threads, não expõe ferramentas MCP e só grava na KB a skill, por clique do usuário. Só loopback; o proxy não autentica (modelo do CDP local). | S |
| RF-BRW-09 | API local `/api/browser/*` (status, detect, traffic, launch, close, test/*, install/*) e seção `browser` de `Settings` aplicada a quente (listener reinicia só se porta/enabled mudam; env/allowlist valem no próximo spawn). | S |

## RF-LOG — Observabilidade

| ID | Requisito | Prio |
|---|---|---|
| RF-LOG-01 | Logs estruturados em arquivo com rotação diária e retenção configurável. | M |
| RF-LOG-02 | A seção Logs da UI mostra os últimos eventos com filtro por módulo e nível. | S |
| RF-LOG-03 | Botão "Exportar diagnóstico" gera um zip com logs (segredos redigidos), configuração sanitizada e versões. | C |

## RF-INST — Instalador (`will-ia-setup`, ADR-0021)

| ID | Requisito | Prioridade |
|---|---|---|
| RF-INST-01 | Um binário por plataforma (Windows, Linux, macOS) com `will-ia` e `will-ia-app` embutidos; assistente gráfico (egui) com Boas-vindas → Verificações → Opções → Instalação → Concluído e modo texto equivalente (`--cli`, `--yes`), automático sem sessão gráfica. | M |
| RF-INST-02 | Instalação **por usuário**, sem privilégios: binários em `~/Applications/will-ia.app` · `~/.local/bin` · `%LOCALAPPDATA%\Programs\will-ia`; verificação prévia de permissão de escrita. | M |
| RF-INST-03 | Detectar instalação existente (versão por `--version`), dados existentes, serviço registrado/em execução, processos vivos e instalações manuais anteriores; avisar downgrade/reparo. | M |
| RF-INST-04 | Atualização: parar serviço e processos (forçando o servidor residente após 5 s), trocar binários atomicamente, religar; **nunca** alterar a pasta de dados (só `install-manifest.json`). | M |
| RF-INST-05 | Registrar o servidor para iniciar com o sistema: LaunchAgent `dev.will.will-ia` (macOS; absorve `com.will.will-ia` e o Login Item do app), autostart `.desktop` ou user unit systemd (Linux), chave Run (Windows); opção de desligar. | M |
| RF-INST-06 | Atalhos do app: symlink na Mesa (macOS), `.desktop` no menu e na Área de Trabalho (Linux), `.lnk` no Menu Iniciar e na Área de Trabalho (Windows). | S |
| RF-INST-07 | `--uninstall` remove serviço, atalhos, binários e manifesto preservando os dados; `--purge` apaga os dados mediante confirmação. | S |
| RF-INST-08 | Não configurar nada do produto (Telegram, Google, WhatsApp, Claude): isso é do app. | M |
