# 09 — Servidor MCP (will-mcp)

O serviço expõe um servidor **MCP (Model Context Protocol)** local, chamado `will_ia`, para que os agentes Claude Code usem capacidades do próprio will-ia (Telegram, WhatsApp, Agenda, Gmail, agendamentos, notificações). MCPs de **terceiros** não passam por aqui: são catalogados e entregues ao Claude Code via `.mcp.json` da KB, ver [21 — MCPs externos](21-mcps-externos.md).

## Transporte e segurança

- **Streamable HTTP** em `http://127.0.0.1:<porta>/mcp`, servidor `axum` + crate oficial `rmcp` (`transport-streamable-http-server`).
- Porta: aleatória livre por padrão (gravada para a sessão), fixável nas configurações.
- **Bearer token efêmero por processo `claude`**: emitido no spawn, mapeado para `thread_id`, revogado quando o processo encerra. Requisição sem token válido → 401. Isso dá ao servidor a identidade da thread chamadora sem o agente precisar informá-la.
- Só `127.0.0.1`; sem CORS; sem TLS (loopback).
- Toda chamada registrada em `mcp_calls` (thread, ferramenta, args resumidos e redigidos, ok/erro, duração).

## Registro de ferramentas

```rust
#[async_trait]
pub trait WillTool: Send + Sync {
    fn name(&self) -> &'static str;              // ex.: "telegram_send_message"
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> serde_json::Value;
    fn requires(&self) -> &'static [Module];     // ex.: [Module::Telegram] — só listada se o módulo está ativo
    async fn call(&self, ctx: ToolContext, args: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
pub struct ToolContext { pub thread_id: ThreadId, pub origin: Origin, pub orchestrator: OrchestratorHandle }
```

`tools/list` retorna apenas ferramentas com política diferente de `desligada` e cujos módulos requeridos estão ativos e autorizados. Alterar toggles/políticas dispara `notifications/tools/list_changed`.

Cada módulo contribui com um `ToolProvider`:

| Provider | Família | Doc |
|---|---|---|
| will-telegram | `telegram_*` | este doc |
| will-whatsapp | `whatsapp_*` | [17](17-whatsapp.md) |
| will-google (calendar) | `calendar_*` | [19](19-google-agenda.md) |
| will-google (gmail) | `gmail_*` | [20](20-gmail.md) |
| will-google (drive) | `drive_*` | [28](28-google-drive.md) |
| will-scheduler | `schedule_*` (família `scheduler`) | [24](24-scheduler.md) |
| will-core | `notify_user`, `list_threads`, `create_thread`, `send_to_thread`, `kb_info` | este doc |

## Políticas de aprovação

Cada ferramenta tem uma política persistida em `tool_policies`:

| Política | Comportamento |
|---|---|
| `auto` | executa imediatamente |
| `confirmar` | cria um `Approval` com resumo legível dos argumentos, notifica o dono (UI + notificação nativa + Telegram com botões ✅/❌ se ativo) e **bloqueia** a chamada até decisão ou timeout (padrão 5 min → `denied: timeout`) |
| `desligada` | não aparece em `tools/list`; chamada direta retorna erro |

Padrões: leitura → `auto`; efeito externo (`telegram_send_message`/`telegram_send_file`/`telegram_send_voice`, `whatsapp_send_*`, `gmail_send`, `gmail_modify`, `calendar_create/update/delete_event`, `drive_upload/create_file/update/trash/share/unshare`) → `confirmar`. O usuário pode alterar por ferramenta na seção MCP, com aviso ao baixar para `auto`.

O resultado de uma chamada negada é um `ToolOutput` com `is_error: true` e texto "Ação não aprovada pelo usuário", para que o agente informe e não insista. Decisões ficam em `approvals` e aparecem na aba Logs. Ver [ADR-0013](adr/0013-politicas-de-aprovacao-mcp.md).

## Ferramentas

### `telegram_send_message` (MVP)

```json
{
  "type": "object",
  "properties": {
    "text":      { "type": "string", "description": "Texto da mensagem (texto puro; HTML básico se parse_mode=html)." },
    "chat_id":   { "type": "string", "description": "Opcional. Chat de destino. Padrão: chat de origem da thread, ou o chat padrão configurado." },
    "topic_id":  { "type": "integer", "description": "Opcional. Tópico do fórum. Padrão: tópico de origem da thread." },
    "parse_mode":{ "type": "string", "enum": ["text", "html"], "default": "text" },
    "reply_to_message_id": { "type": "integer" }
  },
  "required": ["text"]
}
```

Regras:
- Destino só pode ser um chat da **allowlist** (o agente não consegue mandar mensagem para fora).
- Sem `chat_id`/`topic_id`: se a thread nasceu no Telegram, usa seu tópico; senão usa o "destino padrão" configurado (primeiro chat da allowlist, como na referência).
- Texto dividido em 4096; retorno `{ "message_ids": [...] }`.
- Em modo `html`, o texto é validado/escapado; falha de parse do Telegram → tenta de novo como texto puro e avisa no resultado.

### `telegram_send_file`

Envia um arquivo da **base de conhecimento** (foto, vídeo, áudio, GIF/animação ou documento) com legenda opcional. Política padrão **confirmar**.

```json
{
  "type": "object",
  "properties": {
    "path":       { "type": "string", "description": "Caminho do arquivo dentro da KB (absoluto ou relativo à KB)." },
    "caption":    { "type": "string", "description": "Legenda (opcional, ≤ 1024 caracteres; HTML básico se parse_mode=html)." },
    "kind":       { "type": "string", "enum": ["auto", "photo", "video", "audio", "animation", "document"], "default": "auto" },
    "parse_mode": { "type": "string", "enum": ["text", "html"], "default": "text" },
    "duration_secs": { "type": "integer", "minimum": 1 },
    "chat_id":    { "type": "string" },
    "topic_id":   { "type": "integer" },
    "reply_to_message_id": { "type": "integer" }
  },
  "required": ["path"]
}
```

Regras (`crates/will-telegram/src/media.rs`):
- `path` precisa resolver (canônico) para um arquivo regular **dentro da KB**; fora → `Forbidden`.
- `kind=auto` pela extensão: imagem → `sendPhoto` (acima de 10 MB vira `sendDocument`), `.gif` → `sendAnimation`, vídeo → `sendVideo`, áudio → `sendAudio`, resto → `sendDocument`. `document` explícito preserva o arquivo sem recompressão.
- Limite do Bot API por upload: **50 MB**; legenda ≤ 1024 caracteres (texto longo vai em `telegram_send_message`).
- Chat action coerente com o tipo (`upload_photo`, `upload_video`, `upload_document`…). Legenda em HTML recusada → reenvia como texto puro.
- Retorno `{ message_id, chat_id, topic_id, kind, mime, size_bytes, file_name }`.

### `telegram_send_voice`

Mensagem de **voz** (balão com onda sonora, `sendVoice`): `text` → fala sintetizada pela OpenAI (mesma chave do módulo de voz; voz e velocidade do "resumo em áudio" do Telegram) **ou** `path` → áudio da KB (ogg/opus de preferência; mp3/m4a também). Política padrão **confirmar**.

```json
{
  "type": "object",
  "properties": {
    "text":    { "type": "string", "description": "Texto a ser falado (≤ 4000 caracteres); alternativa a path." },
    "path":    { "type": "string", "description": "Áudio dentro da KB; alternativa a text." },
    "caption": { "type": "string" },
    "duration_secs": { "type": "integer", "minimum": 1 },
    "chat_id": { "type": "string" },
    "topic_id": { "type": "integer" },
    "reply_to_message_id": { "type": "integer" }
  }
}
```

- Sem chave da OpenAI, `text` devolve `Unavailable` pedindo `path`.
- A fala sintetizada é gravada em `<kb>/<attachments_dir>/voz-enviada/voz-<uuid>.ogg` antes do envio (fica como registro).
- Retorno `{ message_id, chat_id, topic_id, path, synthesized, mime, size_bytes }`.

### `notify_user` (Should)

`{ title, body }` → notificação nativa no desktop. Útil para threads em background avisarem "terminei".

### `list_threads` / `create_thread` / `send_to_thread` (Could)

Permitem que um agente consulte threads existentes, crie uma nova com prompt inicial e envie mensagem a outra thread (delegação entre cérebros). Guardadas por toggle, desligadas por padrão.

### Famílias `whatsapp_*`, `calendar_*`, `gmail_*`, `drive_*`

Especificadas nos docs dos módulos ([17](17-whatsapp.md), [19](19-google-agenda.md), [20](20-gmail.md), [28](28-google-drive.md)). Compartilham as convenções: nomes `modulo_verbo_objeto`, paginação por token/cursor, limites de tamanho de saída, caminhos de arquivo sempre dentro da KB, erros com mensagem acionável (ex.: "conta Google precisa reautorizar").

### Família `schedule_*` (scheduler)

Especificada em [24 — Agendamentos](24-scheduler.md). Política padrão **`auto`** nas cinco (agendar não tem efeito externo; o efeito acontece quando o agente age no disparo, sob as políticas das outras ferramentas). `id` aceita uuid completo ou prefixo único.

| Ferramenta | Entrada | Saída |
|---|---|---|
| `schedule_create` | `name`, `message`, `cron` **ou** `run_at`, `timezone?` | `{ ok, schedule, hint }` — exige thread chamadora (o disparo volta para ela) |
| `schedule_list` | `all?` | `{ count, schedules }` (padrão: só os da thread) |
| `schedule_get` | `id` | `{ schedule, runs }` |
| `schedule_update` | `id`, campos parciais, `enabled?` (pausar/retomar) | `{ ok, schedule }` |
| `schedule_delete` | `id` | `{ ok, deleted }` |

### `kb_info` (Could)

Retorna caminho da KB, variáveis do contexto e lista de ferramentas em `tools/README.md`. Redundante com o filesystem, mas conveniente para scripts.

## Uso por scripts em `tools/`

Scripts Node/Python também podem chamar o MCP (é HTTP), usando `WILL_IA_MCP_URL` e o header `Authorization` disponível em `WILL_IA_MCP_TOKEN` no ambiente do processo. Isso permite, por exemplo, um script de monitoramento disparar um alerta no Telegram.

### Token para scripts (fora das threads)

Processos que **não** nasceram de uma thread (um terminal, um cron, outro programa) não têm `WILL_IA_MCP_TOKEN` no ambiente. Para eles existe um **token fixo**, gerado pelo serviço na primeira subida do servidor MCP e guardado no cofre de segredos (chave `mcp_script_token`, keyring ou arquivo 0600 em modo portátil). A cada subida ele é registrado de novo no `TokenRegistry` como chamador **geral** (`Caller.generic = true`, sem `thread_id`/`origin`).

- UI: **Configurações › Servidor MCP › Token para scripts** — mostrar/ocultar, copiar, "copiar como export" (`WILL_IA_MCP_URL` + `WILL_IA_MCP_TOKEN` prontos para o shell) e **Regenerar** (o anterior deixa de valer na hora).
- API local: `GET /api/mcp/script-token` e `POST /api/mcp/script-token/rotate` (ver [22](22-api-local.md)).
- Chamadas com esse token não têm thread de origem: ferramentas que respondem "na thread atual" precisam do destino explícito; políticas e aprovações valem igual. O valor nunca entra em logs, settings ou `.mcp.json`; a URL do servidor muda a cada execução quando a porta é 0, por isso o "copiar como export" inclui a URL atual.

## Injeção no Claude Code

Entrada `will_ia` no `<kb>/.mcp.json` gerado, com `Authorization: Bearer ${WILL_IA_MCP_TOKEN}` expandido por spawn. Ver [05 — Motor](05-motor-claude-code.md#configuração-mcp) e [21](21-mcps-externos.md). O engine valida no evento `system/init` que `mcp_servers[]` contém `will_ia` com `status: connected`; caso contrário registra aviso (o agente seguirá sem as ferramentas).

## Testes

- Cliente MCP de teste (`rmcp` client) contra o servidor: `initialize`, `tools/list` com toggles, `tools/call` com e sem bearer, destino fora da allowlist.
- Teste de integração: `fake-claude` recebe `--mcp-config` e faz uma chamada real ao servidor.

## Política "automático para listados" e destinatários confiáveis (desde 0.1.33)

Quarto nível de política, além de `auto`, `confirm` e `off`: **`auto_listed`**. A chamada executa sem
confirmação só quando **todos** os destinatários estão na lista de confiança do canal; em qualquer outro caso
(alguém fora da lista, ou a ferramenta não soube dizer para quem vai) cai em confirmar.

- **Quem aceita:** só ferramenta com destinatário, isto é, com `WillTool::trust_channel()` (`ToolListing.trust_channel`
  na API; `PUT /api/mcp/tools/{name}/policy` recusa `auto_listed` nas demais).
  - WhatsApp: tudo que aponta para um chat (`chat_id`/`phone`: ler mensagens, grupo, marcar lido, enviar
    texto/mídia/voz/contato), para uma mensagem (`message_id`: baixar mídia, reagir, estado, editar, apagar) ou
    para um contato (`contact`). Listar chats/contatos e buscar não têm destinatário.
  - Gmail: enviar, criar/atualizar/enviar/apagar rascunho, encaminhar e responder a todos (Para + Cc + Cco; no
    rascunho e no responder a todos os destinatários são lidos da mensagem). Leitura e `gmail_modify` não entram.
- **Contrato:** `WillTool::recipients(args) -> Option<Vec<Recipient>>` devolve a chave **canônica** de cada
  destinatário (`will_core::trust`): telefone só em dígitos e, no Brasil, sem o nono dígito (`wa_phone_key`);
  grupo pelo JID `…@g.us` (item próprio: listar uma pessoa não libera os grupos dela); LID resolvido para o
  telefone pelo índice; e-mail em minúsculas. O servidor MCP só compara chaves.
- **Lista:** `settings.mcp.trusted` (`TrustedRecipients { whatsapp, email }`, entradas `{ key, label }`), uma por
  **canal**. No e-mail vale endereço exato ou domínio inteiro (`@empresa.com`; subdomínio não herda).
  Só muda por `GET/POST /api/mcp/trusted` e `POST /api/mcp/trusted/remove` (`{ channel, value, label }`, o servidor
  normaliza); `PUT /api/settings` da seção `mcp` preserva a lista vigente. Nenhuma ferramenta do agente a altera.
- **Aprovar e confiar:** quando `auto_listed` cai em confirmar com destinatários conhecidos, o pedido leva
  `ApprovalRequest.trust` (`TrustOffer { channel, recipients }`, só os que faltam). `POST /api/approvals/{id}` com
  `{ "decision": "approved", "trust": true }` aprova e inclui esses destinatários (a oferta sai do pedido pendente,
  nunca do corpo). Só no app (página e pilha flutuante); o Telegram segue com Aprovar/Negar.
- **Auditoria:** `mcp_calls.policy` (migration 0016) registra como cada chamada foi liberada: `auto`,
  `auto_listed`, `confirm` ou `off`; aparece na coluna "Liberação" de Chamadas recentes.
- **App:** Configurações › Servidor MCP › aba **Destinatários confiáveis** (WhatsApp: busca por nome nos
  contatos/grupos ou número digitado; e-mail: endereço ou domínio) e a opção "automático p/ listados" no seletor
  de política das ferramentas com destinatário.

## Espera da aprovação, cancelamento e envio duplicado (desde 0.1.33)

Caso real (17/11/2026): o `claude` desiste de uma chamada HTTP ao MCP em **60 s** quando o servidor não tem
`timeout` próprio no `.mcp.json`. A aprovação demorou 66 s, o agente recebeu "The operation timed out.", o serviço
enviou a mensagem assim que o usuário aprovou e o agente, sem saber, mandou de novo.

- **Limite do cliente alinhado:** a entrada `will_ia` do `.mcp.json` leva `timeout` (ms) =
  `McpSettings::client_timeout_ms()` = tempo de aprovação + 120 s. Quem esgota primeiro é sempre o serviço, que
  responde "não aprovada (tempo esgotado)" e tira o pedido da tela.
- **Cancelamento:** se o cliente cancela a chamada durante a espera (`RequestContext.ct`), o pedido sai da fila
  (`ApprovalManager::request` emite `ApprovalDecided` com `Timeout` ao ser descartado) e nada é executado.
- **Envio duplicado:** ferramentas com `WillTool::guards_duplicates()` (WhatsApp: mensagem, mídia, voz, contato;
  Gmail: enviar, enviar rascunho, encaminhar, responder a todos) não repetem uma chamada **idêntica** (mesma
  ferramenta e mesmos argumentos): em andamento → erro "aguardando aprovação, não repita"; concluída dentro de
  `settings.mcp.duplicate_window_secs` (padrão 600; 0 desliga) → devolve, sem erro, o resultado da primeira com
  o aviso de duplicidade. Falha ou negativa libera nova tentativa. Em `mcp_calls.policy` fica `duplicate`.

