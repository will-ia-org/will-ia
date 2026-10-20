//! tracing → stdout + arquivo diário + barramento (aba Logs), com redator de segredos.

use std::sync::OnceLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
use will_core::{AppPaths, EventBus};

static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

pub fn redact(s: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(sk-ant-[A-Za-z0-9_\-]{6})[A-Za-z0-9_\-]+|(sk-[A-Za-z0-9]{4})[A-Za-z0-9\-_]{16,}|(\d{8,}:[A-Za-z0-9_\-]{4})[A-Za-z0-9_\-]{20,}|(Bearer\s+[A-Za-z0-9_\-\.]{4})[A-Za-z0-9_\-\.]+").unwrap()
    });
    re.replace_all(s, |c: &regex::Captures| {
        let keep = c
            .get(1)
            .or(c.get(2))
            .or(c.get(3))
            .or(c.get(4))
            .map(|m| m.as_str())
            .unwrap_or("");
        format!("{keep}…[redigido]")
    })
    .to_string()
}

struct BusLayer {
    bus: EventBus,
}

struct MsgVisitor(String);
impl tracing::field::Visit for MsgVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={value:?}", field.name()));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={value}", field.name()));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for BusLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if level > tracing::Level::INFO {
            return;
        }
        let target = event.metadata().target();
        if !target.starts_with("will_") {
            return;
        }
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        let module = target
            .split("::")
            .next()
            .unwrap_or(target)
            .trim_start_matches("will_")
            .to_string();
        self.bus.emit(will_core::Event::Log {
            at: chrono::Utc::now(),
            level: level.to_string().to_lowercase(),
            module,
            message: redact(&v.0),
        });
    }
}

pub fn init(paths: &AppPaths, bus: EventBus) {
    let level = std::env::var("WILL_IA_LOG").unwrap_or_else(|_| {
        "info,will_=debug,sqlx=warn,hyper=warn,h2=warn,rustls=warn,teloxide=warn,reqwest=warn"
            .into()
    });
    let filter = tracing_subscriber::EnvFilter::try_new(&level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = tracing_appender::rolling::daily(&paths.logs_dir, "will-ia.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let _ = GUARD.set(guard);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(nb)
        .with_ansi(false);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .with(BusLayer { bus })
        .try_init();
}
