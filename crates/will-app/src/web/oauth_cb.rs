//! Versão do navegador de `oauth_cb.rs`: uma página não abre porta local para
//! receber o retorno do Google. Conectar a conta fica para o app de desktop.

use std::time::Duration;

/// Marcador no lugar do `TcpListener` do desktop.
pub struct NoListener;

pub async fn bind(_fixed_port: Option<u16>) -> Result<(NoListener, u16), String> {
    Err("conectar a conta Google só funciona pelo app de desktop".into())
}

pub async fn wait_code(
    _listener: NoListener,
    _expected_state: &str,
    _timeout: Duration,
) -> Result<String, String> {
    Err("conectar a conta Google só funciona pelo app de desktop".into())
}
