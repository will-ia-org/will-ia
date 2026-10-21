# 09 — Servidor MCP (will-mcp)

O serviço expõe um servidor **MCP (Model Context Protocol)** local, chamado `will_ia`, para que os agentes Claude Code usem capacidades do próprio will-ia (Telegram, WhatsApp, Agenda, Gmail, notificações). MCPs de **terceiros** não passam por aqui: são catalogados e entregues ao Claude Code via `.mcp.json` da KB, ver [21 — MCPs externos](21-mcps-externos.md).

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
| will-core | `notify_user`, `list_threads`, `create_thread`, `send_to_thread`, `kb_info` | este doc |

## Políticas de aprovação

Cada ferramenta tem uma política persistida em `tool_policies`:

| Política | Comportamento |
|---|---|
| `auto` | executa imediatamente |
| `confirmar` | cria um `Approval` com resumo legível dos argumentos, notifica o dono (UI + notificação nativa + Telegram com botões ✅/❌ se ativo) e **bloqueia** a chamada até decisão ou timeout (padrão 5 min → `denied: timeout`) |
| `desligada` | não aparece em `tools/list`; chamada direta retorna erro |

Padrões: leitura → `auto`; efeito externo (`telegram_send_message` para fora do tópico de origem, `whatsapp_send_*`, `gmail_send`, `gmail_modify`, `calendar_create/update/delete_event`) → `confirmar`. O usuário pode alterar por ferramenta na seção MCP, com aviso ao baixar para `auto`.

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

### `notify_user` (Should)

`{ title, body }` → notificação nativa no desktop. Útil para threads em background avisarem "terminei".

### `list_threads` / `create_thread` / `send_to_thread` (Could)

Permitem que um agente consulte threads existentes, crie uma nova com prompt inicial e envie mensagem a outra thread (delegação entre cérebros). Guardadas por toggle, desligadas por padrão.

### Famílias `whatsapp_*`, `calendar_*`, `gmail_*`

Especificadas nos docs dos módulos ([17](17-whatsapp.md), [19](19-google-agenda.md), [20](20-gmail.md)). Compartilham as convenções: nomes `modulo_verbo_objeto`, paginação por token/cursor, limites de tamanho de saída, caminhos de arquivo sempre dentro da KB, erros com mensagem acionável (ex.: "conta Google precisa reautorizar").

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
