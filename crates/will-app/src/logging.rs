//! Logs do app: stdout, arquivo diário em `<config_dir>/logs/will-ia-app.log`
//! e telemetria OTLP (`will-telemetry`, `process = "app"`). O destino vem do
//! servidor (`GET /api/telemetry/client-config`) quando "Enviar logs deste app"
//! está ligado; até lá os eventos ficam numa fila curta em memória.

use std::sync::OnceLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
static TELEMETRY: OnceLock<will_telemetry::Handle> = OnceLock::new();

pub fn telemetry() -> Option<&'static will_telemetry::Handle> {
    TELEMETRY.get()
}

pub fn logs_dir() -> std::path::PathBuf {
    crate::profiles::config_dir().join("logs")
}

pub fn init() {
    let level = std::env::var("WILL_IA_APP_LOG")
        .unwrap_or_else(|_| "info,will_=debug,wgpu=warn,naga=warn,egui=warn,eframe=warn".into());
    let filter = tracing_subscriber::EnvFilter::try_new(&level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let dir = logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = tracing_appender::rolling::daily(&dir, "will-ia-app.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let _ = GUARD.set(guard);
    let instance = will_telemetry::InstanceInfo::new(
        will_telemetry::instance_id(&crate::profiles::config_dir()),
        "app",
        env!("CARGO_PKG_VERSION"),
    );
    let (otel_layer, handle) = will_telemetry::Telemetry::layer(instance);
    let _ = TELEMETRY.set(handle);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact(),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(nb)
                .with_ansi(false),
        )
        .with(otel_layer)
        .try_init();
}

/// Aplica a resposta de `telemetry/client-config` ao remetente do app.
pub fn apply_client_config(v: &serde_json::Value) {
    let Some(h) = telemetry() else {
        return;
    };
    let cfg = will_telemetry::ShipConfig::from_client_config(v);
    let level = will_telemetry::Handle::level_from_str(
        v.get("level").and_then(|l| l.as_str()).unwrap_or("info"),
    );
    h.instance
        .set_label(v.get("label").and_then(|l| l.as_str()).unwrap_or(""));
    h.instance.set_peer(
        v.get("instance_id")
            .and_then(|l| l.as_str())
            .map(|s| s.to_string()),
    );
    let on = cfg.is_some();
    h.configure(cfg, level);
    if on {
        tracing::info!("telemetria do app ligada (destino do servidor)");
    }
}
