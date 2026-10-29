//! Ícone da janela (barra de título, barra de tarefas/Dock onde o SO usa o da
//! janela), decodificado dos PNGs embutidos em `will_platform::icon`.

pub fn egui_icon() -> egui::IconData {
    match will_platform::icon::app_rgba(256) {
        Ok(i) => egui::IconData {
            rgba: i.data,
            width: i.width,
            height: i.height,
        },
        Err(e) => {
            tracing::warn!(error = %e, "ícone da janela");
            egui::IconData::default()
        }
    }
}
