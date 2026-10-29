//! Ícones da interface: **Material Symbols** (crate `egui_material_icons`, fonte
//! embutida, registrada em `ui::install_fonts`). Nada de glifos Unicode soltos
//! (▸ ▾ 🗐 ⋯) como ícone: as fontes de texto (Inter, Noto Emoji) não os têm e
//! viravam "caixinha". Só os ícones usados pelo app entram aqui, com nome
//! semântico; desenhe com `ui::icon`, `ui::icon_button` ou `ui::icon_text`.
//!
//! Emojis de **conteúdo** (mensagens, Markdown) continuam na Noto Emoji.

use egui_material_icons::icons as m;
pub use egui_material_icons::MaterialIcon;

pub const CHEVRON_RIGHT: MaterialIcon = m::ICON_CHEVRON_RIGHT;
pub const EXPAND_MORE: MaterialIcon = m::ICON_EXPAND_MORE;
pub const CLOSE: MaterialIcon = m::ICON_CLOSE;
pub const CHECK_CIRCLE: MaterialIcon = m::ICON_CHECK_CIRCLE;
pub const ERROR: MaterialIcon = m::ICON_ERROR;
pub const WARNING: MaterialIcon = m::ICON_WARNING;
pub const INFO: MaterialIcon = m::ICON_INFO;
pub const REFRESH: MaterialIcon = m::ICON_REFRESH;
pub const SEND: MaterialIcon = m::ICON_SEND;
pub const MIC: MaterialIcon = m::ICON_MIC;
pub const RECORD: MaterialIcon = m::ICON_FIBER_MANUAL_RECORD;
pub const ATTACH: MaterialIcon = m::ICON_ATTACH_FILE;
pub const ARROW_UPWARD: MaterialIcon = m::ICON_ARROW_UPWARD;
pub const VISIBILITY: MaterialIcon = m::ICON_VISIBILITY;
pub const VISIBILITY_OFF: MaterialIcon = m::ICON_VISIBILITY_OFF;

/// Todos os ícones nomeados (o teste garante que a fonte tem cada glifo).
#[cfg(test)]
pub const ALL: &[(&str, MaterialIcon)] = &[
    ("CHEVRON_RIGHT", CHEVRON_RIGHT),
    ("EXPAND_MORE", EXPAND_MORE),
    ("CLOSE", CLOSE),
    ("CHECK_CIRCLE", CHECK_CIRCLE),
    ("ERROR", ERROR),
    ("WARNING", WARNING),
    ("INFO", INFO),
    ("REFRESH", REFRESH),
    ("SEND", SEND),
    ("MIC", MIC),
    ("RECORD", RECORD),
    ("ATTACH", ATTACH),
    ("ARROW_UPWARD", ARROW_UPWARD),
    ("VISIBILITY", VISIBILITY),
    ("VISIBILITY_OFF", VISIBILITY_OFF),
];

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::{Font, FontRef};

    /// Cada ícone nomeado existe na fonte Material embutida (nada de "caixinha").
    #[test]
    fn every_named_icon_exists_in_the_font() {
        let insert = egui_material_icons::font_insert();
        let bytes: &[u8] = insert.data.font.as_ref().as_ref();
        let font = FontRef::try_from_slice(bytes).expect("fonte Material válida");
        for (name, ic) in ALL {
            for ch in ic.codepoint.chars() {
                assert_ne!(
                    font.glyph_id(ch).0,
                    0,
                    "{name}: U+{:04X} fora da fonte",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in ALL {
            assert!(seen.insert(*name), "nome repetido: {name}");
        }
    }
}
