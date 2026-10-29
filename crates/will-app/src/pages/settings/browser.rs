//! Seção Browser CDP: proxy local do Chrome DevTools Protocol (porta de escuta,
//! navegador real, lançamento automático com perfil dedicado, log de tráfego) e
//! a allowlist `Bash(agent-browser:*)` para o claude.

use super::SettingsState;
use crate::help;
use crate::i18n::tr;
use crate::pages::{Cx, Nav};
use crate::service::Loader;
use crate::ui;
use serde_json::Value;
use will_core::Module;

/// Valor do seletor de navegador quando o usuário quer digitar um caminho.
const CUSTOM_PATH: &str = "__custom__";

#[derive(Default)]
pub struct BrowserPage {
    /// Candidatos de `detect` (`{browsers: [{name, path}]}`), carregados ao abrir.
    detect: Loader<Value>,
    /// Caminho digitado em "outro caminho" (mantido mesmo trocando o seletor).
    custom_path: Option<String>,
    /// `true` quando o usuário escolheu "outro caminho" explicitamente, mesmo
    /// com o campo ainda vazio (senão o seletor voltaria para "detectar").
    custom_mode: bool,
    extra_args_buf: Option<String>,
}

impl BrowserPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        if self.detect.stale(std::time::Duration::from_secs(300)) {
            self.detect
                .start(cx.svc.call(|c| async move { c.browser_detect().await }));
        }
        self.detect.poll();

        ui::heading_help(
            ui,
            tr("Browser CDP"),
            Some(tr(
                "Proxy local do Chrome DevTools Protocol: o agente fala com o proxy (AGENT_BROWSER_CDP) e o proxy fala com o navegador real, subindo-o se preciso.",
            )),
            &help::BROWSER,
        );

        ui::section(ui, tr("Configuração"), |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Browser)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label(tr("Módulo:"));
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                if ui.button(tr("ver status e tráfego")).clicked() {
                    *cx.go = Some(Nav::Browser);
                }
            });
            ui.add_space(4.0);

            let b = &mut st.draft.browser;
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr(
                    "Desligado, o proxy não escuta e o agente não recebe AGENT_BROWSER_CDP.",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.enabled);
                },
            );
            ui::field(
                ui,
                tr("Porta do proxy"),
                Some(tr(
                    "Onde o proxy escuta (só loopback). É a porta que vai em AGENT_BROWSER_CDP.",
                )),
                |ui| {
                    ui::number(ui, &mut b.listen_port, 1..=65_535);
                },
            );
            ui::field(
                ui,
                tr("Host do navegador"),
                Some(tr(
                    "Navegador real com depuração remota. Lançamento automático só funciona em loopback.",
                )),
                |ui| {
                    ui::text_input(ui, &mut b.upstream_host, "127.0.0.1");
                },
            );
            ui::field(
                ui,
                tr("Porta do navegador"),
                Some(tr("Porta do --remote-debugging-port do navegador real.")),
                |ui| {
                    ui::number(ui, &mut b.upstream_port, 1..=65_535);
                },
            );
            ui::field(
                ui,
                tr("Subir o navegador sozinho"),
                Some(tr(
                    "Se ninguém responde na porta do navegador, o proxy lança um com --remote-debugging-port.",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.auto_launch);
                },
            );
            ui::field(
                ui,
                tr("Anexar a navegador já aberto"),
                Some(tr(
                    "Se a porta está ocupada por um navegador sem /json/version (Chrome do seu perfil com depuração ligada por outro app), lê o DevToolsActivePort dos perfis do usuário e conecta pelo WebSocket. O Chrome pode pedir \"Allow remote debugging?\" uma vez por conexão.",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.attach_existing);
                },
            );
            ui::field(
                ui,
                tr("Executável do navegador"),
                Some(tr(
                    "Vazio = detectar (Chrome, Chromium, Brave, Edge, Chromium do agent-browser).",
                )),
                |ui| {
                    self.browser_path_ui(ui, &mut b.browser_path);
                },
            );
            ui::field(
                ui,
                tr("Headless (sem janela)"),
                Some(tr(
                    "Lança o navegador sem interface. Desligado, a janela aparece e você vê o agente navegar.",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.headless);
                },
            );
            ui::field(
                ui,
                tr("Pasta do perfil"),
                Some(tr(
                    "Perfil dedicado: o Chrome 136+ ignora a depuração remota no perfil padrão.",
                )),
                |ui| {
                    ui::optional_text(
                        ui,
                        &mut b.user_data_dir,
                        tr("vazio = <data_dir>/browser-profile"),
                    );
                },
            );
            ui::field(
                ui,
                tr("Argumentos extras"),
                Some(tr(
                    "Um por linha, passados na linha de comando do navegador lançado.",
                )),
                |ui| {
                    let mut buf = self
                        .extra_args_buf
                        .take()
                        .unwrap_or_else(|| b.extra_args.join("\n"));
                    if ui::lines_editor(ui, &mut buf, 4).changed() {
                        b.extra_args = ui::parse_lines(&buf);
                    }
                    self.extra_args_buf = Some(buf);
                },
            );
            ui::field(
                ui,
                tr("Espera pelo lançamento (s)"),
                Some(tr(
                    "Quanto esperar o navegador lançado responder antes de desistir.",
                )),
                |ui| {
                    ui::number(ui, &mut b.launch_wait_secs, 1..=300);
                },
            );
            ui::field(
                ui,
                tr("Capacidade do log de tráfego"),
                Some(tr(
                    "Mensagens CDP guardadas em memória para a página Browser CDP.",
                )),
                |ui| {
                    ui::number(ui, &mut b.traffic_capacity, 10..=100_000);
                },
            );
            ui::field(
                ui,
                tr("Fechar o navegador ao parar"),
                Some(tr(
                    "Ao desligar o módulo, fecha o navegador que nós lançamos (nunca um que já estava aberto).",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.close_browser_on_stop);
                },
            );
            ui::field(
                ui,
                tr("Liberar o CLI agent-browser"),
                Some(tr(
                    "Adiciona Bash(agent-browser:*) às ferramentas permitidas do claude.",
                )),
                |ui| {
                    ui::toggle(ui, &mut b.allow_agent_browser_tool);
                },
            );
        });
        st.save_bar(ui, cx.svc, "browser");
    }

    /// Seletor "detectar automaticamente" / candidatos de `detect` / "outro caminho"
    /// (campo de texto). `None` em `value` = automático.
    fn browser_path_ui(&mut self, ui: &mut egui::Ui, value: &mut Option<String>) {
        let candidates: Vec<(String, String)> = self
            .detect
            .data
            .as_ref()
            .and_then(|v| v.get("browsers"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|b| (ui::s(b, "path"), ui::s(b, "name")))
                    .filter(|(p, _)| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Estado do seletor derivado do valor salvo: caminho conhecido → ele;
        // caminho desconhecido ou "outro" escolhido → campo de texto.
        let mut selected = match value {
            None if self.custom_mode => CUSTOM_PATH.to_string(),
            None => String::new(),
            Some(p)
                if candidates.iter().any(|(c, _)| c.as_str() == p.as_str())
                    && !self.custom_mode =>
            {
                p.clone()
            }
            Some(_) => CUSTOM_PATH.to_string(),
        };
        let labels: Vec<(String, String)> = candidates
            .iter()
            .map(|(p, n)| (p.clone(), format!("{n} · {}", ui::ellipsize(p, 60))))
            .collect();
        let mut options: Vec<(&str, &str)> = vec![("", tr("detectar automaticamente"))];
        options.extend(labels.iter().map(|(k, l)| (k.as_str(), l.as_str())));
        options.push((CUSTOM_PATH, tr("outro caminho…")));
        if ui::select(ui, "browser-path", &mut selected, &options) {
            if selected == CUSTOM_PATH {
                self.custom_mode = true;
                *value = self.custom_path.clone().filter(|p| !p.trim().is_empty());
            } else if selected.is_empty() {
                self.custom_mode = false;
                *value = None;
            } else {
                self.custom_mode = false;
                *value = Some(selected.clone());
            }
        }
        if selected == CUSTOM_PATH {
            let mut text = self
                .custom_path
                .take()
                .or_else(|| value.clone())
                .unwrap_or_default();
            if ui::text_input(ui, &mut text, tr("/caminho/para/chrome")).changed() {
                *value = Some(text.trim().to_string()).filter(|p| !p.is_empty());
            }
            self.custom_path = Some(text);
        }
        if self.detect.loading() {
            ui.label(
                egui::RichText::new(tr("procurando navegadores…"))
                    .weak()
                    .small(),
            );
        } else if let Some(e) = &self.detect.error {
            ui.label(
                egui::RichText::new(e)
                    .small()
                    .color(ui::Tone::Warning.color(ui.visuals().dark_mode)),
            );
        } else if candidates.is_empty() {
            ui.label(
                egui::RichText::new(tr(
                    "Nenhum navegador encontrado; instale o Chrome ou baixe o Chromium do agent-browser.",
                ))
                .weak()
                .small(),
            );
        }
    }
}
