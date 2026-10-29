//! Ícone da bandeja do servidor, a partir dos PNGs embutidos em
//! `will_platform::icon` (`assets/icon/`): colorido no Windows/Linux; no macOS
//! a versão *template* (só alfa), que a barra de menus pinta de preto/branco.

/// RGBA + lado para o `tray-icon` (macOS/Windows).
#[cfg(not(target_os = "linux"))]
pub fn tray_rgba() -> Option<(Vec<u8>, u32)> {
    let i = if cfg!(target_os = "macos") {
        will_platform::icon::tray_template_rgba(44)
    } else {
        will_platform::icon::app_rgba(32)
    };
    match i {
        Ok(i) => Some((i.data, i.width)),
        Err(e) => {
            tracing::warn!(error = %e, "ícone da bandeja");
            None
        }
    }
}

/// ARGB32 em ordem de rede (big-endian), como o StatusNotifierItem espera.
#[cfg(target_os = "linux")]
pub fn argb_network(size: u32) -> Vec<u8> {
    let Ok(i) = will_platform::icon::app_rgba(size) else {
        return vec![0; (size * size * 4) as usize];
    };
    let mut out = Vec::with_capacity(i.data.len());
    for px in i.data.chunks(4) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out
}
