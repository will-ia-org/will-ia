//! will-whatsapp — WhatsApp pessoal do dono exposto aos agentes **somente como
//! ferramentas MCP** (`whatsapp_*`). Não é canal de entrada: mensagens recebidas
//! apenas alimentam um índice local (SQLite/FTS5 em `will-storage`).
//!
//! Arquitetura (ver `docs/17-whatsapp.md`):
//! - [`WhatsAppBackend`]: trait que isola o protocolo. [`MockBackend`] sempre
//!   compila (testes / fallback); `RealBackend` (feature `real`) usa
//!   `whatsapp-rust` 0.7.
//! - [`WhatsAppModule`]: ciclo de vida de **várias contas** ([`WaAccount`], uma
//!   por número pareado, backends criados por [`BackendFactory`]), loop único de
//!   eventos `(account_id, BackendEvent)` → índice por conta, eventos
//!   `Event::WhatsApp{state, detail}` (JSON com `account_id`) no barramento,
//!   poda por retenção e `ToolProvider` com as 16 ferramentas (parâmetro `account`).

pub mod backend;
pub mod mock;
pub mod module;
#[cfg(feature = "real")]
pub mod real;
pub mod tools;

pub use backend::{BackendEvent, IncomingMessage, MessageRef, WhatsAppBackend};
pub use mock::{MockAction, MockBackend, MockSent};
pub use module::{
    is_default_label, BackendFactory, ConnState, Tts, WaAccount, WhatsAppDeps, WhatsAppModule,
};
#[cfg(feature = "real")]
pub use real::RealBackend;
pub use tools::{ToolKind, WaTool, FAMILY, MAX_MESSAGES, MAX_TEXT_BYTES};

/// Renderiza o conteúdo de um QR de pareamento como SVG (para a UI).
pub fn qr_svg(data: &str) -> String {
    use qrcode::render::svg;
    match qrcode::QrCode::new(data.as_bytes()) {
        Ok(code) => code
            .render::<svg::Color>()
            .min_dimensions(256, 256)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
        Err(e) => {
            tracing::warn!("qr_svg: {e}");
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"256\" height=\"64\"><text x=\"8\" y=\"36\" font-size=\"14\">QR inválido: {}</text></svg>",
                e.to_string().replace('<', "&lt;")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn qr_svg_renders() {
        let svg = super::qr_svg("2@abc,def,ghi");
        assert!(svg.contains("<svg"), "{svg}");
        assert!(svg.contains("</svg>"));
    }
}
