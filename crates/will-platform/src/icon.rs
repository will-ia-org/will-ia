//! Ícone do produto (`assets/icon/`, gerado por `assets/icon/gerar.sh`) embutido
//! nos binários e decodificado para RGBA quando o SO pede pixels: janela/Dock/
//! barra de tarefas (eframe), bandeja (tray-icon/ksni) e os PNGs que o instalador
//! grava no tema `hicolor` do Linux.

/// PNGs coloridos (squircle azul + onda), por lado em pixels.
pub const APP_PNGS: &[(u32, &[u8])] = &[
    (16, include_bytes!("../../../assets/icon/will-ia-16.png")),
    (22, include_bytes!("../../../assets/icon/will-ia-22.png")),
    (32, include_bytes!("../../../assets/icon/will-ia-32.png")),
    (44, include_bytes!("../../../assets/icon/will-ia-44.png")),
    (48, include_bytes!("../../../assets/icon/will-ia-48.png")),
    (64, include_bytes!("../../../assets/icon/will-ia-64.png")),
    (128, include_bytes!("../../../assets/icon/will-ia-128.png")),
    (256, include_bytes!("../../../assets/icon/will-ia-256.png")),
    (512, include_bytes!("../../../assets/icon/will-ia-512.png")),
];

/// Versão *template* (só alfa, preto) para a barra de menus do macOS.
pub const TRAY_TEMPLATE_PNGS: &[(u32, &[u8])] = &[
    (
        22,
        include_bytes!("../../../assets/icon/tray-template-22.png"),
    ),
    (
        32,
        include_bytes!("../../../assets/icon/tray-template-32.png"),
    ),
    (
        44,
        include_bytes!("../../../assets/icon/tray-template-44.png"),
    ),
    (
        64,
        include_bytes!("../../../assets/icon/tray-template-64.png"),
    ),
];

pub fn app_png(size: u32) -> Option<&'static [u8]> {
    APP_PNGS.iter().find(|(s, _)| *s == size).map(|(_, b)| *b)
}

pub fn tray_template_png(size: u32) -> Option<&'static [u8]> {
    TRAY_TEMPLATE_PNGS
        .iter()
        .find(|(s, _)| *s == size)
        .map(|(_, b)| *b)
}

#[derive(Debug, Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    /// RGBA 8 bits não pré-multiplicado, linha a linha.
    pub data: Vec<u8>,
}

/// Decodifica um PNG (qualquer tipo de cor/profundidade) para RGBA8.
pub fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    use png::{ColorType, Decoder, Transformations};
    let mut decoder = Decoder::new(bytes);
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let px = &buf[..info.buffer_size()];
    let data = match info.color_type {
        ColorType::Rgba => px.to_vec(),
        ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        ColorType::GrayscaleAlpha => px
            .chunks(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        other => return Err(format!("PNG com tipo de cor não suportado: {other:?}")),
    };
    Ok(Rgba {
        width: info.width,
        height: info.height,
        data,
    })
}

/// Ícone colorido decodificado no tamanho pedido (exato) ou o maior disponível.
pub fn app_rgba(size: u32) -> Result<Rgba, String> {
    let bytes = app_png(size)
        .or_else(|| APP_PNGS.last().map(|(_, b)| *b))
        .ok_or("sem ícone embutido")?;
    decode_png(bytes)
}

/// Ícone template (preto + alfa) para a barra de menus do macOS.
pub fn tray_template_rgba(size: u32) -> Result<Rgba, String> {
    let bytes = tray_template_png(size)
        .or_else(|| TRAY_TEMPLATE_PNGS.last().map(|(_, b)| *b))
        .ok_or("sem ícone de bandeja embutido")?;
    decode_png(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_embedded_icons() {
        let i = app_rgba(32).unwrap();
        assert_eq!((i.width, i.height), (32, 32));
        assert_eq!(i.data.len(), 32 * 32 * 4);
        // centro do squircle é opaco e azul; canto é transparente
        let center = (16 * 32 + 16) * 4;
        assert_eq!(i.data[center + 3], 255);
        assert!(i.data[center + 2] > i.data[center], "azul > vermelho");
        assert_eq!(i.data[3], 0, "canto transparente");
        let t = tray_template_rgba(22).unwrap();
        assert_eq!((t.width, t.height), (22, 22));
        assert!(t.data.chunks(4).any(|p| p[3] > 0));
        for s in [16, 22, 32, 44, 48, 64, 128, 256, 512] {
            assert!(app_png(s).is_some(), "falta png {s}");
        }
    }
}
