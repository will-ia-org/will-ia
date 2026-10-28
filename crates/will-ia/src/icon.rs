//! Ícone do app gerado proceduralmente (sem arquivos de recurso): disco azul
//! escuro com um anel claro e um ponto, em RGBA não pré-multiplicado.

pub fn rgba(size: u32, active: bool) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    let r = size as f32 / 2.0;
    let (bg, ring, dot) = if active {
        ([30u8, 120, 90], [120u8, 230, 180], [255u8, 255, 255])
    } else {
        ([31u8, 42, 68], [150u8, 170, 220], [255u8, 255, 255])
    };
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            let i = ((y * size + x) * 4) as usize;
            let aa = (r - d).clamp(0.0, 1.0);
            if aa <= 0.0 {
                continue;
            }
            let ring_w = r * 0.12;
            let ring_r = r * 0.62;
            let on_ring = ((d - ring_r).abs() < ring_w) as u8 as f32;
            let on_dot = (d < r * 0.22) as u8 as f32;
            let mut col = bg;
            if on_ring > 0.0 {
                col = ring;
            }
            if on_dot > 0.0 {
                col = dot;
            }
            out[i] = col[0];
            out[i + 1] = col[1];
            out[i + 2] = col[2];
            out[i + 3] = (aa * 255.0) as u8;
        }
    }
    out
}

/// ARGB32 em ordem de rede (big-endian), como o StatusNotifierItem espera.
#[cfg(target_os = "linux")]
pub fn argb_network(size: u32, active: bool) -> Vec<u8> {
    let rgba = rgba(size, active);
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks(4) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out
}
