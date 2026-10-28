//! Seção Agendamentos: liga/desliga o scheduler, notificação ao disparar, fuso
//! padrão das expressões cron e limites (máximo de agendamentos, intervalo mínimo).

use super::SettingsState;
use crate::help;
use crate::i18n::tr;
use crate::pages::{Cx, Nav};
use crate::ui;
use will_core::Module;

#[derive(Default)]
pub struct SchedulerPage;

impl SchedulerPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::heading_help(
            ui,
            tr("Agendamentos"),
            Some(tr(
                "Lembretes e tarefas programados (cron ou data única), entregues como mensagem na thread dona.",
            )),
            &help::SCHEDULER,
        );

        ui::section(ui, tr("Configuração"), |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Scheduler)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label(tr("Módulo:"));
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                if ui.button(tr("ver agendamentos")).clicked() {
                    *cx.go = Some(Nav::Schedules);
                }
            });
            ui.add_space(4.0);

            let s = &mut st.draft.scheduler;
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr(
                    "Desligado, nada dispara e o agente não consegue criar agendamentos; os existentes ficam guardados.",
                )),
                |ui| {
                    ui::toggle(ui, &mut s.enabled);
                },
            );
            ui::field(
                ui,
                tr("Notificar ao disparar"),
                Some(tr(
                    "Notificação nativa quando um agendamento dispara em thread da UI ou da voz (no Telegram a resposta já chega no tópico).",
                )),
                |ui| {
                    ui::toggle(ui, &mut s.notify_on_fire);
                },
            );
            ui::field(
                ui,
                tr("Fuso padrão"),
                Some(tr(
                    "Nome IANA usado pelas expressões cron sem fuso próprio. Vazio = fuso do sistema.",
                )),
                |ui| {
                    ui::optional_text(
                        ui,
                        &mut s.timezone,
                        tr("ex.: America/Sao_Paulo; vazio = padrão"),
                    );
                },
            );
            ui::field(
                ui,
                tr("Máximo de agendamentos ativos"),
                Some(tr(
                    "Proteção contra um agente em loop criando agendamentos sem parar.",
                )),
                |ui| {
                    ui::number(ui, &mut s.max_schedules, 1..=10_000);
                },
            );
            ui::field(
                ui,
                tr("Intervalo mínimo (minutos)"),
                Some(tr(
                    "Entre dois disparos do mesmo cron. Expressões mais frequentes são rejeitadas (ex.: * * * * *).",
                )),
                |ui| {
                    ui::number(ui, &mut s.min_interval_minutes, 1..=1_440);
                },
            );
        });
        st.save_bar(ui, cx.svc, "scheduler");
    }
}
