//! Disparo ponta a ponta com storage em memória e um ThreadHost falso.

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::broadcast;
use will_core::settings::SchedulerSettings;
use will_core::{
    ApprovalDecision, ApprovalId, ClaudeTokenInfo, Event, EventBus, Origin, ScheduleTrigger,
    ThreadHost, ThreadId, ThreadSummary, TurnId,
};
use will_scheduler::{ScheduleInput, SchedulerDeps, SchedulerModule};
use will_storage::Storage;

#[derive(Default)]
struct FakeHost {
    threads: Mutex<Vec<ThreadSummary>>,
    sent: Mutex<Vec<(ThreadId, String)>>,
    bus: Option<EventBus>,
}

impl FakeHost {
    fn add_thread(&self, origin: Origin, session: Option<&str>) -> ThreadId {
        let t = ThreadSummary {
            id: ThreadId::new(),
            origin,
            title: Some("t".into()),
            claude_session_id: session.map(String::from),
            status: will_core::ThreadStatus::Idle,
            created_at: Utc::now(),
            last_activity_at: Utc::now(),
            total_cost_usd: 0.0,
            alive: false,
            pending: 0,
            session_expired: false,
        };
        let id = t.id;
        self.threads.lock().push(t);
        id
    }
}

#[async_trait]
impl ThreadHost for FakeHost {
    async fn ensure_thread(
        &self,
        origin: &Origin,
        _t: Option<&str>,
    ) -> Result<ThreadSummary, String> {
        let id = self.add_thread(origin.clone(), None);
        self.get_thread(id).await
    }
    async fn get_thread(&self, thread: ThreadId) -> Result<ThreadSummary, String> {
        self.threads
            .lock()
            .iter()
            .find(|t| t.id == thread)
            .cloned()
            .ok_or_else(|| "thread não encontrada".into())
    }
    async fn list_threads(&self) -> Result<Vec<ThreadSummary>, String> {
        Ok(self.threads.lock().clone())
    }
    async fn send_message_with(
        &self,
        thread: ThreadId,
        text: &str,
        _a: &[will_core::Attachment],
    ) -> Result<TurnId, String> {
        self.get_thread(thread).await?;
        self.sent.lock().push((thread, text.to_string()));
        Ok(TurnId::new())
    }
    async fn restart_thread(&self, _t: ThreadId) -> Result<(), String> {
        Ok(())
    }
    async fn reset_thread(&self, _t: ThreadId) -> Result<(), String> {
        Ok(())
    }
    async fn interrupt_thread(&self, _t: ThreadId) -> Result<(), String> {
        Ok(())
    }
    async fn delete_thread(&self, thread: ThreadId, _p: bool) -> Result<(), String> {
        self.threads.lock().retain(|t| t.id != thread);
        Ok(())
    }
    async fn revive_thread(
        &self,
        origin: &Origin,
        _title: Option<&str>,
        claude_session_id: Option<&str>,
    ) -> Result<ThreadSummary, String> {
        let id = self.add_thread(origin.clone(), claude_session_id);
        self.get_thread(id).await
    }
    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.as_ref().expect("bus").subscribe()
    }
    fn claude_tokens(&self) -> Vec<ClaudeTokenInfo> {
        vec![]
    }
    async fn switch_claude_token(&self, _id: &str) -> Result<usize, String> {
        Ok(0)
    }
    fn decide_approval(&self, _id: ApprovalId, _d: ApprovalDecision, _via: &str) -> bool {
        false
    }
    fn uptime_secs(&self) -> u64 {
        0
    }
}

async fn setup() -> (Arc<SchedulerModule>, Arc<FakeHost>, EventBus) {
    let storage = Storage::open_memory().await.expect("storage");
    let bus = EventBus::new(256);
    let host = Arc::new(FakeHost {
        bus: Some(bus.clone()),
        ..Default::default()
    });
    let module = SchedulerModule::new(
        SchedulerDeps {
            storage,
            host: host.clone(),
            bus: bus.clone(),
        },
        SchedulerSettings {
            min_interval_minutes: 0,
            ..Default::default()
        },
    );
    (module, host, bus)
}

#[tokio::test]
async fn once_schedule_fires_and_pauses() {
    let (module, host, bus) = setup().await;
    let thread = host.add_thread(Origin::Ui, Some("sess-1"));
    let mut events = bus.subscribe();
    module.start();
    let at = (Utc::now() + chrono::Duration::seconds(6)).to_rfc3339();
    let v = module
        .create(
            ScheduleInput {
                name: Some("Teste".into()),
                message: Some("faça X".into()),
                run_at: Some(at),
                ..Default::default()
            },
            thread,
            "agent",
        )
        .await
        .expect("create");
    assert!(matches!(v.schedule.trigger, ScheduleTrigger::Once { .. }));
    assert!(v.schedule.next_run_at.is_some());

    // espera o ScheduleFired
    let fired = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ScheduleFired { ok, .. }) = events.recv().await {
                return ok;
            }
        }
    })
    .await
    .expect("disparo em até 20 s");
    assert!(fired);
    let sent = host.sent.lock().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, thread);
    assert!(sent[0].1.starts_with("[Agendamento \"Teste\""));
    assert!(sent[0].1.ends_with("faça X"));

    let after = module.get(v.schedule.id).await.expect("get");
    assert!(!after.schedule.enabled);
    assert_eq!(after.schedule.run_count, 1);
    assert_eq!(after.schedule.last_result.as_deref(), Some("ok"));
    assert!(after.schedule.paused_reason.is_some());
    module.stop();
}

#[tokio::test]
async fn deleted_thread_is_revived_with_session() {
    let (module, host, _bus) = setup().await;
    let thread = host.add_thread(
        Origin::Telegram {
            chat_id: 1,
            topic_id: Some(7),
        },
        Some("sess-9"),
    );
    let v = module
        .create(
            ScheduleInput {
                name: Some("Rotina".into()),
                message: Some("resuma".into()),
                cron: Some("0 8 * * *".into()),
                timezone: Some("America/Sao_Paulo".into()),
                ..Default::default()
            },
            thread,
            "agent",
        )
        .await
        .expect("create");
    assert!(!v.cron_description.is_empty());
    host.delete_thread(thread, false).await.unwrap();
    module.run_now(v.schedule.id).await.expect("run_now");
    let sent = host.sent.lock().clone();
    assert_eq!(sent.len(), 1);
    let new_thread = sent[0].0;
    assert_ne!(new_thread, thread);
    let t = host.get_thread(new_thread).await.unwrap();
    assert_eq!(t.claude_session_id.as_deref(), Some("sess-9"));
    assert_eq!(
        t.origin,
        Origin::Telegram {
            chat_id: 1,
            topic_id: Some(7)
        }
    );
    // o agendamento passa a apontar para a thread nova
    let after = module.get(v.schedule.id).await.unwrap();
    assert_eq!(after.schedule.thread_id, new_thread);
    assert!(after.schedule.enabled);
}

#[tokio::test]
async fn validation_errors() {
    let (module, host, _bus) = setup().await;
    let thread = host.add_thread(Origin::Ui, None);
    let both = module
        .create(
            ScheduleInput {
                name: Some("x".into()),
                message: Some("y".into()),
                cron: Some("0 8 * * *".into()),
                run_at: Some("2030-01-01T10:00".into()),
                ..Default::default()
            },
            thread,
            "user",
        )
        .await;
    assert!(both.is_err());
    let past = module
        .create(
            ScheduleInput {
                name: Some("x".into()),
                message: Some("y".into()),
                run_at: Some("2001-01-01T10:00".into()),
                ..Default::default()
            },
            thread,
            "user",
        )
        .await;
    assert!(past.unwrap_err().contains("futuro"));
    let bad_cron = module
        .create(
            ScheduleInput {
                name: Some("x".into()),
                message: Some("y".into()),
                cron: Some("99 99 * * *".into()),
                ..Default::default()
            },
            thread,
            "user",
        )
        .await;
    assert!(bad_cron.is_err());
}
