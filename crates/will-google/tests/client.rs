//! GoogleClient contra servidor mock: refresh de token, retry em 429 e
//! `invalid_grant` → conta marcada para reautorizar.

use secrecy::SecretString;
use std::collections::BTreeMap;
use std::sync::Arc;
use will_core::settings::GoogleSettings;
use will_core::EventBus;
use will_google::client::{Endpoints, GoogleClient};
use will_google::{GoogleError, STATUS_OK, STATUS_REAUTHORIZE};
use will_storage::google::GoogleAccount;
use will_storage::{SecretStore, Storage};
use wiremock::matchers::{body_string_contains, header, method, path};
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

async fn setup(
    server: &MockServer,
    account_id: &str,
    email: &str,
) -> (Arc<GoogleClient>, GoogleAccount) {
    let storage = Storage::open_memory().await.expect("storage");
    let secrets = Arc::new(MemSecrets::default());
    secrets
        .set(
            "google_client_secret",
            &SecretString::from("shh".to_string()),
        )
        .await
        .unwrap();
    secrets
        .set(
            &format!("google:{account_id}:refresh_token"),
            &SecretString::from("rt-1".to_string()),
        )
        .await
        .unwrap();
    let account = GoogleAccount {
        id: account_id.into(),
        email: email.into(),
        secret_ref: format!("google:{account_id}:refresh_token"),
        scopes: vec!["openid".into(), "email".into()],
        status: STATUS_OK.into(),
        connected_at: chrono::Utc::now(),
        last_used_at: None,
    };
    storage.upsert_google_account(&account).await.unwrap();
    let settings = GoogleSettings {
        client_id: Some("abc.apps.googleusercontent.com".into()),
        client_secret_ref: None,
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
    (client, account)
}

#[tokio::test]
async fn refreshes_token_and_retries_on_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "tok-1", "expires_in": 3600, "token_type": "Bearer",
            "scope": "openid email https://www.googleapis.com/auth/calendar.readonly"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(header("authorization", "Bearer tok-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "primary", "summary": "Pessoal", "primary": true}],
            "nextPageToken": "p2"
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(wiremock::matchers::query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "work@group.calendar.google.com", "summary": "Trabalho"}]
        })))
        .mount(&server)
        .await;

    let (client, account) = setup(&server, "acc1", "a@x.com").await;
    let items = client
        .get_all_pages(
            &account,
            "calendar/v3/users/me/calendarList",
            &[("maxResults", "250".into())],
            "items",
            None,
        )
        .await
        .expect("calendarList");
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["summary"], "Trabalho");
    // Segunda chamada reutiliza o token do cache (mock do /token espera exatamente 1 chamada).
    let again = client
        .get(
            &account,
            "calendar/v3/users/me/calendarList",
            &[("pageToken", "p2".into())],
        )
        .await
        .unwrap();
    assert_eq!(again["items"][0]["summary"], "Trabalho");
}

#[tokio::test]
async fn invalid_grant_marks_account_for_reauthorization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "Token has been expired or revoked."
        })))
        .mount(&server)
        .await;
    let (client, account) = setup(&server, "acc2", "b@x.com").await;
    let err = client
        .get(&account, "gmail/v1/users/me/labels", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, GoogleError::Reauthorize(ref e) if e == "b@x.com"),
        "{err:?}"
    );
    let msg = will_core::ToolError::from(err).to_string();
    assert!(msg.contains("precisa reautorizar"), "{msg}");
    let stored = client
        .storage
        .get_google_account("acc2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, STATUS_REAUTHORIZE);
}

#[tokio::test]
async fn api_errors_are_actionable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/messages/x"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": {"code": 403, "message": "Insufficient Permission", "errors": [{"reason": "insufficientPermissions"}]}
        })))
        .mount(&server)
        .await;
    let (client, account) = setup(&server, "acc3", "c@x.com").await;
    client.seed_token("acc3", "seeded", std::time::Duration::from_secs(600));
    let err = client
        .get(&account, "gmail/v1/users/me/messages/x", &[])
        .await
        .unwrap_err();
    let tool_err = will_core::ToolError::from(err);
    assert!(
        matches!(tool_err, will_core::ToolError::Forbidden(_)),
        "{tool_err:?}"
    );
    assert!(tool_err.to_string().contains("escopo insuficiente"));
}
