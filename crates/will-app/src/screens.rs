//! Monitores do computador: onde cada um fica na área de trabalho e qual a
//! área útil (sem barra de menus e Dock). Serve para ancorar as caixinhas
//! flutuantes no canto da tela em que a janela do will-ia está.
//!
//! O egui só informa o **tamanho** do monitor da janela, não a posição dele;
//! com mais de um monitor isso não basta. No macOS as telas vêm do AppKit.
//! Nos outros sistemas a lista vem vazia e quem chama usa o monitor principal.

/// Um monitor, em pontos, com a origem no canto superior esquerdo do monitor
/// principal e o eixo Y para baixo (o sistema de coordenadas das janelas).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// O monitor inteiro.
    pub frame: egui::Rect,
    /// A parte livre de barra de menus e Dock.
    pub work: egui::Rect,
}

/// Converte um retângulo do AppKit (origem embaixo à esquerda do monitor
/// principal, Y para cima) para o sistema das janelas (Y para baixo).
/// `primary_height` é a altura do monitor principal.
#[cfg(any(target_os = "macos", test))]
pub fn flip(x: f64, y: f64, width: f64, height: f64, primary_height: f64) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(x as f32, (primary_height - (y + height)) as f32),
        egui::vec2(width as f32, height as f32),
    )
}

/// O monitor que contém `point`; sem nenhum que contenha, o mais próximo.
pub fn containing(screens: &[Screen], point: egui::Pos2) -> Option<Screen> {
    screens
        .iter()
        .find(|s| s.frame.contains(point))
        .or_else(|| {
            screens.iter().min_by(|a, b| {
                a.frame
                    .distance_sq_to_pos(point)
                    .total_cmp(&b.frame.distance_sq_to_pos(point))
            })
        })
        .copied()
}

/// Todos os monitores. Só responde na thread principal (onde a interface roda).
#[cfg(target_os = "macos")]
pub fn all() -> Vec<Screen> {
    use objc2_app_kit::NSScreen;
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return Vec::new();
    };
    let screens = NSScreen::screens(mtm);
    // O primeiro é o monitor principal, de origem (0, 0).
    let Some(primary_height) = screens.iter().next().map(|s| s.frame().size.height) else {
        return Vec::new();
    };
    screens
        .iter()
        .map(|s| {
            let (f, v) = (s.frame(), s.visibleFrame());
            Screen {
                frame: flip(
                    f.origin.x,
                    f.origin.y,
                    f.size.width,
                    f.size.height,
                    primary_height,
                ),
                work: flip(
                    v.origin.x,
                    v.origin.y,
                    v.size.width,
                    v.size.height,
                    primary_height,
                ),
            }
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn all() -> Vec<Screen> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f32, y: f32, w: f32, h: f32) -> Screen {
        let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
        Screen { frame: r, work: r }
    }

    #[test]
    fn appkit_rects_are_flipped_to_window_coordinates() {
        // principal 1728×1117; área útil sem a barra de menus (38) nem o Dock (70)
        let work = flip(0.0, 70.0, 1728.0, 1009.0, 1117.0);
        assert_eq!(work.min, egui::pos2(0.0, 38.0));
        assert_eq!(work.max, egui::pos2(1728.0, 1047.0));
        // monitor à esquerda, mais alto, alinhado por baixo
        let left = flip(-2560.0, 0.0, 2560.0, 1440.0, 1117.0);
        assert_eq!(left.min, egui::pos2(-2560.0, -323.0));
        assert_eq!(left.max, egui::pos2(0.0, 1117.0));
    }

    #[test]
    fn picks_the_screen_under_the_point_or_the_nearest() {
        let screens = [
            screen(0.0, 0.0, 1000.0, 800.0),
            screen(-1200.0, 0.0, 1200.0, 900.0),
        ];
        assert_eq!(
            containing(&screens, egui::pos2(500.0, 400.0)),
            Some(screens[0])
        );
        assert_eq!(
            containing(&screens, egui::pos2(-300.0, 100.0)),
            Some(screens[1])
        );
        // fora de todos: o mais próximo
        assert_eq!(
            containing(&screens, egui::pos2(1500.0, 400.0)),
            Some(screens[0])
        );
        assert_eq!(containing(&[], egui::pos2(0.0, 0.0)), None);
    }
}
