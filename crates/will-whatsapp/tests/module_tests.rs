//! Testes do módulo com `MockBackend` (sem rede), uma ou várias contas.

use chrono::{Duration, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use will_core::settings::{WhatsAppAccountRef, WhatsAppSettings};
use will_core::{
    AppPaths, Event, EventBus, ModuleStatus, ToolContext, ToolError, ToolOutput, ToolProvider,
};
use will_storage::Storage;
use will_whatsapp::{
    BackendEvent, BackendFactory, ConnState, IncomingMessage, MockBackend, WhatsAppDeps,
    WhatsAppModule,
};

const ACC1: &str = "acc1";
const ACC2: &str = "acc2";
const ME1: &str = "5511900000000@s.whatsapp.net";
const ME2: &str = "5511900000001@s.whatsapp.net";

struct Harness {
    module: Arc<WhatsAppModule>,
    mocks: Arc<Mutex<HashMap<String, Arc<MockBackend>>>>,
    events: broadcast::Receiver<Event>,
    tmp: tempfile::TempDir,
}

impl Harness {
    fn kb(&self) -> std::path::PathBuf {
        self.tmp.path().join("kb")
    }

    /// Backend mock da conta `id` (criado pela fábrica).
    fn backend(&self, id: &str) -> Arc<MockBackend> {
        self.mocks
            .lock()
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("backend da conta {id}"))
    }

    /// Backend da conta única `acc1`.
    fn b(&self) -> Arc<MockBackend> {
        self.backend(ACC1)
    }

    fn state(&self, id: &str) -> ConnState {
        self.module
            .account(id)
            .map(|a| a.state())
            .unwrap_or(ConnState::Disconnected)
    }
}

fn settings() -> WhatsAppSettings {
    WhatsAppSettings {
        enabled: true,
        ..Default::default()
    }
}

fn account(id: &str, label: &str, phone: Option<&str>) -> WhatsAppAccountRef {
    WhatsAppAccountRef {
        id: id.into(),
        label: label.into(),
        phone: phone.map(str::to_string),
        enabled: true,
        created_at: Some(Utc::now()),
    }
}

fn paths(tmp: &tempfile::TempDir) -> AppPaths {
    AppPaths {
        data_dir: tmp.path().join("data"),
        logs_dir: tmp.path().join("logs"),
        cache_dir: tmp.path().join("cache"),
        portable_root: None,
    }
}

/// `sessions`: `(account_id, me)` das contas que já têm sessão persistida.
async fn harness(
    mut settings: WhatsAppSettings,
    accounts: Vec<WhatsAppAccountRef>,
    sessions: Vec<(&str, Value)>,
) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let kb = tmp.path().join("kb");
    std::fs::create_dir_all(&kb).unwrap();
    let storage = Storage::open_memory().await.unwrap();
    let bus = EventBus::default();
    let events = bus.subscribe();
    let mocks: Arc<Mutex<HashMap<String, Arc<MockBackend>>>> = Default::default();
    let sessions: HashMap<String, Value> = sessions
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let factory: BackendFactory = {
        let mocks = mocks.clone();
        Arc::new(move |id, dir, tx| {
            assert!(dir.ends_with(id), "store dir por conta: {dir:?}");
            let mut b = MockBackend::new(id, tx);
            if let Some(me) = sessions.get(id) {
                b = b.with_session(me.clone());
            }
            let b = Arc::new(b);
            mocks.lock().insert(id.to_string(), b.clone());
            b
        })
    };
    settings.accounts = accounts;
    let deps = WhatsAppDeps {
        storage,
        paths: paths(&tmp),
        bus,
        kb_path: kb,
    };
    let module = WhatsAppModule::new(deps, settings, factory);
    Harness {
        module,
        mocks,
        events,
        tmp,
    }
}

/// Uma conta `acc1` ("Conta 1"), com ou sem sessão.
async fn harness_with(settings: WhatsAppSettings, session: bool) -> Harness {
    let sessions = if session {
        vec![(
            ACC1,
            json!({"jid": ME1, "phone": "5511900000000", "push_name": "Eu"}),
        )]
    } else {
        vec![]
    };
    harness(settings, vec![account(ACC1, "Conta 1", None)], sessions).await
}

async fn connected_harness(settings: WhatsAppSettings) -> Harness {
    let h = harness_with(settings, true).await;
    h.module.start().await.unwrap();
    wait_for(|| h.module.is_connected()).await;
    h
}

/// Duas contas conectadas: `acc1` "Pessoal" e `acc2` "Trabalho".
async fn two_accounts_harness(settings: WhatsAppSettings) -> Harness {
    let h = harness(
        settings,
        vec![
            account(ACC1, "Pessoal", Some("5511900000000")),
            account(ACC2, "Trabalho", Some("5511900000001")),
        ],
        vec![
            (
                ACC1,
                json!({"jid": ME1, "phone": "5511900000000", "push_name": "Eu"}),
            ),
            (
                ACC2,
                json!({"jid": ME2, "phone": "5511900000001", "push_name": "Eu trabalho"}),
            ),
        ],
    )
    .await;
    h.module.start().await.unwrap();
    wait_for(|| h.module.connected_accounts().len() == 2).await;
    h
}

async fn wait_for(mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed().as_secs() < 5, "timeout aguardando condição");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
}

/// Aguarda o índice (todas as contas) chegar a `n` mensagens.
async fn wait_indexed(h: &Harness, n: i64) {
    let start = Instant::now();
    loop {
        if h.module.status().await["index_total"]["messages"].as_i64() == Some(n) {
            return;
        }
        assert!(
            start.elapsed().as_secs() < 5,
            "timeout aguardando {n} mensagens indexadas"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn call(m: &Arc<WhatsAppModule>, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    let tool = m
        .tools()
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap_or_else(|| panic!("tool {name}"));
    tool.call(
        ToolContext {
            thread_id: None,
            origin: None,
        },
        args,
    )
    .await
}

fn msg(
    id: &str,
    chat: &str,
    sender: &str,
    text: &str,
    from_me: bool,
    minutes_ago: i64,
) -> IncomingMessage {
    IncomingMessage {
        id: id.into(),
        chat_jid: chat.into(),
        sender_jid: sender.into(),
        sender_name: Some(if from_me {
            "Eu".into()
        } else {
            format!("Contato {}", &sender[..4])
        }),
        from_me,
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
        kind: "text".into(),
        text: Some(text.into()),
        media_ref: None,
        quoted_id: None,
        is_group: chat.ends_with("@g.us"),
        sender_phone: None,
        chat_phone: None,
        chat_name: if chat.ends_with("@g.us") {
            Some("Família".into())
        } else {
            None
        },
    }
}

fn contact(account: &str, jid: &str, name: &str) -> will_storage::whatsapp::WaContact {
    will_storage::whatsapp::WaContact {
        account_id: account.into(),
        jid: jid.into(),
        phone: None,
        name: Some(name.into()),
        push_name: None,
    }
}

fn drain(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

fn wa_details(evs: &[Event], wanted: &str) -> Vec<Value> {
    evs.iter()
        .filter_map(|e| match e {
            Event::WhatsApp { state, detail } if state == wanted => {
                Some(serde_json::from_str(detail.as_deref().expect("detail")).expect("detail JSON"))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn pairing_flow_qr_then_connected_creates_account() {
    let mut h = harness(settings(), vec![], vec![]).await;
    h.module.start().await.unwrap();
    assert_eq!(
        h.module.module_status(),
        ModuleStatus::Degraded("nenhuma conta pareada".into())
    );
    assert!(!h.module.available());
    assert!(h.module.account_refs().is_empty());

    let id = h.module.pair(None, None, None).await.unwrap();
    assert_eq!(id.len(), 10);
    wait_for(|| h.state(&id) == ConnState::Pairing("qr".into())).await;
    assert_eq!(h.module.module_status(), ModuleStatus::Starting);
    let st = h.module.status().await;
    assert_eq!(st["backend"], "mock");
    assert_eq!(st["accounts"].as_array().unwrap().len(), 1);
    let a = &st["accounts"][0];
    assert_eq!(a["id"], id);
    assert_eq!(a["label"], "Conta 1");
    assert_eq!(a["state"], "qr");
    assert!(a["last_qr"]["svg"].as_str().unwrap().contains("<svg"));
    assert_eq!(a["last_qr"]["timeout_secs"], 60);
    assert!(h.tmp.path().join("data/whatsapp").join(&id).is_dir());

    let evs = drain(&mut h.events);
    let qr = wa_details(&evs, "qr");
    assert_eq!(qr.len(), 1);
    assert_eq!(qr[0]["account_id"], id);
    assert!(qr[0]["data"].as_str().unwrap().starts_with("mock-qr-"));
    assert!(qr[0]["svg"].as_str().unwrap().contains("</svg>"));
    assert!(qr[0]["at"].is_string());

    h.backend(&id)
        .push(BackendEvent::Connected {
            me: json!({"jid": ME1, "push_name": "Will"}),
        })
        .await;
    wait_for(|| h.module.is_connected()).await;
    let st = h.module.status().await;
    let a = &st["accounts"][0];
    assert_eq!(a["state"], "connected");
    assert_eq!(a["me"]["push_name"], "Will");
    assert_eq!(a["label"], "Will", "rótulo padrão vira push_name");
    assert_eq!(a["phone"], "5511900000000", "telefone vem do JID");
    assert!(a["last_qr"].is_null());
    assert_eq!(h.module.module_status(), ModuleStatus::Running);
    assert!(h.module.available());
    let refs = h.module.account_refs();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, id);
    assert_eq!(refs[0].label, "Will");
    assert!(refs[0].enabled && refs[0].created_at.is_some());
    let evs = drain(&mut h.events);
    let conn = wa_details(&evs, "connected");
    assert_eq!(conn[0]["account_id"], id);
    assert_eq!(conn[0]["me"]["push_name"], "Will");
}

#[tokio::test]
async fn pairing_by_code_with_label() {
    let mut h = harness(settings(), vec![], vec![]).await;
    let id = h
        .module
        .pair(
            None,
            Some("+55 (11) 98888-7777".into()),
            Some("Trabalho".into()),
        )
        .await
        .unwrap();
    wait_for(|| h.state(&id) == ConnState::Pairing("code".into())).await;
    let st = h.module.status().await;
    assert_eq!(st["accounts"][0]["last_pair_code"], "ABCD-1234");
    assert_eq!(st["accounts"][0]["label"], "Trabalho");
    let evs = drain(&mut h.events);
    let code = wa_details(&evs, "code");
    assert_eq!(code[0], json!({"account_id": id, "code": "ABCD-1234"}));
    assert!(h.module.pair(None, Some("12".into()), None).await.is_err());
    assert!(h
        .module
        .pair(Some("nope".into()), None, None)
        .await
        .is_err());
    // segunda conta ganha "Conta 2"; parear de novo a existente mantém o id
    let id2 = h.module.pair(None, None, None).await.unwrap();
    assert_ne!(id, id2);
    assert_eq!(h.module.account(&id2).unwrap().label(), "Conta 2");
    assert_eq!(
        h.module.pair(Some(id.clone()), None, None).await.unwrap(),
        id
    );
    assert_eq!(h.module.account_refs().len(), 2);
    // conectada, o rótulo explícito é preservado
    h.backend(&id)
        .push(BackendEvent::Connected {
            me: json!({"jid": ME1, "push_name": "Will"}),
        })
        .await;
    wait_for(|| h.state(&id) == ConnState::Connected).await;
    assert_eq!(h.module.account(&id).unwrap().label(), "Trabalho");
}

#[tokio::test]
async fn disabled_module_does_not_connect() {
    let h = harness_with(
        WhatsAppSettings {
            enabled: false,
            ..Default::default()
        },
        true,
    )
    .await;
    assert!(h.module.start().await.is_ok());
    assert!(!h.module.is_connected());
    assert_eq!(h.module.module_status(), ModuleStatus::Disabled);
}

#[tokio::test]
async fn tools_unavailable_when_disconnected() {
    let h = harness_with(settings(), false).await;
    let r = call(&h.module, "whatsapp_list_chats", json!({})).await;
    assert!(matches!(r, Err(ToolError::Unavailable(_))));
    let names: Vec<String> = h.module.tools().iter().map(|t| t.spec().name).collect();
    assert_eq!(
        names,
        vec![
            "whatsapp_list_chats",
            "whatsapp_get_messages",
            "whatsapp_search_messages",
            "whatsapp_list_contacts",
            "whatsapp_get_group",
            "whatsapp_download_media",
            "whatsapp_mark_read",
            "whatsapp_send_message",
            "whatsapp_send_media"
        ]
    );
    for t in h.module.tools() {
        let spec = t.spec();
        assert_eq!(spec.family, "whatsapp");
        assert_eq!(spec.requires, vec![will_core::Module::WhatsApp]);
        let confirm = spec.name.starts_with("whatsapp_send_");
        assert_eq!(
            spec.default_policy == will_core::ToolPolicy::Confirm,
            confirm,
            "{}",
            spec.name
        );
        assert!(
            spec.input_schema["properties"]["account"].is_object(),
            "{} sem parâmetro account",
            spec.name
        );
        assert!(!spec.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "account"));
    }
}

#[tokio::test]
async fn incoming_messages_are_indexed_and_searchable() {
    let h = connected_harness(settings()).await;
    let ana = "5511911111111@s.whatsapp.net";
    let bob = "5511922222222@s.whatsapp.net";
    let grp = "120363000000000000@g.us";
    h.b()
        .push(BackendEvent::Message(msg(
            "m1",
            ana,
            ana,
            "Oi, viu o orçamento da reforma?",
            false,
            30,
        )))
        .await;
    h.b()
        .push(BackendEvent::Message(msg(
            "m2",
            ana,
            ana,
            "Preciso de resposta hoje",
            false,
            20,
        )))
        .await;
    h.b()
        .push(BackendEvent::Message(msg(
            "m3",
            bob,
            ME1,
            "Fechado, te mando depois",
            true,
            15,
        )))
        .await;
    h.b()
        .push(BackendEvent::Message(msg(
            "m4",
            grp,
            bob,
            "Alguém traz o bolo?",
            false,
            5,
        )))
        .await;
    h.b()
        .push(BackendEvent::ContactUpdate(
            will_storage::whatsapp::WaContact {
                account_id: "ignorado".into(),
                jid: ana.into(),
                phone: Some("5511911111111".into()),
                name: Some("Ana Lima".into()),
                push_name: None,
            },
        ))
        .await;
    settle().await;

    let out = call(
        &h.module,
        "whatsapp_list_chats",
        json!({"filter": "unread"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["account"], ACC1);
    assert_eq!(v["account_label"], "Eu");
    let chats = v["chats"].as_array().unwrap();
    assert_eq!(chats.len(), 2, "{v}");
    let ana_chat = chats.iter().find(|c| c["id"] == ana).unwrap();
    assert_eq!(ana_chat["unread"], 2);
    assert_eq!(ana_chat["last_message"], "Preciso de resposta hoje");
    let grp_chat = chats.iter().find(|c| c["id"] == grp).unwrap();
    assert_eq!(grp_chat["is_group"], true);
    assert_eq!(grp_chat["name"], "Família");

    let out = call(
        &h.module,
        "whatsapp_list_chats",
        json!({"filter": "groups"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 1);
    let out = call(&h.module, "whatsapp_list_chats", json!({}))
        .await
        .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 3);
    assert!(
        call(&h.module, "whatsapp_list_chats", json!({"filter": "bogus"}))
            .await
            .is_err()
    );

    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "limit": 10}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["message_id"], "m1");
    assert_eq!(msgs[1]["message_id"], "m2");
    // O nome da agenda (ContactUpdate) prevalece sobre o push name no chat.
    assert_eq!(v["chat_name"], "Ana Lima");
    assert!(v["next_before"].is_null());
    assert_eq!(v["account"], ACC1);

    let since = (Utc::now() - Duration::minutes(25)).to_rfc3339();
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "since": since}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 1);
    assert!(call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "since": "ontem"})
    )
    .await
    .is_err());

    let out = call(
        &h.module,
        "whatsapp_search_messages",
        json!({"query": "orçamento"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["results"][0]["message_id"], "m1");
    assert_eq!(v["results"][0]["chat_id"], ana);

    let out = call(
        &h.module,
        "whatsapp_search_messages",
        json!({"query": "bolo", "chat_id": "Família"}),
    )
    .await
    .unwrap();
    assert_eq!(
        out.structured.unwrap()["results"][0]["chat_name"],
        "Família"
    );

    let out = call(&h.module, "whatsapp_list_contacts", json!({"query": "ana"}))
        .await
        .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["contacts"][0]["name"], "Ana Lima");
    assert_eq!(v["contacts"][0]["phone"], "5511911111111");

    let out = call(&h.module, "whatsapp_mark_read", json!({"chat_id": ana}))
        .await
        .unwrap();
    assert_eq!(out.structured.unwrap()["receipt_sent"], false);
    let out = call(
        &h.module,
        "whatsapp_list_chats",
        json!({"filter": "unread"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 1);

    // Recibo "lido" vindo do telefone zera o grupo.
    h.b()
        .push(BackendEvent::Receipt {
            chat_jid: grp.into(),
            message_ids: vec!["m4".into()],
            kind: "read".into(),
        })
        .await;
    settle().await;
    let out = call(
        &h.module,
        "whatsapp_list_chats",
        json!({"filter": "unread"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 0);

    let st = h.module.status().await;
    assert_eq!(st["accounts"][0]["index"]["messages"], 4);
    assert_eq!(st["accounts"][0]["index"]["chats"], 3);
    assert_eq!(st["index_total"]["messages"], 4);
}

#[tokio::test]
async fn get_messages_pagination_and_cap() {
    let h = connected_harness(settings()).await;
    let chat = "5511933333333@s.whatsapp.net";
    for i in 0..250 {
        h.b()
            .push(BackendEvent::Message(msg(
                &format!("p{i:03}"),
                chat,
                chat,
                &format!("mensagem {i}"),
                false,
                300 - i,
            )))
            .await;
    }
    wait_indexed(&h, 250).await;
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": chat, "limit": 1000}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 200);
    let before = v["next_before"].as_str().expect("next_before").to_string();
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": chat, "before": before}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 50);
    assert_eq!(v["messages"][0]["message_id"], "p000");
}

#[tokio::test]
async fn send_message_rate_limit_and_unsaved_numbers() {
    let s = WhatsAppSettings {
        send_rate_limit_count: 2,
        send_rate_limit_minutes: 10,
        allow_unsaved_numbers: false,
        ..settings()
    };
    let h = connected_harness(s.clone()).await;
    let ana = "5511911111111@s.whatsapp.net";
    h.b()
        .push(BackendEvent::Message(msg("m1", ana, ana, "oi", false, 1)))
        .await;
    settle().await;

    // phone não aceito sem a opção
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"phone": "5511944444444", "text": "oi"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Forbidden(_))), "{r:?}");
    // chat_id só com dígitos é tratado como telefone
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": "+55 11 94444-4444", "text": "oi"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Forbidden(_))), "{r:?}");
    // JID desconhecido também é bloqueado
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": "5511955555555@s.whatsapp.net", "text": "oi"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Forbidden(_))), "{r:?}");
    assert!(matches!(
        call(&h.module, "whatsapp_send_message", json!({"text": "oi"})).await,
        Err(ToolError::InvalidArgs(_))
    ));

    // envios dentro do limite
    let out = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "resposta 1", "reply_to": "m1"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["message_id"], "MOCK000001");
    assert_eq!(v["account"], ACC1);
    call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "resposta 2"}),
    )
    .await
    .unwrap();
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "resposta 3"}),
    )
    .await;
    match r {
        Err(ToolError::Failed(e)) => assert!(e.contains("limite de envio"), "{e}"),
        other => panic!("esperava rate limit, veio {other:?}"),
    }
    let sent = h.b().sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].quote.as_deref(), Some("m1"));
    assert_eq!(sent[0].chat_jid, ana);

    // mensagens enviadas entram no índice como from_me
    let out = call(&h.module, "whatsapp_get_messages", json!({"chat_id": ana}))
        .await
        .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 3);
    assert_eq!(v["messages"][2]["from_me"], true);

    // recibo de leitura atualiza o outbox sem erro
    h.b()
        .push(BackendEvent::Receipt {
            chat_jid: ana.into(),
            message_ids: vec!["MOCK000001".into()],
            kind: "read".into(),
        })
        .await;
    settle().await;

    // com a opção ligada, phone passa a ser aceito e resolvido
    h.module
        .update_settings(WhatsAppSettings {
            allow_unsaved_numbers: true,
            send_rate_limit_count: 0,
            ..h.module.settings()
        })
        .await;
    assert!(
        h.module.is_connected(),
        "update_settings não derruba a conta"
    );
    let out = call(
        &h.module,
        "whatsapp_send_message",
        json!({"phone": "+55 11 94444-4444", "text": "novo"}),
    )
    .await
    .unwrap();
    assert_eq!(
        out.structured.unwrap()["chat_id"],
        "5511944444444@s.whatsapp.net"
    );
    h.b().set_phone("5511966666666", None);
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"phone": "5511966666666", "text": "x"}),
    )
    .await;
    match r {
        Err(ToolError::Failed(e)) => assert!(e.contains("não está no WhatsApp"), "{e}"),
        other => panic!("{other:?}"),
    }

    // erro do backend vira ToolError::Failed e registra outbox com erro
    h.b().set_fail_send(true);
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "x"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Failed(_))));
}

#[tokio::test]
async fn name_resolution() {
    let h = connected_harness(WhatsAppSettings {
        allow_unsaved_numbers: false,
        ..settings()
    })
    .await;
    let silva = "5511911111111@s.whatsapp.net";
    let souza = "5511922222222@s.whatsapp.net";
    let mut m1 = msg("m1", silva, silva, "oi", false, 10);
    m1.sender_name = Some("Maria Silva".into());
    let mut m2 = msg("m2", souza, souza, "olá", false, 9);
    m2.sender_name = Some("Maria Souza".into());
    h.b().push(BackendEvent::Message(m1)).await;
    h.b().push(BackendEvent::Message(m2)).await;
    settle().await;

    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": "Maria", "text": "oi"}),
    )
    .await;
    match r {
        Err(ToolError::InvalidArgs(e)) => {
            assert!(e.contains("ambíguo"), "{e}");
            assert!(e.contains(silva) && e.contains(souza), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let out = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": "maria silva", "text": "oi"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["chat_id"], silva);
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": "Souza"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["chat_id"], souza);
    let r = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": "Fulano"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::InvalidArgs(_))), "{r:?}");

    // contato conhecido sem chat ainda: resolve pelo nome do contato
    h.b()
        .push(BackendEvent::ContactUpdate(contact(
            ACC1,
            "5511977777777@s.whatsapp.net",
            "Pedro Rocha",
        )))
        .await;
    settle().await;
    let out = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": "Pedro Rocha", "text": "oi"}),
    )
    .await
    .unwrap();
    assert_eq!(
        out.structured.unwrap()["chat_id"],
        "5511977777777@s.whatsapp.net"
    );
}

#[tokio::test]
async fn approval_summaries() {
    let h = harness_with(settings(), false).await;
    let tool = h
        .module
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "whatsapp_send_message")
        .unwrap();
    let long = "x".repeat(300);
    let s = tool.approval_summary(&json!({"chat_id": "Ana Lima", "text": long}));
    assert!(
        s.starts_with("Enviar WhatsApp (conta Conta 1) para Ana Lima: xxx"),
        "{s}"
    );
    assert!(s.chars().count() < 260);
    let s = tool.approval_summary(&json!({"phone": "5511", "text": "oi"}));
    assert_eq!(s, "Enviar WhatsApp (conta Conta 1) para 5511: oi");
    let s = tool.approval_summary(&json!({"account": "acc1", "phone": "5511", "text": "oi"}));
    assert_eq!(s, "Enviar WhatsApp (conta Conta 1) para 5511: oi");
    let tool = h
        .module
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "whatsapp_send_media")
        .unwrap();
    let s = tool.approval_summary(&json!({"chat_id": "g", "path": "a.pdf"}));
    assert!(s.contains("a.pdf") && s.contains("(conta Conta 1)"), "{s}");

    // com duas contas e sem `account`, o resumo não inventa conta
    let h = two_accounts_harness(settings()).await;
    let tool = h
        .module
        .tools()
        .into_iter()
        .find(|t| t.spec().name == "whatsapp_send_message")
        .unwrap();
    assert_eq!(
        tool.approval_summary(&json!({"phone": "5511", "text": "oi"})),
        "Enviar WhatsApp para 5511: oi"
    );
    assert_eq!(
        tool.approval_summary(&json!({"account": "trabalho", "phone": "5511", "text": "oi"})),
        "Enviar WhatsApp (conta Trabalho) para 5511: oi"
    );
}

#[tokio::test]
async fn media_download_and_send_media_path_rules() {
    let h = connected_harness(WhatsAppSettings {
        allow_unsaved_numbers: true,
        ..settings()
    })
    .await;
    let chat = "5511911111111@s.whatsapp.net";
    let mut m = msg("img1", chat, chat, "", false, 1);
    m.kind = "image".into();
    m.text = None;
    m.media_ref = Some("{\"t\":\"image\"}".into());
    h.b().push(BackendEvent::Message(m)).await;
    settle().await;

    let out = call(
        &h.module,
        "whatsapp_download_media",
        json!({"message_id": "img1"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    assert!(path.starts_with(h.kb().join("inbox/whatsapp")), "{path:?}");
    assert!(path.is_file());
    assert_eq!(v["size"], 10);
    // segunda chamada reutiliza o arquivo
    let out2 = call(
        &h.module,
        "whatsapp_download_media",
        json!({"message_id": "img1"}),
    )
    .await
    .unwrap();
    assert_eq!(out2.structured.unwrap()["path"], v["path"]);
    assert!(matches!(
        call(
            &h.module,
            "whatsapp_download_media",
            json!({"message_id": "nope"})
        )
        .await,
        Err(ToolError::InvalidArgs(_))
    ));

    // send_media: dentro da KB ok; fora, proibido
    let inside = h.kb().join("docs");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::write(inside.join("relatorio.pdf"), b"%PDF").unwrap();
    let outside = h.tmp.path().join("segredo.txt");
    std::fs::write(&outside, b"x").unwrap();
    let out = call(
        &h.module,
        "whatsapp_send_media",
        json!({"chat_id": chat, "path": "docs/relatorio.pdf", "caption": "segue"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["kind"], "document");
    assert_eq!(v["mime"], "application/pdf");
    let r = call(
        &h.module,
        "whatsapp_send_media",
        json!({"chat_id": chat, "path": outside.to_string_lossy()}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Forbidden(_))), "{r:?}");
    let r = call(
        &h.module,
        "whatsapp_send_media",
        json!({"chat_id": chat, "path": "docs/../../segredo.txt"}),
    )
    .await;
    assert!(
        matches!(
            r,
            Err(ToolError::Forbidden(_)) | Err(ToolError::InvalidArgs(_))
        ),
        "{r:?}"
    );
    let sent = h.b().sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].caption.as_deref(), Some("segue"));
}

#[tokio::test]
async fn group_metadata_from_backend_and_index() {
    let h = connected_harness(settings()).await;
    let grp = "120363000000000000@g.us";
    h.b().set_group(grp, json!({"jid": grp, "subject": "Família", "description": "avisos", "participants": ["a@s.whatsapp.net", "b@s.whatsapp.net"], "admins": ["a@s.whatsapp.net"]}));
    h.b()
        .push(BackendEvent::Message(msg(
            "g1",
            grp,
            "a@s.whatsapp.net",
            "oi",
            false,
            1,
        )))
        .await;
    h.b()
        .push(BackendEvent::GroupUpdate {
            jid: grp.into(),
            subject: None,
            participants: vec![],
            admins: vec![],
            description: None,
        })
        .await;
    settle().await;
    let out = call(
        &h.module,
        "whatsapp_get_group",
        json!({"chat_id": "Família"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["participants"].as_array().unwrap().len(), 2);
    assert_eq!(v["admins"][0], "a@s.whatsapp.net");
    assert_eq!(v["account"], ACC1);
    assert!(matches!(
        call(
            &h.module,
            "whatsapp_get_group",
            json!({"chat_id": "a@s.whatsapp.net"})
        )
        .await,
        Err(ToolError::InvalidArgs(_))
    ));

    h.b()
        .push(BackendEvent::ChatArchived {
            jid: grp.into(),
            archived: true,
        })
        .await;
    settle().await;
    let out = call(
        &h.module,
        "whatsapp_list_chats",
        json!({"filter": "archived"}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["count"], 1);
}

#[tokio::test]
async fn logout_and_unlink() {
    let mut h = connected_harness(settings()).await;
    let chat = "5511911111111@s.whatsapp.net";
    h.b()
        .push(BackendEvent::Message(msg("m1", chat, chat, "oi", false, 1)))
        .await;
    settle().await;
    drain(&mut h.events);

    h.b().push(BackendEvent::LoggedOut).await;
    wait_for(|| !h.module.is_connected()).await;
    assert_eq!(
        h.module.module_status(),
        ModuleStatus::Degraded("SessionRevoked".into())
    );
    let evs = drain(&mut h.events);
    let lo = wa_details(&evs, "logged_out");
    assert_eq!(lo[0]["account_id"], ACC1);
    assert_eq!(lo[0]["reason"], "revogado no telefone");
    assert!(evs.iter().any(|e| matches!(e, Event::Notification { .. })));
    assert!(matches!(
        call(&h.module, "whatsapp_list_chats", json!({})).await,
        Err(ToolError::Unavailable(_))
    ));

    assert!(h.module.unlink("nope", false, false).await.is_err());
    h.module.unlink(ACC1, true, false).await.unwrap();
    let st = h.module.status().await;
    assert_eq!(st["accounts"][0]["state"], "disconnected");
    assert_eq!(st["accounts"][0]["index"]["messages"], 0);
    assert_eq!(st["accounts"][0]["has_session"], false);
    assert_eq!(
        h.module.account_refs().len(),
        1,
        "sem remove, a conta continua"
    );
    let evs = drain(&mut h.events);
    assert_eq!(
        wa_details(&evs, "logged_out")[0]["reason"],
        "desvinculado pelo usuário"
    );

    h.module.stop().await;
    assert_eq!(h.module.module_status(), ModuleStatus::Stopped);
}

#[tokio::test]
async fn disconnect_then_reconnect() {
    let mut h = connected_harness(settings()).await;
    drain(&mut h.events);
    h.b()
        .push(BackendEvent::Disconnected {
            reason: "sem rede".into(),
        })
        .await;
    wait_for(|| !h.module.is_connected()).await;
    assert_eq!(
        h.module.module_status(),
        ModuleStatus::Degraded("sem rede".into())
    );
    let evs = drain(&mut h.events);
    assert_eq!(
        wa_details(&evs, "disconnected")[0],
        json!({"account_id": ACC1, "reason": "sem rede"})
    );
    // backoff de 1 s → mock com sessão reconecta sozinho
    let start = Instant::now();
    wait_for(|| h.module.is_connected()).await;
    assert!(start.elapsed().as_millis() >= 500);
    h.module.reconnect(ACC1).await.unwrap();
    wait_for(|| h.module.is_connected()).await;
    assert!(h.module.reconnect("nope").await.is_err());
}

#[tokio::test]
async fn new_default_without_real_feature_reports_unavailable() {
    if cfg!(feature = "real") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open_memory().await.unwrap();
    let deps = WhatsAppDeps {
        storage,
        paths: paths(&tmp),
        bus: EventBus::default(),
        kb_path: tmp.path().to_path_buf(),
    };
    let m = WhatsAppModule::new_default(deps, settings());
    let err = m.pair(None, None, None).await.unwrap_err();
    assert!(err.contains("indisponível"), "{err}");
    let st = m.status().await;
    assert_eq!(st["backend"], "mock");
    assert_eq!(st["accounts"][0]["state"], "degraded");
}

// ----- múltiplas contas -----

#[tokio::test]
async fn two_accounts_list_chats_aggregates_and_tags_account() {
    let h = two_accounts_harness(settings()).await;
    let ana = "5511911111111@s.whatsapp.net";
    let cli = "5511922222222@s.whatsapp.net";
    h.backend(ACC1)
        .push(BackendEvent::Message(msg(
            "m1",
            ana,
            ana,
            "oi da Ana",
            false,
            10,
        )))
        .await;
    h.backend(ACC2)
        .push(BackendEvent::Message(msg(
            "m2",
            cli,
            cli,
            "proposta comercial",
            false,
            5,
        )))
        .await;
    h.backend(ACC2)
        .push(BackendEvent::ContactUpdate(contact(ACC2, cli, "Cliente X")))
        .await;
    settle().await;

    let out = call(&h.module, "whatsapp_list_chats", json!({}))
        .await
        .unwrap();
    let v = out.structured.unwrap();
    assert!(v["account"].is_null());
    assert_eq!(v["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(v["count"], 2, "{v}");
    let chats = v["chats"].as_array().unwrap();
    // ordenado por última mensagem: a do acc2 é mais recente
    assert_eq!(chats[0]["id"], cli);
    assert_eq!(chats[0]["account"], ACC2);
    assert_eq!(chats[0]["account_label"], "Trabalho");
    assert_eq!(chats[1]["account"], ACC1);
    assert_eq!(chats[1]["account_label"], "Pessoal");

    let out = call(
        &h.module,
        "whatsapp_search_messages",
        json!({"query": "proposta"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["results"][0]["account"], ACC2);

    let out = call(&h.module, "whatsapp_list_contacts", json!({}))
        .await
        .unwrap();
    let v = out.structured.unwrap();
    let contacts = v["contacts"].as_array().unwrap();
    assert!(
        contacts
            .iter()
            .any(|c| c["account"] == ACC2 && c["name"] == "Cliente X"),
        "{v}"
    );
    assert!(contacts.iter().any(|c| c["account"] == ACC1), "{v}");

    // com `account`, filtra
    let out = call(&h.module, "whatsapp_list_chats", json!({"account": ACC1}))
        .await
        .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["account"], ACC1);
    assert_eq!(v["chats"][0]["id"], ana);
}

#[tokio::test]
async fn two_accounts_get_messages_requires_account_and_resolves_by_label_or_phone() {
    let h = two_accounts_harness(settings()).await;
    let ana = "5511911111111@s.whatsapp.net";
    h.backend(ACC1)
        .push(BackendEvent::Message(msg("m1", ana, ana, "oi", false, 10)))
        .await;
    h.backend(ACC2)
        .push(BackendEvent::Message(msg(
            "m2",
            ana,
            ana,
            "oi (trabalho)",
            false,
            5,
        )))
        .await;
    settle().await;

    // (b) sem account → erro listando as contas
    let r = call(&h.module, "whatsapp_get_messages", json!({"chat_id": ana})).await;
    match r {
        Err(ToolError::InvalidArgs(e)) => {
            assert!(e.contains("acc1 — Pessoal — 5511900000000"), "{e}");
            assert!(e.contains("acc2 — Trabalho — 5511900000001"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    for name in [
        "whatsapp_send_message",
        "whatsapp_mark_read",
        "whatsapp_get_group",
        "whatsapp_download_media",
        "whatsapp_send_media",
    ] {
        let r = call(
            &h.module,
            name,
            json!({"chat_id": ana, "text": "x", "message_id": "m1", "path": "x"}),
        )
        .await;
        assert!(
            matches!(r, Err(ToolError::InvalidArgs(ref e)) if e.contains("informe `account`")),
            "{name}: {r:?}"
        );
    }

    // (c) rótulo (case-insensitive), telefone e id
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "account": "trabalho"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["account"], ACC2);
    assert_eq!(v["messages"][0]["message_id"], "m2");
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "account": "+55 11 90000-0000"}),
    )
    .await
    .unwrap();
    let v = out.structured.unwrap();
    assert_eq!(v["account"], ACC1);
    assert_eq!(v["messages"][0]["message_id"], "m1");
    let out = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "account": ACC2}),
    )
    .await
    .unwrap();
    assert_eq!(out.structured.unwrap()["account_label"], "Trabalho");
    let r = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "account": "Inexistente"}),
    )
    .await;
    assert!(
        matches!(r, Err(ToolError::InvalidArgs(ref e)) if e.contains("não encontrada") && e.contains("acc2 — Trabalho")),
        "{r:?}"
    );

    // envio sai pela conta certa e o rate limit é por conta
    h.module
        .update_settings(WhatsAppSettings {
            send_rate_limit_count: 1,
            ..h.module.settings()
        })
        .await;
    call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "a", "account": "Pessoal"}),
    )
    .await
    .unwrap();
    let r = call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "b", "account": "Pessoal"}),
    )
    .await;
    assert!(
        matches!(r, Err(ToolError::Failed(ref e)) if e.contains("limite de envio da conta Pessoal")),
        "{r:?}"
    );
    call(
        &h.module,
        "whatsapp_send_message",
        json!({"chat_id": ana, "text": "c", "account": "Trabalho"}),
    )
    .await
    .unwrap();
    assert_eq!(h.backend(ACC1).sent().len(), 1);
    assert_eq!(h.backend(ACC2).sent().len(), 1);

    // conta desconectada: Unavailable mesmo quando resolvida
    h.backend(ACC2)
        .push(BackendEvent::Disconnected {
            reason: "sem rede".into(),
        })
        .await;
    wait_for(|| !h.module.account(ACC2).unwrap().is_connected()).await;
    let r = call(
        &h.module,
        "whatsapp_get_messages",
        json!({"chat_id": ana, "account": "Trabalho"}),
    )
    .await;
    assert!(matches!(r, Err(ToolError::Unavailable(_))), "{r:?}");
    // só uma conectada → ela é usada sem `account`
    let out = call(&h.module, "whatsapp_get_messages", json!({"chat_id": ana}))
        .await
        .unwrap();
    assert_eq!(out.structured.unwrap()["account"], ACC1);
}

#[tokio::test]
async fn unlink_with_remove_drops_account() {
    let mut h = two_accounts_harness(settings()).await;
    let ana = "5511911111111@s.whatsapp.net";
    h.backend(ACC2)
        .push(BackendEvent::Message(msg("m2", ana, ana, "oi", false, 5)))
        .await;
    settle().await;
    drain(&mut h.events);

    h.module.unlink(ACC2, false, true).await.unwrap();
    let refs = h.module.account_refs();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].id, ACC1);
    assert!(h.module.account(ACC2).is_none());
    assert_eq!(h.module.module_status(), ModuleStatus::Running);
    let evs = drain(&mut h.events);
    assert_eq!(wa_details(&evs, "logged_out")[0]["account_id"], ACC2);
    // o store no disco não é apagado pelo módulo; o índice ficou (clear_index=false)
    assert!(h.tmp.path().join("data/whatsapp").join(ACC2).is_dir());
    let st = h.module.status().await;
    assert_eq!(st["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(
        st["index_total"]["messages"], 1,
        "índice da conta removida ainda conta no total"
    );
    // eventos tardios da conta removida são ignorados sem pânico
    h.backend(ACC2)
        .push(BackendEvent::Message(msg(
            "m3", ana, ana, "tarde", false, 1,
        )))
        .await;
    settle().await;
    assert_eq!(h.module.status().await["index_total"]["messages"], 1);
}

#[tokio::test]
async fn status_json_lists_accounts_with_states() {
    let h = harness(
        settings(),
        vec![
            account(ACC1, "Pessoal", Some("5511900000000")),
            account(ACC2, "Trabalho", None),
        ],
        vec![(
            ACC1,
            json!({"jid": ME1, "phone": "5511900000000", "push_name": "Eu"}),
        )],
    )
    .await;
    h.module.start().await.unwrap();
    wait_for(|| h.module.is_connected()).await;
    let st = h.module.status().await;
    assert_eq!(st["enabled"], true);
    assert_eq!(st["started"], true);
    assert_eq!(st["backend"], "mock");
    assert!(st["media_dir"]
        .as_str()
        .unwrap()
        .ends_with("inbox/whatsapp"));
    let accounts = st["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0]["id"], ACC1);
    assert_eq!(accounts[0]["label"], "Pessoal");
    assert_eq!(accounts[0]["phone"], "5511900000000");
    assert_eq!(accounts[0]["enabled"], true);
    assert_eq!(accounts[0]["state"], "connected");
    assert!(accounts[0]["detail"].is_null());
    assert_eq!(accounts[0]["has_session"], true);
    assert_eq!(
        accounts[0]["index"],
        json!({"chats": 0, "messages": 0, "contacts": 0})
    );
    assert_eq!(accounts[1]["id"], ACC2);
    assert_eq!(accounts[1]["state"], "disconnected");
    assert_eq!(accounts[1]["has_session"], false);
    assert!(accounts[1]["phone"].is_null());
    assert_eq!(
        st["index_total"],
        json!({"chats": 0, "messages": 0, "contacts": 0})
    );
    assert_eq!(h.module.module_status(), ModuleStatus::Running);

    // parear a segunda pela API existente
    h.module.pair(Some(ACC2.into()), None, None).await.unwrap();
    wait_for(|| h.state(ACC2) == ConnState::Pairing("qr".into())).await;
    assert_eq!(h.module.status().await["accounts"][1]["state"], "qr");
}

#[tokio::test]
async fn update_settings_and_set_account_reconcile_accounts() {
    let mut h = two_accounts_harness(settings()).await;
    drain(&mut h.events);

    // desativar via set_account desconecta; o módulo continua Running pela outra conta
    h.module
        .set_account(ACC2, Some("Empresa".into()), Some(false))
        .await
        .unwrap();
    wait_for(|| !h.module.account(ACC2).unwrap().is_connected()).await;
    assert_eq!(h.module.account(ACC2).unwrap().label(), "Empresa");
    assert_eq!(h.module.module_status(), ModuleStatus::Running);
    let evs = drain(&mut h.events);
    assert_eq!(
        wa_details(&evs, "disconnected")[0],
        json!({"account_id": ACC2, "reason": "conta desativada"})
    );
    assert!(!h.module.account_refs()[1].enabled);
    // reativar reconecta (mock com sessão)
    h.module.set_account(ACC2, None, Some(true)).await.unwrap();
    wait_for(|| h.module.account(ACC2).unwrap().is_connected()).await;
    assert!(h.module.set_account("nope", None, None).await.is_err());

    // settings sem acc2 → conta removida e desconectada; rótulo padrão nas settings não sobrescreve o atual
    let mut s = h.module.settings();
    s.accounts = vec![WhatsAppAccountRef {
        label: "Conta 1".into(),
        ..account(ACC1, "", Some("5511900000000"))
    }];
    h.module.update_settings(s).await;
    assert!(h.module.account(ACC2).is_none());
    assert_eq!(h.module.account_refs().len(), 1);
    assert_eq!(h.module.account(ACC1).unwrap().label(), "Pessoal");
    assert!(h.module.is_connected());

    // settings com conta nova → criada pela fábrica, sem sessão
    let mut s = h.module.settings();
    s.accounts.push(account("acc3", "Terceira", None));
    h.module.update_settings(s).await;
    assert!(h.module.account("acc3").is_some());
    assert!(h.mocks.lock().contains_key("acc3"));
    assert_eq!(
        h.module.status().await["accounts"][1]["state"],
        "disconnected"
    );
    // start() de novo não derruba quem está conectado e tolera conta sem sessão
    h.module.start().await.unwrap();
    assert!(h.module.account(ACC1).unwrap().is_connected());
    assert_eq!(h.module.module_status(), ModuleStatus::Running);
}
