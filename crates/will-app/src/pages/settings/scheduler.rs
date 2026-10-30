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
            tr("Triggers"),
            Some(tr(
                "Regras que acordam threads: WhatsApp, e-mail (Gmail) e agendamentos por tempo.",
            )),
            &help::TRIGGERS,
        );

        // Abas (padrão do app para páginas longas): regras × agendamentos.
        let tab = ui::tabs(
            ui,
            "settings-triggers",
            &[
                ui::Tab::new("rules", tr("WhatsApp e E-mail")),
                ui::Tab::new("schedules", tr("Agendamentos")),
            ],
        );
        ui.add_space(6.0);
        if tab == "schedules" {
            self.schedules_tab(ui, cx, st);
            return;
        }

        // ---- triggers universais ----
        ui::section(ui, tr("WhatsApp e E-mail"), |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Triggers)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label(tr("Módulo:"));
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                if ui.button(tr("ver triggers")).clicked() {
                    *cx.go = Some(Nav::Schedules);
                }
            });
            ui.add_space(4.0);
            let t = &mut st.draft.triggers;
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr("Desligado, nenhuma trigger de WhatsApp/e-mail dispara; as regras ficam guardadas.")),
                |ui| {
                    ui::toggle(ui, &mut t.enabled);
                },
            );
            ui::field(
                ui,
                tr("Notificar ao disparar"),
                Some(tr("Notificação nativa a cada disparo.")),
                |ui| {
                    ui::toggle(ui, &mut t.notify_on_fire);
                },
            );
            ui::field(
                ui,
                tr("Varrer o Gmail a cada (minutos)"),
                Some(tr(
                    "Intervalo da consulta à caixa de entrada; o WhatsApp é imediato (evento).",
                )),
                |ui| {
                    ui::number(ui, &mut t.gmail_poll_minutes, 1..=1_440);
                },
            );
            ui::field(
                ui,
                tr("E-mails novos por varredura"),
                Some(tr(
                    "Máximo processado por trigger a cada varredura (os mais antigos primeiro).",
                )),
                |ui| {
                    ui::number(ui, &mut t.gmail_max_per_poll, 1..=100);
                },
            );
            ui::field(ui, tr("Máximo de triggers ativas"), None, |ui| {
                ui::number(ui, &mut t.max_triggers, 1..=10_000);
            });
            ui::field(
                ui,
                tr("Cooldown padrão (segundos)"),
                Some(tr("Intervalo mínimo entre dois disparos da mesma trigger nas regras novas; 0 = sem limite.")),
                |ui| {
                    ui::number(ui, &mut t.default_cooldown_secs, 0..=86_400);
                },
            );
            ui::field(
                ui,
                tr("Agrupar mensagens do WhatsApp (segundos)"),
                Some(tr("Padrão das regras novas: espera a pessoa parar de escrever por estes segundos e dispara uma vez com todas as mensagens. 0 = cada mensagem dispara.")),
                |ui| {
                    ui::number(ui, &mut t.default_debounce_secs, 0..=600);
                },
            );
            ui::field(
                ui,
                tr("Espera máxima do agrupamento (segundos)"),
                Some(tr("Mesmo com mensagens chegando sem parar, dispara ao completar este tempo desde a primeira.")),
                |ui| {
                    ui::number(ui, &mut t.debounce_max_wait_secs, 10..=3_600);
                },
            );
        });
        st.save_bar(ui, cx.svc, "triggers");
    }

    fn schedules_tab(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::section_help(
            ui,
            tr("Agendamentos (por tempo)"),
            Some(&help::SCHEDULER),
            |ui| {
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
            },
        );
        st.save_bar(ui, cx.svc, "scheduler");
    }
}
