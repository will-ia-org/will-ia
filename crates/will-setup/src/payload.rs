//! Binários embutidos pelo `build.rs` (vazios quando o instalador foi compilado
//! sem `WILL_SETUP_PAYLOAD_DIR`).

pub static SERVER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/will-ia.payload"));
pub static APP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/will-ia-app.payload"));

/// Versão que este instalador instala (a do workspace no momento do build).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Info.plist do `.app` do macOS (o mesmo do release), com a versão preenchida em runtime.
#[cfg(target_os = "macos")]
pub const MACOS_INFO_PLIST: &str = include_str!("../../../packaging/macos/Info.plist");

pub fn available() -> bool {
    cfg!(payload) && !SERVER.is_empty() && !APP.is_empty()
}

pub fn total_bytes() -> u64 {
    (SERVER.len() + APP.len()) as u64
}
