# will-whatsapp

WhatsApp pessoal do dono exposto aos agentes **somente como ferramentas MCP**
(`whatsapp_*`). Não é canal de entrada. Suporta **várias contas pareadas**
(uma sessão, um índice e um estado por conta). Especificação: `docs/17-whatsapp.md`.

## Build

```bash
export PATH=$HOME/.cargo/bin:$PATH
cargo check  -p will-whatsapp                      # padrão: só MockBackend
cargo test   -p will-whatsapp
cargo clippy -p will-whatsapp --no-deps -- -D warnings
cargo check  -p will-whatsapp --features real      # backend whatsapp-rust 0.7
cargo clippy -p will-whatsapp --no-deps --features real -- -D warnings
```

`--no-deps` é necessário porque `clippy -p` também lint-a os crates do workspace
dos quais dependemos, e `will-core` tem um `derivable_impls`
(`ClaudeTokenRef::default`) fora do escopo deste crate.

## Feature `real` — estado

| Item | Resultado |
|---|---|
| Toolchain testada | Rust **stable 1.99.1** (rustc 58cf8f9c3 2026-10-13) |
| `whatsapp-rust 0.7.0` (`rust-version = 1.94`, `edition = 2024`) | compila em stable |
| Features usadas | `default-features = false, features = ["sqlite-storage", "tokio-transport", "tokio-runtime", "ureq-client", "tokio-native"]` |
| Features evitadas | `simd` (o `rust-toolchain.toml` do upstream pede nightly por causa dela) e `signal` (não precisamos de Ctrl-C) |
| `cargo check/clippy --features real` | **verde** (sem warnings com `-D warnings`) |
| `cargo test --features real` | compila; testes continuam usando `MockBackend` (sem rede) |
| Teste manual de pareamento | **não executado** (exige telefone real; fora do escopo automatizável) |

Se uma atualização do `whatsapp-rust` voltar a exigir nightly:
`cargo +nightly check -p will-whatsapp --features real`. O build padrão nunca
depende da feature: sem ela `WhatsAppModule::new_default` usa uma fábrica de
`MockBackend::unavailable`, cujo `connect()` responde
"backend WhatsApp indisponível neste build (compile com --features real)".

## Arquitetura

```
backend.rs  WhatsAppBackend (trait) + BackendEvent + IncomingMessage + helpers de JID/mime
mock.rs     MockBackend::new(account_id, tx) — roteiros de eventos (push), registro de envios (sent)
real.rs     RealBackend::new(account_id, dir, tx) — whatsapp-rust (feature `real`)
module.rs   WaAccount (estado por conta) + WhatsAppModule — N contas, BackendFactory,
            loop único de eventos (account_id, BackendEvent) → índice por conta (will-storage),
            Event::WhatsApp{state, detail JSON com account_id}, poda diária, backoff 1s→60s por conta
tools.rs    9 ferramentas MCP com parâmetro `account` + ToolProvider (available() == alguma conta conectada)
lib.rs      qr_svg(data) -> SVG (crate qrcode)
```

Fluxo: backend da conta → `mpsc::Sender<(account_id, BackendEvent)>` (compartilhado)
→ `WhatsAppModule::event_loop` → `Storage::wa_*(account_id, …)` (índice SQLite/FTS5) + `EventBus`.

### API do módulo

```rust
pub type BackendFactory = Arc<dyn Fn(&str, PathBuf, mpsc::Sender<(String, BackendEvent)>) -> Arc<dyn WhatsAppBackend> + Send + Sync>;

WhatsAppModule::new(deps, settings, factory) -> Arc<Self>   // contas de settings.accounts
WhatsAppModule::new_default(deps, settings) -> Arc<Self>    // RealBackend (feature real) ou MockBackend::unavailable
update_settings(&self, s).await                              // espelha s.accounts (cria/remove), aplica enabled/label
start(&self).await -> Result<(), String>                      // conecta contas ativas com sessão
stop(&self).await
pair(&self, account_id: Option<String>, phone: Option<String>, label: Option<String>).await -> Result<String, String>
unlink(&self, account_id, clear_index: bool, remove: bool).await -> Result<(), String>
reconnect(&self, account_id).await -> Result<(), String>
set_account(&self, account_id, label: Option<String>, enabled: Option<bool>).await -> Result<(), String>
account_refs(&self) -> Vec<WhatsAppAccountRef>               // para o servidor persistir
accounts() / account(id) / connected_accounts() / resolve_account(query) / is_connected()
status(&self).await -> serde_json::Value
module_status(&self) -> ModuleStatus
```

### Eventos no barramento (`Event::WhatsApp { state, detail }`)

`detail` é sempre um JSON string com `account_id`:

| state | detail |
|---|---|
| `qr` | `{"account_id", "data", "svg", "timeout_secs", "at"}` — `svg` pronto para a UI |
| `code` | `{"account_id", "code": "ABCD-1234"}` |
| `connected` | `{"account_id", "me": {jid, phone, push_name, platform, lid}}` |
| `disconnected` | `{"account_id", "reason"}` |
| `logged_out` | `{"account_id", "reason"}` — `"revogado no telefone"` ou `"desvinculado pelo usuário"` |

`status()` devolve:

```json
{ "enabled": true, "started": true, "backend": "real|mock", "media_dir": "<kb>/inbox/whatsapp",
  "accounts": [ { "id", "label", "phone", "enabled", "state", "detail", "me", "last_qr",
                  "last_pair_code", "last_sync", "has_session", "index": {"chats","messages","contacts"} } ],
  "index_total": {"chats","messages","contacts"} }
```

`state` ∈ `disconnected | qr | code | connected | logged_out | degraded`.
`module_status()`: `Disabled` · `Degraded("nenhuma conta pareada")` · `Stopped` · `Running`
(alguma conectada) · `Starting` (alguma pareando) · `Degraded(motivo da primeira conta ativa)`.

### Sessão e arquivos

- Sessão: `<data_dir>/whatsapp/<account_id>/store.db` (diretório 0700, arquivo 0600; `unlink`
  apaga `store.db`, `-wal`, `-shm`; remover a conta não apaga o diretório).
- Mídia baixada: `<kb>/<media_dir>` (padrão `inbox/whatsapp`), nome
  `<message_id>.<ext>` ou `<message_id>-<nome original>`.
- `media_ref` no índice (backend real) é um JSON com os parâmetros do CDN
  (`direct_path`, chaves em base64, tamanho, mime) — permite baixar mesmo após
  reinício, sem a mensagem em memória.

## Ferramentas

Todas com `requires: [Module::WhatsApp]`, `family: "whatsapp"`; retornam
`ToolError::Unavailable` quando nenhuma conta está conectada. Limites: ≤ 200 mensagens/chamada,
≤ 50 KB de texto (mantém as mais recentes e devolve `next_before` para paginar).

Parâmetro `account` (id, rótulo ou telefone) em todas:
- 1 conta conectada → usada; várias + `account` → id exato → rótulo (case-insensitive) → telefone (dígitos);
- várias sem `account`: `list_chats`, `search_messages`, `list_contacts` agregam (itens com
  `account`/`account_label`; raiz com `account: null`, `accounts: [...]`); as demais → `InvalidArgs`
  listando `id — rótulo — telefone`.
- Saída sempre com `account` e `account_label` na raiz; `approval_summary` dos envios inclui
  `(conta <rótulo>)`. Rate limit por conta (`wa_sent_count_since(account_id, since)`).

| Ferramenta | Política | Observações |
|---|---|---|
| `whatsapp_list_chats` | auto | `filter: all\|unread\|groups\|archived`, `limit` |
| `whatsapp_get_messages` | auto | `chat_id` (JID **ou nome**), `limit`, `before`, `since` (ISO-8601) |
| `whatsapp_search_messages` | auto | FTS5 com fallback LIKE; `chat_id?`, `since?` |
| `whatsapp_list_contacts` | auto | `query?` |
| `whatsapp_get_group` | auto | índice local; consulta o backend se faltar/`refresh` |
| `whatsapp_download_media` | auto | grava em `<kb>/inbox/whatsapp`, reutiliza se já baixado |
| `whatsapp_mark_read` | auto | local; recibo remoto só com `send_read_receipts` |
| `whatsapp_send_message` | **confirm** | `chat_id` ou `phone`; rate limit via `wa_sent_count_since`; `wa_outbox` |
| `whatsapp_send_media` | **confirm** | `path` canonicalizado dentro da KB |

Regras de envio:
- `phone` (ou `chat_id` só com dígitos) exige `allow_unsaved_numbers`; o número é
  resolvido via `resolve_phone` (erro claro se não estiver no WhatsApp).
- `chat_id` sem `@` é tratado como nome: chat único → JID; vários → erro listando
  candidatos; nenhum chat → procura contato por nome.
- JID que não é chat nem contato conhecido também exige `allow_unsaved_numbers`.
- `approval_summary`: `Enviar WhatsApp para <chat_id|phone>: <200 primeiros chars>`.

## `will-storage`

Todos os `wa_*` recebem `account_id` (primeiro parâmetro) ou o levam no struct
(`WaChat`/`WaMessage`/`WaContact.account_id`). `wa_prune(days)` é global;
`wa_stats(account_id)` e `wa_stats_all() -> Vec<(account_id, stats)>` para o `status()`.
Migration `0002_whatsapp_accounts.sql` recria as tabelas com chaves compostas (o índice
antigo de conta única é descartado e reconstruído pelo history sync).
