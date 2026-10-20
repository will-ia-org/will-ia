//! will-server — composição: orquestrador, módulos e API local (biblioteca).
//! O binário `will-ia` vive em `crates/will-desktop`.

pub mod api;
pub mod app;
pub mod engine_cfg;
pub mod kb;
pub mod logging;
pub mod mcp_json;
pub mod modules;
pub mod orchestrator;

pub use app::{AppState, RunOptions, ServerHandle, UiHook};

/// Sobe tudo: storage, módulos, MCP, API. Retorna handle com endereço e token da UI.
pub async fn run(opts: RunOptions) -> anyhow::Result<ServerHandle> {
    app::start(opts).await
}
