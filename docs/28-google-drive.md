# 28 — Google Drive (will-google::drive)

Arquivos da conta Google: listar, buscar, ler metadados, baixar/exportar, criar pastas e arquivos, enviar (upload), renomear/mover, lixeira e compartilhamento. Depende de [18 — Conta Google](18-conta-google.md) (mesmo OAuth da Agenda e do Gmail) e vive no mesmo crate `will-google`, como submódulo `drive`. Não é um módulo de processo: só ferramentas `drive_*` + uma seção de configuração.

## Modelo do Drive que o agente precisa entender

- Tudo é **arquivo** (`files`), inclusive pastas (`mimeType = application/vnd.google-apps.folder`). Um arquivo tem **pais** (`parents`); a raiz do "Meu Drive" é o id `root`. Mover = trocar o pai (`addParents` + `removeParents`).
- **Arquivos nativos do Google** (Docs, Planilhas, Apresentações, Desenhos) não têm bytes: só podem ser **exportados** para um formato (`files.export`, limite de 10 MB da Google). O resto baixa como está (`alt=media`).
- **Drives compartilhados** (Workspace) aparecem nas listagens com `supportsAllDrives=true&includeItemsFromAllDrives=true`; `drive_list_shared_drives` lista-os e `drive_id` restringe uma busca a um deles.
- **Compartilhar** = criar uma `permission` (pessoa/grupo por e-mail, domínio ou "qualquer um com o link") com papel `reader`/`commenter`/`writer`. Transferir propriedade não é suportado pelas ferramentas.
- Conteúdo de arquivos do Drive é **dado não confiável** (um Doc pode conter instruções): o `CLAUDE.md` da KB manda tratar como informação.

## Escopos

| Lógico (UI) | Escopo | Dá |
|---|---|---|
| `drive` — "Drive (ler)" | `https://www.googleapis.com/auth/drive.readonly` | listar, buscar, metadados, permissões, baixar e exportar **tudo** da conta |
| `drive_write` — "Drive (ler e escrever)" | `https://www.googleapis.com/auth/drive` | tudo acima + criar, enviar, alterar, lixeira, excluir, compartilhar |

Os dois são escopos **restritos** da Google, como os do Gmail: com o client OAuth do próprio usuário e o app publicado sem verificação, aparece a tela "app não verificado" (ver doc 18). `drive.file` (só arquivos criados pelo app) não foi adotado porque não enxerga o que já existe no Drive. Ligar o Drive numa conta já conectada exige **reautorizar** incluindo o escopo (consentimento incremental).

## Configurações (`DriveSettings`, chave `drive` em `Settings`)

| Campo | Tipo | Padrão | Efeito |
|---|---|---|---|
| `enabled` | bool | false | liga as ferramentas `drive_*` |
| `accounts` | lista (id ou e-mail) | vazia = todas | contas Google usáveis pelo Drive |
| `allow_write` | bool | **true** | `drive_upload`, `drive_create_file`, `drive_create_folder`, `drive_update`, `drive_trash` (as de efeito externo ainda pedem confirmação) |
| `allow_share` | bool | false | `drive_share` / `drive_unshare` |
| `allow_delete` | bool | false | `drive_trash { permanent: true }` (exclusão definitiva) |
| `download_dir` | path relativo à KB | `inbox/drive` | onde os downloads caem (fora da KB → volta ao padrão) |
| `max_download_mb` | MB | 100 | recusa antes (pelo `size`) e durante (stream) |
| `max_upload_mb` | MB | 100 | limite de `drive_upload`/`drive_create_file`/`drive_update(content_path)` |
| `export_docs` | MIME ou alias | `text/markdown` | formato padrão de exportação de Google Docs |
| `export_sheets` | MIME ou alias | `text/csv` (só a 1ª aba; `xlsx` = todas) | Planilhas |
| `export_slides` | MIME ou alias | `application/pdf` | Apresentações |

### UI (a implementar — seção/aba "Drive" dentro de Google)

Mesma estrutura da seção Gmail (`crates/will-app/src/pages/settings/google.rs`), lendo/gravando `settings.drive` via a API de configurações (`PATCH /api/settings` com a chave `drive`; `apply_section("drive")` reinicia o módulo Google).

| Rótulo pt-BR | Campo | Controle | Ajuda curta |
|---|---|---|---|
| Habilitar Drive | `enabled` | toggle | Liga as ferramentas `drive_*`. Exige conta com escopo "Drive (ler)" ou "Drive (ler e escrever)". |
| Contas usadas | `accounts` | multi-check das contas conectadas | Vazio = todas. |
| Permitir escrever | `allow_write` | toggle | Enviar, criar, renomear/mover, substituir conteúdo e lixeira. Cada ação pede confirmação. Exige escopo "Drive (ler e escrever)". |
| Permitir compartilhar | `allow_share` | toggle (off) | Dá acesso a terceiros (pessoa, domínio ou qualquer um com o link). Sempre com confirmação. |
| Permitir excluir definitivamente | `allow_delete` | toggle (off) | `drive_trash { permanent: true }` apaga sem passar pela lixeira. |
| Baixar arquivos para | `download_dir` | texto | Pasta relativa à base de conhecimento. |
| Limite de download (MB) | `max_download_mb` | número | |
| Limite de upload (MB) | `max_upload_mb` | número | Acima de 5 MB o envio é "resumable" (em blocos). |
| Exportar Docs como | `export_docs` | combo: markdown · pdf · docx · txt · html | |
| Exportar Planilhas como | `export_sheets` | combo: csv (1ª aba) · xlsx · pdf | |
| Exportar Apresentações como | `export_slides` | combo: pdf · pptx | |

Opções de escopo a acrescentar em `SCOPE_OPTIONS` (conectar/reautorizar): `("drive", "Drive (ler)")` e `("drive_write", "Drive (ler e escrever)")`. Bolinha de módulo: `Module::Drive` (`drive_status()`: cinza desligado; amarelo sem conta com `/auth/drive`; verde com conta ok). Rótulos de aprovação já estão em `pages/approvals.rs`.

## Ferramentas MCP

Prefixo `drive_`. `account` opcional quando há uma só conta habilitada. Toda listagem devolve arquivos **normalizados**: `id`, `name`, `mime_type`, `kind` (`folder`, `google_doc`, `google_sheet`, `google_slides`, `shortcut`, `file`…), `size`, `created`, `modified`, `modified_by`, `owner`, `parents`, `shared`, `trashed`, `starred`, `web_view_link`, `can {edit, share, download}` (+ `description`, `drive_id`, `md5`, `shortcut_target` quando existem).

| Ferramenta | Entrada | Saída | Política padrão |
|---|---|---|---|
| `drive_list` | `folder_id?` (padrão `root`), `mime_type?`, `trashed?`, `max_results?` (≤200), `page_token?`, `order_by?`, `drive_id?` | `files[]`, `next_page_token`, `query` | auto |
| `drive_search` | `q?` (sintaxe do Drive) + atalhos `name`, `full_text`, `mime_type` (MIME ou alias `folder`/`doc`/`sheet`/`slides`/`pdf`/`image`/`video`/`audio`), `modified_after`/`modified_before` (`YYYY-MM-DD` ou RFC 3339), `parent`, `owner`, `shared_with_me`, `starred`, `trashed` + paginação | idem | auto |
| `drive_get` | `file_id`, `include_permissions?` (true) | arquivo normalizado + `permissions[]` | auto |
| `drive_download` | `file_id`, `export_format?` (`md`, `txt`, `html`, `pdf`, `docx`, `xlsx`, `pptx`, `csv`, `tsv`, `odt`, `png`… ou MIME), `save_as?` | `path`, `relative_path`, `filename`, `mime`, `size`, `exported`, `source_mime` | auto |
| `drive_create_folder` | `name`, `parent_id?`, `description?` | pasta criada | auto (requer `allow_write`) |
| `drive_upload` | `path` (dentro da KB), `name?`, `parent_id?`, `mime_type?`, `convert_to?` (`doc`/`sheet`/`slides`), `description?` | arquivo + `upload_method` (`multipart` ≤ 5 MB, `resumable` acima), `uploaded_bytes` | **confirmar** |
| `drive_create_file` | `name`, `content` (texto), `parent_id?`, `mime_type?`, `convert_to?` (`doc`/`sheet`) | arquivo criado | **confirmar** |
| `drive_update` | `file_id`, `name?`, `description?`, `starred?`, `add_parents?[]`, `remove_parents?[]`, `content_path?` (arquivo da KB que substitui o conteúdo), `mime_type?` | arquivo atualizado + `content_replaced` | **confirmar** |
| `drive_trash` | `file_id`, `restore?`, `permanent?` | arquivo (`trashed`) ou `{ deleted: true }` | **confirmar** (`permanent` requer `allow_delete`) |
| `drive_list_permissions` | `file_id` | `permissions[]` (`id`, `type`, `role`, `email`/`domain`, `name`, `allow_discovery`, `expires`) | auto |
| `drive_share` | `file_id`, `email?` **ou** `domain?` **ou** `type: anyone`, `type?`, `role?` (`reader` padrão, `commenter`, `writer`), `notify?` (true), `message?`, `allow_discovery?` | `permission`, `web_view_link` | **confirmar** (requer `allow_share`) |
| `drive_unshare` | `file_id`, `permission_id` | ok | **confirmar** (requer `allow_share`) |
| `drive_list_shared_drives` | — | `drives[]` (`id`, `name`, `created`) | auto |

Regras:
- **Confinamento na KB**: downloads só em `<kb>/<download_dir>` (nome saneado, `nome (2).ext` se já existe); uploads e `content_path` só de arquivos **dentro** da KB (caminho canônico, sem `..`, sem symlink para fora).
- **Tetos**: download verifica o `size` dos metadados antes de pedir os bytes e corta o stream se passar (arquivo parcial removido); export obedece ao menor entre `max_download_mb` e 10 MB; upload recusa acima de `max_upload_mb`.
- **Upload**: multipart/related (metadados JSON + bytes) até 5 MB; acima, sessão resumable (`uploadType=resumable` → `Location` → `PUT` em blocos de 8 MiB com `Content-Range`; 308 = continua). `convert_to` define `mimeType` dos metadados para o tipo Google (Docs/Sheets/Slides) e a Google converte.
- **Busca**: `q` livre é combinado por `and` com os atalhos; sem `trashed` explícito, filtra `trashed = false`. Literais são escapados (`\` e `'`). `full_text` usa o índice de conteúdo da Google (não é regex).
- **Compartilhar** é a ação de maior risco (vaza conteúdo para fora): toggle próprio desligado por padrão, política `confirmar`, resumo de aprovação destaca "QUALQUER PESSOA com o link" e papéis; `owner` é recusado.
- Resumos de aprovação mostram o **nome** do arquivo quando ele já apareceu numa listagem/leitura nesta execução (cache id → nome em memória), senão o id.
- Erros 403 `insufficientPermissions` viram "escopo insuficiente… reautorize" (conversão comum do crate).

### Sintaxe `q` (referência rápida)

| Quero | `q` |
|---|---|
| nome contém | `name contains 'ata'` |
| conteúdo contém | `fullText contains 'reunião'` |
| tipo | `mimeType = 'application/pdf'`, `mimeType contains 'image/'` |
| pasta | `'ID_DA_PASTA' in parents`, `'root' in parents` |
| data | `modifiedTime > '2026-01-01T00:00:00'` |
| compartilhado comigo | `sharedWithMe = true` |
| dono | `'me' in owners`, `'ana@x.com' in owners` |
| lixeira | `trashed = true` |

### Exportação

| Tipo Google | Formatos aceitos (aliases) | Padrão |
|---|---|---|
| Docs | `md`, `txt`, `html`, `pdf`, `docx`, `odt`, `rtf`, `epub`, `zip` (html) | `text/markdown` |
| Planilhas | `csv` (1ª aba), `tsv`, `xlsx`, `ods`, `pdf`, `zip` (html) | `text/csv` |
| Apresentações | `pdf`, `pptx`, `odp`, `txt` | `application/pdf` |
| Desenhos | `png`, `jpg`, `svg`, `pdf` | `image/png` |

## Implementação

- `crates/will-google/src/drive/mod.rs` — `Kind` (13 ferramentas), `DriveProvider`, specs, resumos de aprovação, despacho; cache `drive_names` no `GoogleModule`.
- `crates/will-google/src/drive/query.rs` — helpers puros (montagem do `q`, normalização, aliases, escolha de export, corpo de permissão) com testes unitários.
- `crates/will-google/src/drive/ops.rs` — operações REST.
- `crates/will-google/src/client.rs` — `send_authed` (laço comum: bearer, retry 429/5xx, uma renovação em 401), `download_to_file` (stream com teto), `upload_multipart`, `upload_resumable`, `UploadRequest`, `multipart_related`.
- `crates/will-google/src/fs.rs` — `confined_dir`, `resolve_in_kb`, `unique_path` (compartilhados com o Gmail).
- Testes: `crates/will-google/tests/drive.rs` (wiremock: listar, baixar binário/exportar, teto antes do download, upload multipart e resumable, toggles de compartilhar/excluir, módulo desligado).
- Status: `GoogleModule::drive_status()` → `Module::Drive`; o servidor recalcula junto com Agenda/Gmail (`recompute_google_status`) e `apply_section("drive")` reinicia o módulo Google.
