//! Ferramentas `drive_*` (Google Drive API v3). Leitura, download/exportação,
//! criação, upload (multipart/resumable), atualização, lixeira e compartilhamento.
//! Downloads caem em `<kb>/inbox/drive/`; uploads só saem de dentro da KB.
//! Ver `docs/28-google-drive.md`.

mod ops;
pub mod query;

use crate::GoogleModule;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::tools::{arg_opt_str, truncate};
use will_core::{
    Module, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};
use will_storage::google::GoogleAccount;

const FAMILY: &str = "drive";
/// Tamanho do cache id → nome usado nos resumos de aprovação.
const NAME_CACHE_MAX: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    List,
    Search,
    Get,
    Download,
    CreateFolder,
    Upload,
    CreateFile,
    Update,
    Trash,
    ListPermissions,
    Share,
    Unshare,
    ListSharedDrives,
}

impl Kind {
    const ALL: [Kind; 13] = [
        Kind::List,
        Kind::Search,
        Kind::Get,
        Kind::Download,
        Kind::CreateFolder,
        Kind::Upload,
        Kind::CreateFile,
        Kind::Update,
        Kind::Trash,
        Kind::ListPermissions,
        Kind::Share,
        Kind::Unshare,
        Kind::ListSharedDrives,
    ];

    fn name(self) -> &'static str {
        match self {
            Kind::List => "drive_list",
            Kind::Search => "drive_search",
            Kind::Get => "drive_get",
            Kind::Download => "drive_download",
            Kind::CreateFolder => "drive_create_folder",
            Kind::Upload => "drive_upload",
            Kind::CreateFile => "drive_create_file",
            Kind::Update => "drive_update",
            Kind::Trash => "drive_trash",
            Kind::ListPermissions => "drive_list_permissions",
            Kind::Share => "drive_share",
            Kind::Unshare => "drive_unshare",
            Kind::ListSharedDrives => "drive_list_shared_drives",
        }
    }
}

pub struct DriveProvider {
    module: Arc<GoogleModule>,
}

impl DriveProvider {
    pub fn new(module: Arc<GoogleModule>) -> Self {
        Self { module }
    }
}

impl ToolProvider for DriveProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        Kind::ALL
            .iter()
            .map(|k| {
                Arc::new(DriveTool {
                    kind: *k,
                    module: self.module.clone(),
                }) as Arc<dyn WillTool>
            })
            .collect()
    }
    fn available(&self) -> bool {
        self.module.config().drive.enabled
            && self
                .module
                .drive_accounts()
                .iter()
                .any(|a| a.status == crate::STATUS_OK)
    }
}

impl GoogleModule {
    /// Contas usáveis pelo Drive (restritas a `drive.accounts` se não vazio).
    pub(crate) fn drive_accounts(&self) -> Vec<GoogleAccount> {
        self.accounts_allowed(&self.config().drive.accounts)
    }

    pub(crate) async fn resolve_drive_account(
        &self,
        arg: Option<&str>,
    ) -> Result<GoogleAccount, ToolError> {
        if self.client.cached_accounts().is_empty() {
            let _ = self.client.reload_accounts().await;
        }
        self.resolve_account(arg, self.drive_accounts(), "Drive")
            .await
    }

    /// Guarda `id → nome` de arquivos vistos, para os resumos de aprovação.
    pub(crate) fn remember_drive_name(&self, file: &Value) {
        let (Some(id), Some(name)) = (
            file.get("id").and_then(|v| v.as_str()),
            file.get("name").and_then(|v| v.as_str()),
        ) else {
            return;
        };
        let mut names = self.drive_names.write();
        if names.len() >= NAME_CACHE_MAX {
            names.clear();
        }
        names.insert(id.to_string(), name.to_string());
    }

    fn drive_name(&self, id: Option<&str>) -> String {
        let Some(id) = id else {
            return "?".into();
        };
        match self.drive_names.read().get(id) {
            Some(n) => format!("'{n}'"),
            None => id.to_string(),
        }
    }
}

pub struct DriveTool {
    kind: Kind,
    module: Arc<GoogleModule>,
}

fn account_prop() -> Value {
    json!({"type": "string", "description": "Conta (e-mail ou id); opcional quando há uma só"})
}

fn file_id_prop(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn page_props(props: &mut Value) {
    props["max_results"] =
        json!({"type": "integer", "minimum": 1, "maximum": ops::MAX_PAGE, "default": 50});
    props["page_token"] = json!({"type": "string"});
    props["order_by"] = json!({"type": "string", "description": "Ex.: 'modifiedTime desc', 'name', 'folder,name' (padrão: pastas primeiro, mais recentes)"});
    props["drive_id"] = json!({"type": "string", "description": "Restringe a um drive compartilhado (drive_list_shared_drives)"});
}

#[async_trait::async_trait]
impl WillTool for DriveTool {
    fn spec(&self) -> ToolSpec {
        let (description, input_schema, default_policy) = match self.kind {
            Kind::List => {
                let mut props = json!({
                    "account": account_prop(),
                    "folder_id": {"type": "string", "description": "Pasta a listar (padrão: raiz 'root' do Meu Drive)"},
                    "mime_type": {"type": "string", "description": "Filtra por tipo (MIME ou alias: folder, doc, sheet, slides, pdf, image, video)"},
                    "trashed": {"type": "boolean", "default": false}
                });
                page_props(&mut props);
                (
                    "Lista o conteúdo de uma pasta do Drive (raiz por padrão): id, nome, tipo, tamanho, dono, modificado, link. Inclui drives compartilhados.",
                    json!({"type": "object", "properties": props}),
                    ToolPolicy::Auto,
                )
            }
            Kind::Search => {
                let mut props = json!({
                    "account": account_prop(),
                    "q": {"type": "string", "description": "Consulta na sintaxe do Drive (ex.: \"name contains 'ata' and mimeType = 'application/pdf'\")"},
                    "name": {"type": "string", "description": "Atalho: nome contém"},
                    "full_text": {"type": "string", "description": "Atalho: conteúdo contém (texto indexado)"},
                    "mime_type": {"type": "string", "description": "MIME ou alias: folder, doc, sheet, slides, pdf, image, video, audio"},
                    "modified_after": {"type": "string", "description": "YYYY-MM-DD ou RFC 3339"},
                    "modified_before": {"type": "string"},
                    "parent": {"type": "string", "description": "Id da pasta pai ('root' = raiz)"},
                    "owner": {"type": "string", "description": "E-mail do dono ('me' = eu)"},
                    "shared_with_me": {"type": "boolean"},
                    "starred": {"type": "boolean"},
                    "trashed": {"type": "boolean", "default": false}
                });
                page_props(&mut props);
                (
                    "Busca arquivos no Drive por nome, conteúdo, tipo, data, pasta, dono ou 'compartilhados comigo'. Combine `q` livre com os atalhos.",
                    json!({"type": "object", "properties": props}),
                    ToolPolicy::Auto,
                )
            }
            Kind::Get => (
                "Metadados completos de um arquivo/pasta: tipo, tamanho, dono, datas, pais, link, se está compartilhado e com quem.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "include_permissions": {"type": "boolean", "default": true}
                }, "required": ["file_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::Download => (
                "Baixa um arquivo para a pasta de downloads da base de conhecimento (inbox/drive). Docs/Planilhas/Apresentações são exportados (padrão: markdown / csv / pdf; `export_format` muda).",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "export_format": {"type": "string", "description": "Para arquivos Google: md, txt, html, pdf, docx, xlsx, pptx, csv, tsv, odt, png… (ou MIME)"},
                    "save_as": {"type": "string", "description": "Nome do arquivo local (opcional)"}
                }, "required": ["file_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::CreateFolder => (
                "Cria uma pasta (na raiz ou dentro de `parent_id`).",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "name": {"type": "string"},
                    "parent_id": {"type": "string", "description": "Pasta pai (padrão: raiz)"},
                    "description": {"type": "string"}
                }, "required": ["name"]}),
                ToolPolicy::Auto,
            ),
            Kind::Upload => (
                "Envia um arquivo da base de conhecimento para o Drive (multipart até 5 MB, resumable acima). `convert_to: doc|sheet|slides` converte para o formato Google. Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "path": {"type": "string", "description": "Caminho do arquivo dentro da base de conhecimento"},
                    "name": {"type": "string", "description": "Nome no Drive (padrão: nome do arquivo)"},
                    "parent_id": {"type": "string", "description": "Pasta destino (padrão: raiz)"},
                    "mime_type": {"type": "string", "description": "Tipo do conteúdo (padrão: pela extensão)"},
                    "convert_to": {"type": "string", "enum": ["doc", "sheet", "slides"], "description": "Converter para Google Docs/Sheets/Slides"},
                    "description": {"type": "string"}
                }, "required": ["path"]}),
                ToolPolicy::Confirm,
            ),
            Kind::CreateFile => (
                "Cria um arquivo no Drive a partir de texto (ex.: .md, .txt, .csv, .json). `convert_to: doc|sheet` cria um Google Doc/Planilha. Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "name": {"type": "string", "description": "Nome com extensão (ex.: notas.md)"},
                    "content": {"type": "string", "description": "Conteúdo em texto"},
                    "parent_id": {"type": "string", "description": "Pasta destino (padrão: raiz)"},
                    "mime_type": {"type": "string", "description": "Tipo do conteúdo (padrão: pela extensão; texto puro)"},
                    "convert_to": {"type": "string", "enum": ["doc", "sheet"]},
                    "description": {"type": "string"}
                }, "required": ["name", "content"]}),
                ToolPolicy::Confirm,
            ),
            Kind::Update => (
                "Altera um arquivo: renomear, descrição, favorito, mover (add_parents/remove_parents) e/ou substituir o conteúdo por um arquivo da base de conhecimento. Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "starred": {"type": "boolean"},
                    "add_parents": {"type": "array", "items": {"type": "string"}, "description": "Pastas a adicionar (mover = add + remove)"},
                    "remove_parents": {"type": "array", "items": {"type": "string"}},
                    "content_path": {"type": "string", "description": "Arquivo da base de conhecimento cujo conteúdo substitui o atual"},
                    "mime_type": {"type": "string", "description": "Tipo do novo conteúdo (padrão: pela extensão)"}
                }, "required": ["file_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::Trash => (
                "Manda um arquivo para a lixeira (`restore: true` tira da lixeira; `permanent: true` exclui definitivamente, se permitido). Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "restore": {"type": "boolean", "default": false},
                    "permanent": {"type": "boolean", "default": false}
                }, "required": ["file_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::ListPermissions => (
                "Lista quem tem acesso a um arquivo (tipo, papel, e-mail/domínio, id da permissão).",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo")
                }, "required": ["file_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::Share => (
                "Compartilha um arquivo com uma pessoa/grupo (`email`), um domínio ou 'qualquer um com o link' (`type: anyone`), com papel reader/commenter/writer. Dá acesso a terceiros: só com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "email": {"type": "string", "description": "Pessoa ou grupo"},
                    "domain": {"type": "string"},
                    "type": {"type": "string", "enum": ["user", "group", "domain", "anyone"], "description": "Padrão: user se há email, domain se há domain"},
                    "role": {"type": "string", "enum": ["reader", "commenter", "writer"], "default": "reader"},
                    "notify": {"type": "boolean", "default": true, "description": "Enviar e-mail de aviso (user/group)"},
                    "message": {"type": "string", "description": "Mensagem no e-mail de aviso"},
                    "allow_discovery": {"type": "boolean", "default": false, "description": "type anyone: aparecer em buscas públicas"}
                }, "required": ["file_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::Unshare => (
                "Remove uma permissão de acesso (id vindo de drive_list_permissions/drive_get).",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "file_id": file_id_prop("Id do arquivo"),
                    "permission_id": {"type": "string"}
                }, "required": ["file_id", "permission_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::ListSharedDrives => (
                "Lista os drives compartilhados (Workspace) a que a conta tem acesso.",
                json!({"type": "object", "properties": {"account": account_prop()}}),
                ToolPolicy::Auto,
            ),
        };
        ToolSpec {
            name: self.kind.name().to_string(),
            description: description.to_string(),
            input_schema,
            requires: vec![Module::Google, Module::Drive],
            default_policy,
            family: FAMILY.to_string(),
        }
    }

    fn approval_summary(&self, args: &Value) -> String {
        let m = &self.module;
        let file = |k: &str| m.drive_name(arg_opt_str(args, k));
        let dest = || match arg_opt_str(args, "parent_id") {
            Some(p) => format!(" na pasta {}", m.drive_name(Some(p))),
            None => " na raiz do Drive".to_string(),
        };
        match self.kind {
            Kind::Upload => format!(
                "Enviar o arquivo '{}' para o Drive{}{}",
                arg_opt_str(args, "path").unwrap_or("?"),
                dest(),
                match arg_opt_str(args, "convert_to") {
                    Some(c) => format!(" (converter para Google {c})"),
                    None => String::new(),
                }
            ),
            Kind::CreateFile => format!(
                "Criar o arquivo '{}'{} ({} bytes)",
                arg_opt_str(args, "name").unwrap_or("?"),
                dest(),
                arg_opt_str(args, "content").map(|c| c.len()).unwrap_or(0)
            ),
            Kind::Update => {
                let mut what = Vec::new();
                if let Some(n) = arg_opt_str(args, "name") {
                    what.push(format!("renomear para '{n}'"));
                }
                if args.get("add_parents").is_some() || args.get("remove_parents").is_some() {
                    what.push("mover de pasta".to_string());
                }
                if arg_opt_str(args, "content_path").is_some() {
                    what.push(format!(
                        "substituir o conteúdo por '{}'",
                        arg_opt_str(args, "content_path").unwrap_or("?")
                    ));
                }
                if args.get("description").is_some() {
                    what.push("alterar a descrição".to_string());
                }
                if args.get("starred").is_some() {
                    what.push("alterar favorito".to_string());
                }
                if what.is_empty() {
                    what.push("alterar".to_string());
                }
                format!("Arquivo {}: {}", file("file_id"), what.join(", "))
            }
            Kind::Trash => {
                if args.get("permanent").and_then(|v| v.as_bool()) == Some(true) {
                    format!("EXCLUIR DEFINITIVAMENTE o arquivo {}", file("file_id"))
                } else if args.get("restore").and_then(|v| v.as_bool()) == Some(true) {
                    format!("Tirar o arquivo {} da lixeira", file("file_id"))
                } else {
                    format!("Mandar o arquivo {} para a lixeira", file("file_id"))
                }
            }
            Kind::Share => {
                let who = if let Some(e) = arg_opt_str(args, "email") {
                    e.to_string()
                } else if let Some(d) = arg_opt_str(args, "domain") {
                    format!("todo o domínio {d}")
                } else if arg_opt_str(args, "type") == Some("anyone") {
                    "QUALQUER PESSOA com o link".to_string()
                } else {
                    "?".to_string()
                };
                format!(
                    "Compartilhar o arquivo {} com {} como {}",
                    file("file_id"),
                    who,
                    arg_opt_str(args, "role").unwrap_or("reader")
                )
            }
            Kind::Unshare => format!(
                "Remover a permissão {} do arquivo {}",
                arg_opt_str(args, "permission_id").unwrap_or("?"),
                file("file_id")
            ),
            _ => format!("{} {}", self.kind.name(), truncate(&args.to_string(), 200)),
        }
    }

    async fn call(&self, ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        crate::traced_op("drive", self.kind.name(), &ctx, async {
            if !m.config().drive.enabled {
                return Err(ToolError::Unavailable("módulo Drive desligado".into()));
            }
            let acc = m
                .resolve_drive_account(arg_opt_str(&args, "account"))
                .await?;
            self.dispatch(m, &acc, &args).await
        })
        .await
    }
}

impl DriveTool {
    async fn dispatch(
        &self,
        m: &GoogleModule,
        acc: &GoogleAccount,
        args: &Value,
    ) -> Result<ToolOutput, ToolError> {
        let acc = acc.clone();
        let args = args.clone();
        match self.kind {
            Kind::List => ops::list(m, &acc, &args).await,
            Kind::Search => ops::search(m, &acc, &args).await,
            Kind::Get => ops::get(m, &acc, &args).await,
            Kind::Download => ops::download(m, &acc, &args).await,
            Kind::CreateFolder => ops::create_folder(m, &acc, &args).await,
            Kind::Upload => ops::upload(m, &acc, &args).await,
            Kind::CreateFile => ops::create_file(m, &acc, &args).await,
            Kind::Update => ops::update(m, &acc, &args).await,
            Kind::Trash => ops::trash(m, &acc, &args).await,
            Kind::ListPermissions => ops::list_permissions(m, &acc, &args).await,
            Kind::Share => ops::share(m, &acc, &args).await,
            Kind::Unshare => ops::unshare(m, &acc, &args).await,
            Kind::ListSharedDrives => ops::list_shared_drives(m, &acc, &args).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_and_policies() {
        assert_eq!(Kind::ALL.len(), 13);
        for k in Kind::ALL {
            assert!(k.name().starts_with("drive_"));
        }
        let confirm = [
            Kind::Upload,
            Kind::CreateFile,
            Kind::Update,
            Kind::Trash,
            Kind::Share,
            Kind::Unshare,
        ];
        for k in confirm {
            assert!(k.name().starts_with("drive_"));
        }
    }
}
