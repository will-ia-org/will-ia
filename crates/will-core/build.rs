//! Telemetria pré-configurada na distribuição (docs/29): se `WILL_TELEMETRY_DEFAULTS`
//! (caminho de um JSON) ou `WILL_TELEMETRY_DEFAULTS_JSON` (o JSON em si) estiver
//! definido no build, o conteúdo vira `WILL_TELEMETRY_BAKED` e o servidor usa como
//! padrão na primeira execução (`will_core::settings::baked_telemetry`).

fn main() {
    println!("cargo:rerun-if-env-changed=WILL_TELEMETRY_DEFAULTS");
    println!("cargo:rerun-if-env-changed=WILL_TELEMETRY_DEFAULTS_JSON");
    let mut json = std::env::var("WILL_TELEMETRY_DEFAULTS_JSON").unwrap_or_default();
    if let Ok(path) = std::env::var("WILL_TELEMETRY_DEFAULTS") {
        println!("cargo:rerun-if-changed={path}");
        match std::fs::read_to_string(&path) {
            Ok(s) => json = s,
            Err(e) => println!("cargo:warning=WILL_TELEMETRY_DEFAULTS={path}: {e}"),
        }
    }
    let json: String = json.split_whitespace().collect::<Vec<_>>().join(" ");
    if json.trim().is_empty() {
        return;
    }
    println!("cargo:rustc-env=WILL_TELEMETRY_BAKED={json}");
    println!("cargo:warning=will-core: telemetria pré-configurada embutida (WILL_TELEMETRY_BAKED)");
}
