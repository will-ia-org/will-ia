# 17 — WhatsApp (will-whatsapp)

O WhatsApp **pessoal do dono** é pareado ao serviço e exposto aos agentes **somente como ferramentas MCP**: ler conversas, não lidas, grupos, buscar, resumir, enviar. **Não é um canal de entrada**: mensagens recebidas no WhatsApp não criam threads nem acionam o cérebro. Quem decide ler ou responder algo é um agente já em execução (via UI, Telegram ou voz).

O módulo suporta **várias contas pareadas** (ex.: número pessoal e número de trabalho). Cada conta tem sessão, índice e estado de conexão próprios; as ferramentas recebem um parâmetro `account` para escolher a conta.

> Nota: usa o protocolo não oficial do WhatsApp Web (biblioteca `whatsapp-rust`), não a API oficial da Meta. Ver [ADR-0011](adr/0011-whatsapp-rust-tools-only.md).

## Biblioteca

`whatsapp-rust` (oxidezap), MIT, implementação async do protocolo multi-device, com crates `wacore` (núcleo) e `waproto` (protobufs) e backend SQLite próprio. É a opção mais ativa e completa do ecossistema Rust (pareamento por QR e por código, E2E, mídia, grupos, recibos, history sync). Fork `wa-rs` como plano B se a principal exigir toolchain nightly. A versão é 0.x: fixar versão exata e isolar atrás de uma trait `WhatsAppBackend` para absorver quebras.

## Contas e pareamento (UI)

Seção **WhatsApp** nas configurações: toggle do módulo + **lista de contas** com botão **Adicionar conta** (Parear por QR / Parear por código, rótulo opcional). Cada conta da lista mostra:

| Estado | Tela |
|---|---|
| Desconectado (sem sessão) | Botões **Parear por QR** / **Parear por código** |
| Aguardando pareamento | QR renderizado localmente (crate `qrcode` → SVG), renovado a cada expiração; ou campo com o código de 8 dígitos para digitar no telefone (número informado antes) |
| Conectado | Número, nome do perfil, última sincronização, contagem de chats indexados; botões **Desvincular** (logout remoto + apaga sessão local; opções: apagar índice, remover conta) e **Reconectar** |
| Degradado | Motivo (sem rede, sessão revogada pelo telefone, versão do protocolo), com ação sugerida |

Além disso, cada conta tem **rótulo** editável e toggle **ativa** (`PATCH /api/whatsapp/accounts/{id}`). Uma conta nova nasce com rótulo "Conta N"; ao conectar, se o rótulo ainda for o padrão, vira o nome do perfil (ou o telefone).

Identificação: `id` (10 hex, gerado no pareamento), `label`, `phone`. As contas são persistidas em `settings.whatsapp.accounts` (`WhatsAppAccountRef { id, label, phone, enabled, created_at }`), sincronizadas pelo serviço a partir de `WhatsAppModule::account_refs()` após `start`, `pair`, `unlink` e `PATCH`.

A sessão (chaves Signal, identidade) de cada conta fica no banco da própria biblioteca, em `<data_dir>/whatsapp/<account_id>/store.db`, com permissão 0600. Desvincular apaga o arquivo; remover a conta das configurações **não** apaga o diretório.

## Índice local de mensagens

O protocolo entrega mensagens em tempo real (e um history sync inicial), mas não oferece "buscar histórico sob demanda". Para as ferramentas funcionarem (não lidas, busca, resumo), o módulo mantém um **índice local** em SQLite (`wa_chats`, `wa_messages`, `wa_contacts`, `wa_groups`, `wa_outbox`), **particionado por `account_id`** (chaves compostas `(account_id, jid)` / `(account_id, id)`; migration `0002_whatsapp_accounts.sql`), alimentado pelos eventos da biblioteca:

- Texto, legendas, remetente, timestamp, chat, flags (`from_me`, `read`, `starred`), tipo (texto/imagem/áudio/documento/sticker/localização), referência de mídia (download sob demanda).
- Contadores de não lidas por chat, atualizados por recibos e pelo `mark_read`.
- Retenção configurável (padrão 90 dias); mídia só é baixada quando uma ferramenta pede, para `<kb>/inbox/whatsapp/`.
- Busca por `FTS5` sobre o texto.

Sem o índice, só seria possível reagir a mensagens novas. Com ele, o agente consegue "resuma o que aconteceu no grupo X hoje".

### LIDs (identidades novas do WhatsApp) e atualização do índice

Desde 2025 o WhatsApp endereça contatos por **LID** (`<id>@lid`) em vez do telefone (`<n>@s.whatsapp.net`). O índice chaveia cada conversa pelo JID que o WhatsApp usa, então a conversa viva de um contato fica em `…@lid` e o chat `…@s.whatsapp.net` antigo para de receber mensagens. Para não parecer "histórico parado": (1) ao indexar, o telefone vindo de `sender_alt`/`recipient_alt` é gravado em `wa_contacts.phone` do JID LID; (2) `resolve_chat_id` com número ou JID de telefone prefere o chat LID do mesmo contato. O status por conta mostra **última mensagem indexada** e **mensagens nas últimas 24 h** (amarelo se a última tem mais de 24 h). A indexação é contínua enquanto o servidor está conectado; mensagens recebidas com o servidor fora chegam na reconexão (retomada offline da biblioteca).

## Ferramentas MCP

Prefixo `whatsapp_`. Cada uma tem toggle e **política** (`auto` / `confirmar` / `desligada`, ver [09](09-servidor-mcp.md#políticas-de-aprovação)). **Todas** aceitam o parâmetro opcional `account` (id, rótulo ou telefone da conta).

| Ferramenta | Entrada | Saída | Política padrão |
|---|---|---|---|
| `whatsapp_list_chats` | `filter: all\|unread\|groups\|archived`, `limit` | chats com nome, id (JID), não lidas, última mensagem, timestamp | auto |
| `whatsapp_get_messages` | `chat_id`, `limit`, `before?`, `since?` | mensagens cronológicas com remetente, texto, tipo, `message_id` | auto |
| `whatsapp_search_messages` | `query`, `chat_id?`, `since?`, `limit` | ocorrências com contexto | auto |
| `whatsapp_list_contacts` | `query?` | nome, número, JID | auto |
| `whatsapp_get_group` | `chat_id` | nome, descrição, participantes, admins | auto |
| `whatsapp_download_media` | `message_id` | caminho em `inbox/whatsapp/`, mime, tamanho | auto |
| `whatsapp_mark_read` | `chat_id` | ok | auto (toggle: enviar recibo azul ou só local) |
| `whatsapp_send_message` | `chat_id` ou `phone`, `text`, `reply_to?` | `message_id` | **confirmar** |
| `whatsapp_send_media` | `chat_id`, `path` (dentro da KB), `caption?` | `message_id` | **confirmar** |

Seleção de conta (`account`):
- Exatamente **uma** conta conectada → ela é usada (com `account` informado, precisa bater).
- Várias contas e `account` informado → resolve por **id exato**, depois **rótulo** (case-insensitive), depois **telefone** (só dígitos). Conta resolvida mas desconectada → erro "indisponível".
- Várias contas e `account` ausente → `whatsapp_list_chats`, `whatsapp_search_messages` e `whatsapp_list_contacts` **agregam** todas as contas conectadas (cada item com `account` = id e `account_label`; a raiz traz `account: null` e `accounts: [...]`). As demais retornam erro de argumento inválido listando as contas (`id — rótulo — telefone`).
- Toda saída inclui `account` (id) e `account_label` na raiz. O resumo de aprovação de envios inclui o rótulo: `Enviar WhatsApp (conta Trabalho) para …`.

Regras:
- `phone` só é aceito se "permitir envio para números não salvos" estiver ligado; caso contrário, apenas chats/contatos existentes.
- Rate limit de envio **por conta** (padrão 20 mensagens / 10 min) para reduzir risco de bloqueio; excedente retorna erro explicando.
- `path` de mídia deve estar dentro da KB (sem path traversal).
- Toda chamada é registrada em `mcp_calls`; envios também em `wa_outbox` com status (enviado/entregue/lido/erro).
- Saída textual é truncada a um limite por chamada (padrão 200 mensagens / 50 KB) com paginação por `before`.

## Orientação ao agente

O `CLAUDE.md` da base de conhecimento (gerado) inclui: "o WhatsApp é a conta pessoal do usuário; nunca envie mensagens sem pedido explícito; ao resumir conversas, preserve privacidade de terceiros; prefira `whatsapp_get_messages` com `since` ao invés de carregar históricos inteiros".

## Ciclo de vida

- `start()`: liga o loop único de eventos `(account_id, BackendEvent)` e a poda diária; para cada conta **ativa** com sessão, conecta. Sem contas → log "nenhuma conta pareada" e status `Degraded("nenhuma conta pareada")`. Conta sem sessão fica "Desconectado" sem erro.
- `update_settings(s)`: espelha `s.accounts` (cria contas novas via fábrica de backend; conta ausente é desconectada e descartada, sem apagar o store).
- Reconexão por conta com backoff (1 s → 60 s). Logout remoto de uma conta → estado `logged_out` + notificação (as outras contas seguem).
- `stop()`: fecha todas as conexões; stores permanecem.
- Status do módulo: `Disabled` (desligado) · `Degraded("nenhuma conta pareada")` · `Running` (alguma conectada) · `Starting` (alguma pareando) · `Degraded(motivo da primeira conta ativa)`.

## Eventos no barramento

`Event::WhatsApp { state, detail }` com `detail` sempre um JSON string contendo `account_id`:

| state | detail |
|---|---|
| `qr` | `{"account_id","data","svg","timeout_secs","at"}` |
| `code` | `{"account_id","code"}` |
| `connected` | `{"account_id","me":{jid, phone, push_name, lid, platform}}` |
| `disconnected` | `{"account_id","reason"}` |
| `logged_out` | `{"account_id","reason"}` (`"revogado no telefone"` ou `"desvinculado pelo usuário"`) |

## Testes

- `WhatsAppBackend` mockado com roteiros de eventos (mensagem recebida, recibo, history sync, logout), injetado por `BackendFactory` (um mock por conta); cenários com duas contas (agregação, exigência de `account`, resolução por rótulo/telefone, `unlink(remove)`).
- Testes do índice (FTS, contadores de não lidas, retenção).
- Teste manual guiado de pareamento (não automatizável).
