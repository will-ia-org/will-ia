use crate::Event;
use tokio::sync::broadcast;

/// Barramento de eventos do serviço. Clonável e barato.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn emit(&self, event: Event) {
        // Sem assinantes não é erro.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn log(&self, level: &str, module: &str, message: impl Into<String>) {
        self.emit(Event::Log {
            at: chrono::Utc::now(),
            level: level.to_string(),
            module: module.to_string(),
            message: message.into(),
        });
    }
}
