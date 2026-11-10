//! Seção Tarefas: liga/desliga o módulo, fuso das datas e avisos no Telegram
//! (entrou em andamento / entrou em atraso).

use super::SettingsState;
use crate::help;
use crate::i18n::tr;
use crate::pages::{Cx, Nav};
use crate::ui;
use will_core::Module;

#[derive(Default)]
pub struct TasksSettingsPage;

impl TasksSettingsPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::heading_help(
            ui,
            tr("Tarefas"),
            Some(tr(
                "Painel Gantt de tarefas com início/fim e prioridade; o agente mexe nelas pelas ferramentas tasks_*.",
            )),
            &help::TASKS,
        );
        ui::section(ui, tr("Configuração"), |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Tasks)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label(tr("Módulo:"));
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                if ui.button(tr("ver tarefas")).clicked() {
                    *cx.go = Some(Nav::Tasks);
                }
            });
            ui.add_space(4.0);
            let t = &mut st.draft.tasks;
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr(
                    "Desligado, o painel some do agente (tasks_* indisponíveis) e nenhum aviso é enviado; as tarefas ficam guardadas.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.enabled);
                },
            );
            ui::field(
                ui,
                tr("Fuso horário"),
                Some(tr(
                    "Nome IANA usado nas datas digitadas sem fuso e na exibição. Brasil: America/Sao_Paulo (GMT-3).",
                )),
                |ui| {
                    ui::text_input(ui, &mut t.timezone, "America/Sao_Paulo");
                },
            );
            ui::field(
                ui,
                tr("Notificação do sistema"),
                Some(tr(
                    "Notificação nativa quando uma tarefa entra em andamento ou em atraso (como nas triggers e aprovações).",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_native);
                },
            );
            ui::field(
                ui,
                tr("Avisar no Telegram ao entrar em andamento"),
                Some(tr(
                    "Quando o relógio passa do início de uma tarefa pendente, manda uma mensagem ao chat padrão do Telegram.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_start);
                },
            );
            ui::field(
                ui,
                tr("Avisar no Telegram ao entrar em atraso"),
                Some(tr(
                    "Quando o relógio passa do fim de uma tarefa não concluída, manda uma mensagem ao chat padrão do Telegram.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_overdue);
                },
            );
            ui.label(
                egui::RichText::new(tr(
                    "Os avisos usam o destino padrão da seção Telegram (chat padrão ou primeiro chat permitido) e precisam do módulo Telegram no ar.",
                ))
                .weak()
                .small(),
            );
        });
        st.save_bar(ui, cx.svc, "tasks");
    }
}
