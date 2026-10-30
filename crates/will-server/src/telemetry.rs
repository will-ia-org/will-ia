//! Telemetria OTLP: liga as configurações (`Settings.telemetry` + segredo
//! `telemetry_token`) ao `will_telemetry::Handle`, e sobe os relatórios de
//! falha no start. O Axiom é só um preset (endpoint + `X-Axiom-Dataset`).

use crate::app::AppState;
use secrecy::SecretString;
use will_core::settings::TelemetrySettings;
use will_telemetry::ShipConfig;

pub const TOKEN_KEY: &str = "telemetry_token";

/// IP público detectado no start (rótulo padrão da instância).
static PUBLIC_IP: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

pub fn public_ip() -> Option<String> {
    PUBLIC_IP.lock().clone()
}

/// Rótulo efetivo: o configurado ou, vazio, o IP público (ou o host).
pub fn effective_label(s: &TelemetrySettings) -> String {
    let l = s.label.trim();
    if !l.is_empty() {
        return l.to_string();
    }
    public_ip().unwrap_or_default()
}

/// Primeira execução de um binário com telemetria embutida (`baked_telemetry`,
/// docs/29): copia os padrões e o token para esta instalação e liga. Só uma
/// vez (`seeded`), e nunca por cima de uma telemetria já ligada pelo usuário.
pub async fn seed_defaults(state: &AppState) {
    let mut settings = state.settings();
    if settings.telemetry.seeded {
        return;
    }
    settings.telemetry.seeded = true;
    if let Some(baked) = will_core::settings::baked_telemetry() {
        if settings.telemetry.enabled {
            tracing::info!("telemetria já configurada; padrões embutidos ignorados");
        } else {
            baked.apply_to(&mut settings.telemetry);
            settings.telemetry.enabled = true;
            if !baked.token.trim().is_empty() {
                if let Err(e) = state
                    .secrets
                    .set(
                        TOKEN_KEY,
                        &SecretString::from(baked.token.trim().to_string()),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "não gravei o token de telemetria embutido");
                    settings.telemetry.enabled = false;
                }
            }
            tracing::info!(provider = %settings.telemetry.provider, dataset = %settings.telemetry.dataset, "telemetria pré-configurada aplicada");
        }
    }
    if let Err(e) = state.storage.save_settings(&settings).await {
        tracing::warn!(error = %e, "não salvei as configurações (telemetria)");
        return;
    }
    *state.settings.write() = settings;
}

/// Detecta o IP público em segundo plano e reaplica (rótulo/atributo).
pub fn spawn_public_ip_probe(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        let ip = will_telemetry::detect_public_ip().await;
        tracing::debug!(ip = ip.as_deref().unwrap_or("?"), "ip público");
        *PUBLIC_IP.lock() = ip;
        apply(&state).await;
    });
}

/// Monta o destino a partir das configurações; `None` = desligado/incompleto.
pub fn ship_config(s: &TelemetrySettings, token: Option<SecretString>) -> Option<ShipConfig> {
    if !s.enabled {
        return None;
    }
    let base = s.endpoint.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    let (headers, bearer) = match s.provider.as_str() {
        "otlp" => {
            let headers = s
                .headers
                .iter()
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    let (k, v) = (k.trim(), v.trim());
                    (!k.is_empty()).then(|| (k.to_string(), v.to_string()))
                })
                .collect();
            (headers, token)
        }
        _ => {
            let dataset = s.dataset.trim();
            if dataset.is_empty() {
                return None;
            }
            token.as_ref()?;
            (
                vec![("X-Axiom-Dataset".to_string(), dataset.to_string())],
                token,
            )
        }
    };
    Some(ShipConfig {
        logs_url: format!("{base}/v1/logs"),
        traces_url: s.send_traces.then(|| format!("{base}/v1/traces")),
        headers,
        bearer,
    })
}

pub fn level(s: &TelemetrySettings) -> tracing::Level {
    will_telemetry::Handle::level_from_str(&s.level)
}

/// (Re)aplica as configurações ao remetente. Chamado no start e a cada
/// mudança da seção `telemetry` ou do token.
pub async fn apply(state: &AppState) {
    let Some(h) = crate::logging::telemetry() else {
        return;
    };
    let s = state.settings().telemetry;
    let token = state.secrets.get(TOKEN_KEY).await.ok().flatten();
    let cfg = ship_config(&s, token);
    h.instance.set_label(&effective_label(&s));
    h.instance.set_extra("net.public_ip", public_ip());
    let on = cfg.is_some();
    h.configure(cfg, level(&s));
    if on {
        tracing::info!(provider = %s.provider, endpoint = %s.endpoint, level = %s.level, traces = s.send_traces, "telemetria ligada");
    } else if s.enabled {
        tracing::warn!(provider = %s.provider, "telemetria ativada mas incompleta (token/dataset/endpoint)");
    } else {
        tracing::debug!("telemetria desligada");
    }
}

/// Sobe o que ficou nos arquivos de falha desde a última vez (um evento por arquivo).
pub fn ship_crash_reports(state: &AppState) {
    let Some(h) = crate::logging::telemetry() else {
        return;
    };
    if !state.settings().telemetry.ship_crash_reports {
        return;
    }
    let logs = &state.paths.logs_dir;
    let offsets = logs.join("telemetry-offsets.json");
    for (file, kind) in [
        ("will-ia-fatal.log", "fatal"),
        ("service.err.log", "service_err"),
        ("setup.log", "setup"),
    ] {
        h.ship_file_tail(&logs.join(file), kind, &offsets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TelemetrySettings {
        TelemetrySettings {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn axiom_preset_needs_token_and_dataset() {
        let s = base();
        assert!(ship_config(&s, None).is_none());
        let cfg = ship_config(&s, Some(SecretString::from("xaat-1".to_string()))).expect("cfg");
        assert_eq!(cfg.logs_url, "https://api.axiom.co/v1/logs");
        assert_eq!(
            cfg.traces_url.as_deref(),
            Some("https://api.axiom.co/v1/traces")
        );
        assert!(cfg
            .headers
            .iter()
            .any(|(k, v)| k == "X-Axiom-Dataset" && v == "will-ia"));
        let mut s2 = base();
        s2.dataset = "  ".into();
        assert!(ship_config(&s2, Some(SecretString::from("x".to_string()))).is_none());
    }

    #[test]
    fn generic_otlp_parses_headers_and_optional_token() {
        let mut s = base();
        s.provider = "otlp".into();
        s.endpoint = "http://localhost:4318/".into();
        s.headers = vec![
            "api-key: abc".into(),
            "semvalor".into(),
            " X-Scope-OrgID : 1 ".into(),
        ];
        s.send_traces = false;
        let cfg = ship_config(&s, None).expect("cfg");
        assert_eq!(cfg.logs_url, "http://localhost:4318/v1/logs");
        assert!(cfg.traces_url.is_none());
        assert!(cfg.bearer.is_none());
        assert_eq!(
            cfg.headers,
            vec![
                ("api-key".to_string(), "abc".to_string()),
                ("X-Scope-OrgID".to_string(), "1".to_string())
            ]
        );
    }

    #[test]
    fn disabled_is_none() {
        let mut s = base();
        s.enabled = false;
        assert!(ship_config(&s, Some(SecretString::from("x".to_string()))).is_none());
    }
}
