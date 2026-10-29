//! Ferramentas `drive_*` contra um servidor mock: listar, baixar (binário e
//! exportação), limites, upload multipart/resumable, compartilhar e lixeira.

use secrecy::SecretString;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use will_core::settings::{DriveSettings, GoogleSettings};
use will_core::{EventBus, ToolContext, ToolError, WillTool};
use will_google::client::{Endpoints, GoogleClient};
use will_google::{GoogleConfig, GoogleModule, STATUS_OK};
use will_storage::google::GoogleAccount;
use will_storage::{SecretStore, Storage};
use wiremock::matchers::{
    body_string_contains, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct MemSecrets(tokio::sync::Mutex<BTreeMap<String, String>>);

#[async_trait::async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
        Ok(self
            .0
            .lock()
            .await
            .get(key)
            .map(|v| SecretString::from(v.clone())))
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
        use secrecy::ExposeSecret;
        self.0
            .lock()
            .await
            .insert(key.to_string(), value.expose_secret().to_string());
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        self.0.lock().await.remove(key);
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "mem"
    }
}

struct Harness {
    module: Arc<GoogleModule>,
    kb: tempfile::TempDir,
    _server: MockServer,
}

async fn setup(server: MockServer, drive: DriveSettings) -> Harness {
    let storage = Storage::open_memory().await.expect("storage");
    let secrets = Arc::new(MemSecrets::default());
    secrets
        .set(
            "google_client_secret",
            &SecretString::from("shh".to_string()),
        )
        .await
        .unwrap();
    let account = GoogleAccount {
        id: "acc1".into(),
        email: "me@x.com".into(),
        secret_ref: String::new(),
        scopes: vec!["https://www.googleapis.com/auth/drive".into()],
        status: STATUS_OK.into(),
        connected_at: chrono::Utc::now(),
        last_used_at: None,
    };
    storage.upsert_google_account(&account).await.unwrap();
    let settings = GoogleSettings {
        client_id: Some("abc.apps.googleusercontent.com".into()),
        client_secret_ref: None,
        redirect_port: None,
    };
    let client = GoogleClient::new(storage, secrets, EventBus::default(), settings).with_endpoints(
        Endpoints {
            api_base: server.uri(),
            auth_url: format!("{}/auth", server.uri()),
            token_url: format!("{}/token", server.uri()),
            revoke_url: format!("{}/revoke", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        },
    );
    let client = Arc::new(client);
    client.reload_accounts().await.unwrap();
    client.seed_token("acc1", "tok-1", std::time::Duration::from_secs(3600));
    let kb = tempfile::tempdir().unwrap();
    let cfg = GoogleConfig {
        drive: DriveSettings {
            enabled: true,
            ..drive
        },
        ..Default::default()
    };
    let module = GoogleModule::with_client(client, cfg, kb.path().to_path_buf());
    Harness {
        module,
        kb,
        _server: server,
    }
}

fn tool(h: &Harness, name: &str) -> Arc<dyn WillTool> {
    h.module
        .drive_provider()
        .tools()
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap_or_else(|| panic!("ferramenta {name}"))
}

async fn call(h: &Harness, name: &str, args: Value) -> Result<Value, ToolError> {
    let ctx = ToolContext {
        thread_id: None,
        origin: None,
    };
    tool(h, name)
        .call(ctx, args)
        .await
        .map(|o| o.structured.unwrap_or(Value::Null))
}

fn file_json(id: &str, name: &str, mime: &str, size: &str) -> Value {
    json!({
        "id": id, "name": name, "mimeType": mime, "size": size,
        "modifiedTime": "2026-10-29T10:00:00Z",
        "owners": [{"displayName": "Eu", "emailAddress": "me@x.com"}],
        "webViewLink": format!("https://drive.google.com/file/d/{id}/view"),
        "capabilities": {"canEdit": true, "canShare": true, "canDownload": true}
    })
}

#[tokio::test]
async fn lists_root_and_normalizes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .and(query_param("supportsAllDrives", "true"))
        .and(query_param("includeItemsFromAllDrives", "true"))
        .and(query_param("q", "'root' in parents and trashed = false"))
        .and(header("authorization", "Bearer tok-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [
                {"id": "p1", "name": "Projetos", "mimeType": "application/vnd.google-apps.folder"},
                file_json("f1", "foto.png", "image/png", "5")
            ],
            "nextPageToken": "n2"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let h = setup(server, DriveSettings::default()).await;
    let out = call(&h, "drive_list", json!({})).await.unwrap();
    assert_eq!(out["count"], 2);
    assert_eq!(out["files"][0]["kind"], "folder");
    assert_eq!(out["files"][1]["size"], 5);
    assert_eq!(out["files"][1]["owner"], "Eu <me@x.com>");
    assert_eq!(out["next_page_token"], "n2");
    // o nome entra no cache usado pelos resumos de aprovação
    let summary = tool(&h, "drive_trash").approval_summary(&json!({"file_id": "f1"}));
    assert!(summary.contains("'foto.png'"), "{summary}");
}

#[tokio::test]
async fn downloads_binary_and_exports_google_doc() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .and(query_param_is_missing("alt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_json(
            "f1",
            "foto.png",
            "image/png",
            "5",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .and(query_param("alt", "media"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(b"hello".to_vec()),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/d1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "d1", "name": "Plano", "mimeType": "application/vnd.google-apps.document"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/d1/export"))
        .and(query_param("mimeType", "text/markdown"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# Plano\n"))
        .expect(1)
        .mount(&server)
        .await;
    let h = setup(server, DriveSettings::default()).await;

    let out = call(&h, "drive_download", json!({"file_id": "f1"}))
        .await
        .unwrap();
    assert_eq!(out["relative_path"], "inbox/drive/foto.png");
    assert_eq!(out["size"], 5);
    assert_eq!(out["exported"], false);
    assert_eq!(
        std::fs::read(h.kb.path().join("inbox/drive/foto.png")).unwrap(),
        b"hello"
    );
    // segundo download não sobrescreve
    let out2 = call(&h, "drive_download", json!({"file_id": "f1"}))
        .await
        .unwrap();
    assert_eq!(out2["relative_path"], "inbox/drive/foto (2).png");

    let out = call(&h, "drive_download", json!({"file_id": "d1"}))
        .await
        .unwrap();
    assert_eq!(out["relative_path"], "inbox/drive/Plano.md");
    assert_eq!(out["exported"], true);
    assert_eq!(out["mime"], "text/markdown");
    assert_eq!(
        std::fs::read_to_string(h.kb.path().join("inbox/drive/Plano.md")).unwrap(),
        "# Plano\n"
    );
}

#[tokio::test]
async fn download_respects_size_limit_before_fetching() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/big"))
        .and(query_param_is_missing("alt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_json(
            "big",
            "video.mp4",
            "video/mp4",
            "3000000",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/big"))
        .and(query_param("alt", "media"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 10]))
        .expect(0)
        .mount(&server)
        .await;
    let h = setup(
        server,
        DriveSettings {
            max_download_mb: 1,
            ..Default::default()
        },
    )
    .await;
    let err = call(&h, "drive_download", json!({"file_id": "big"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err}");
}

#[tokio::test]
async fn uploads_multipart_from_kb_and_rejects_outside() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "multipart"))
        .and(query_param("supportsAllDrives", "true"))
        .and(header("authorization", "Bearer tok-1"))
        .and(body_string_contains("\"name\":\"nota.txt\""))
        .and(body_string_contains("\"parents\":[\"p1\"]"))
        .and(body_string_contains("Content-Type: text/plain"))
        .and(body_string_contains("conteudo da nota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_json(
            "n1",
            "nota.txt",
            "text/plain",
            "16",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let h = setup(server, DriveSettings::default()).await;
    std::fs::create_dir_all(h.kb.path().join("knowledge")).unwrap();
    std::fs::write(h.kb.path().join("knowledge/nota.txt"), "conteudo da nota").unwrap();
    let out = call(
        &h,
        "drive_upload",
        json!({"path": "knowledge/nota.txt", "parent_id": "p1"}),
    )
    .await
    .unwrap();
    assert_eq!(out["id"], "n1");
    assert_eq!(out["upload_method"], "multipart");
    assert_eq!(out["uploaded_bytes"], 16);

    let outside = tempfile::NamedTempFile::new().unwrap();
    let err = call(
        &h,
        "drive_upload",
        json!({"path": outside.path().to_string_lossy()}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Forbidden(_)), "{err}");
}

#[tokio::test]
async fn uploads_large_file_with_resumable_session() {
    let server = MockServer::start().await;
    let session = format!("{}/upload/session-1", server.uri());
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "resumable"))
        .and(header("x-upload-content-type", "application/octet-stream"))
        .and(body_string_contains("\"name\":\"grande.bin\""))
        .respond_with(ResponseTemplate::new(200).insert_header("location", session.as_str()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/session-1"))
        .and(header("content-range", "bytes 0-5242880/5242881"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file_json(
            "g1",
            "grande.bin",
            "application/octet-stream",
            "5242881",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let h = setup(server, DriveSettings::default()).await;
    std::fs::write(
        h.kb.path().join("grande.bin"),
        vec![7u8; 5 * 1024 * 1024 + 1],
    )
    .unwrap();
    let out = call(&h, "drive_upload", json!({"path": "grande.bin"}))
        .await
        .unwrap();
    assert_eq!(out["id"], "g1");
    assert_eq!(out["upload_method"], "resumable");
}

#[tokio::test]
async fn write_toggles_gate_share_and_permanent_delete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/drive/v3/files/f1/permissions"))
        .and(query_param("sendNotificationEmail", "true"))
        .and(query_param("emailMessage", "segue"))
        .and(body_string_contains("\"emailAddress\":\"ana@x.com\""))
        .and(body_string_contains("\"role\":\"writer\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "perm1", "type": "user", "role": "writer", "emailAddress": "ana@x.com"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/f1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "foto.png", "webViewLink": "https://drive.google.com/file/d/f1/view"
        })))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/drive/v3/files/f1"))
        .and(body_string_contains("\"trashed\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "f1", "name": "foto.png", "mimeType": "image/png", "trashed": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/f1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    // compartilhar desligado por padrão
    let h = setup(server, DriveSettings::default()).await;
    let err = call(
        &h,
        "drive_share",
        json!({"file_id": "f1", "email": "ana@x.com"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Unavailable(_)), "{err}");
    // exclusão definitiva desligada por padrão; lixeira funciona
    let err = call(
        &h,
        "drive_trash",
        json!({"file_id": "f1", "permanent": true}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Unavailable(_)), "{err}");
    let out = call(&h, "drive_trash", json!({"file_id": "f1"}))
        .await
        .unwrap();
    assert_eq!(out["trashed"], true);

    // com o toggle ligado, compartilha e devolve o link
    h.module.update_config(GoogleConfig {
        drive: DriveSettings {
            enabled: true,
            allow_share: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let out = call(
        &h,
        "drive_share",
        json!({"file_id": "f1", "email": "ana@x.com", "role": "writer", "message": "segue"}),
    )
    .await
    .unwrap();
    assert_eq!(out["permission"]["email"], "ana@x.com");
    assert_eq!(out["permission"]["role"], "writer");
    assert_eq!(
        out["web_view_link"],
        "https://drive.google.com/file/d/f1/view"
    );
    let summary =
        tool(&h, "drive_share").approval_summary(&json!({"file_id": "f1", "type": "anyone"}));
    assert!(summary.contains("QUALQUER PESSOA"), "{summary}");
}

#[tokio::test]
async fn disabled_module_refuses_everything() {
    let server = MockServer::start().await;
    let h = setup(server, DriveSettings::default()).await;
    h.module.update_config(GoogleConfig::default());
    let err = call(&h, "drive_list", json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::Unavailable(_)), "{err}");
    assert!(!h.module.drive_provider().available());
}
