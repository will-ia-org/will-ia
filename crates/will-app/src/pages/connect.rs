//! Tela de entrada: aparece em tela cheia (sem menu lateral) sempre que o app
//! não tem conexão com um servidor: sem perfil, perfil escolhido mas servidor
//! fora do ar (com cartão "tentando reconectar"), ou depois de sair do perfil.
//! Duas abas: **Perfis** (salvos + "Esta máquina") e **Novo perfil** (o
//! formulário de login/perfil). Quando o WS de eventos conecta, o app sai
//! desta tela sozinho.
//!
//! Login (`docs/26`, etapa 4): usuário + senha → `POST /api/auth/login` devolve
//! um token de sessão, que fica no perfil (`profiles.json`) junto do nome da
//! conta. O campo Token continua existindo para quem usa o token local da API
//! (`will-ia info`). A troca obrigatória de senha do primeiro acesso é um modal
//! desta página, mostrado pelo app sobre qualquer página ([`ConnectPage::password_gate`])
//! enquanto `principal.must_change_password`. Sair do perfil fica no rodapé do
//! menu lateral (app).
//!
//! TLS (`docs/26`, etapa 5): para `https://` o perfil guarda o certificado do
//! servidor em PEM (`ca_pem`), fixado no estilo TOFU: "Buscar do servidor" lê a
//! impressão digital em `GET /api/health` (sem validar o certificado, só para
//! mostrar), o usuário confere com a que o servidor exibe em Configurações →
//! Rede e cola o PEM aqui. `insecure` aceita qualquer certificado (só testes).

use super::Cx;
use crate::i18n::{tr, trf};
use crate::profiles::{self, Profile, ProfilesFile};
use crate::service::{Task, UiCommand};
use crate::ui::{self, Confirm, Tone};
use serde_json::{json, Value};
use will_client::{pem_fingerprint, Client, TlsOptions};
use will_core::Role;

/// Identificação do app no login (aparece na lista de sessões do usuário).
const CLIENT_NAME: &str = "will-ia-app";
/// Tamanho mínimo exigido na troca de senha (o servidor pode exigir mais).
const MIN_PASSWORD_LEN: usize = 8;
/// Largura do painel central da tela de entrada.
const LANDING_WIDTH: f32 = 720.0;

/// Abas da tela de entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LandingTab {
    #[default]
    Profiles,
    /// Novo perfil / Entrar (o formulário).
    Form,
}

pub struct ConnectPage {
    file: ProfilesFile,
    local: Option<Profile>,
    pub tab: LandingTab,
    /// Perfil aguardando confirmação de remoção (modal).
    remove_pending: Option<Profile>,
    // formulário de perfil
    edit_id: Option<String>,
    name: String,
    url: String,
    token: String,
    username: String,
    password: String,
    /// Certificado do servidor (PEM) para `https://`; vazio = confiar no sistema.
    ca_pem: String,
    /// Aceitar qualquer certificado (inseguro).
    insecure: bool,
    login_task: Option<Task<Value>>,
    /// Quando o login foi disparado por "Conectar" (autologin), o perfil a
    /// atualizar e conectar ao receber o token; `None` = login pelo formulário.
    login_profile: Option<Profile>,
    /// `GET /api/health` do servidor do formulário (Buscar do servidor).
    health_task: Option<Task<Value>>,
    fetched: Option<FetchedHealth>,
    // troca obrigatória de senha
    pw_new: String,
    pw_confirm: String,
    pw_task: Option<Task<Value>>,
    /// Senha já trocada nesta conexão: esconde o modal até o app recarregar o `principal`.
    pw_done: bool,
    /// Pede ao app que recarregue `GET /api/auth/me` (lido e zerado pelo app).
    pub principal_dirty: bool,
    pub last_error: Option<String>,
}

impl Default for ConnectPage {
    fn default() -> Self {
        Self {
            file: profiles::load(),
            local: profiles::local_profile(),
            tab: LandingTab::Profiles,
            remove_pending: None,
            edit_id: None,
            name: String::new(),
            url: "http://".into(),
            token: String::new(),
            username: String::new(),
            password: String::new(),
            ca_pem: String::new(),
            insecure: false,
            login_task: None,
            login_profile: None,
            health_task: None,
            fetched: None,
            pw_new: String::new(),
            pw_confirm: String::new(),
            pw_task: None,
            pw_done: false,
            principal_dirty: false,
            last_error: None,
        }
    }
}

/// Resposta de `GET /api/health` obtida por "Buscar do servidor".
struct FetchedHealth {
    url: String,
    scheme: String,
    fingerprint: Option<String>,
}

/// Normaliza uma impressão digital SHA-256 para comparação (hex minúsculo, sem `:`).
fn norm_fp(fp: &str) -> String {
    fp.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Papel no idioma da UI.
pub fn role_label(role: Role) -> &'static str {
    match role {
        Role::Admin => tr("administrador"),
        Role::Member => tr("membro"),
    }
}

impl ConnectPage {
    pub fn refresh(&mut self) {
        self.file = profiles::load();
        self.local = profiles::local_profile();
    }

    fn clear_form(&mut self) {
        self.edit_id = None;
        self.name.clear();
        self.url = "http://".into();
        self.token.clear();
        self.username.clear();
        self.password.clear();
        self.ca_pem.clear();
        self.insecure = false;
        self.fetched = None;
    }

    /// Abre a aba do formulário já preenchida com o perfil (Entrar/Editar), ou
    /// com um perfil sem token vindo do arranque.
    pub fn open_login(&mut self, p: &Profile) {
        self.load_form(p);
        self.tab = LandingTab::Form;
    }

    fn load_form(&mut self, p: &Profile) {
        self.edit_id = Some(p.id.clone());
        self.name = p.name.clone();
        self.url = p.url.clone();
        self.token = p.token.clone();
        self.username = p.username.clone().unwrap_or_default();
        self.password.clear();
        self.ca_pem = p.ca_pem.clone().unwrap_or_default();
        self.insecure = p.insecure;
        self.fetched = None;
    }

    fn form_is_https(&self) -> bool {
        self.url.trim_start().starts_with("https://")
    }

    /// Política de certificado do formulário (só importa em `https://`).
    fn form_tls(&self) -> TlsOptions {
        TlsOptions {
            ca_pem: Some(self.ca_pem.trim().to_string()).filter(|p| !p.is_empty()),
            insecure: self.insecure,
        }
    }

    /// `https://` sem PEM fixado nem modo inseguro: só funciona se o sistema
    /// confiar no certificado (CA pública), o que não é o caso do autoassinado.
    fn https_without_trust(&self) -> bool {
        self.form_is_https() && !self.insecure && self.ca_pem.trim().is_empty()
    }

    fn https_hint(&self, cx: &mut Cx) {
        if self.https_without_trust() {
            cx.toasts.info(tr(
                "https:// sem certificado fixado: a conexão só funciona se o sistema confiar no certificado do servidor. Cole o PEM em \"TLS (https)\" ou marque o modo inseguro.",
            ));
        }
    }

    fn form_url(&self) -> Result<String, String> {
        let url = self.url.trim().trim_end_matches('/').to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(tr("A URL precisa começar com http:// ou https://").into());
        }
        Ok(url)
    }

    /// Perfil montado a partir do formulário (id novo ou o que está em edição).
    fn form_profile(
        &self,
        url: String,
        token: String,
        username: Option<String>,
        password: Option<String>,
    ) -> Profile {
        let name = if self.name.trim().is_empty() {
            url.clone()
        } else {
            self.name.trim().to_string()
        };
        let id = self
            .edit_id
            .clone()
            .unwrap_or_else(|| format!("p{}", chrono::Utc::now().timestamp_millis()));
        let tls = self.form_tls();
        Profile {
            id,
            name,
            url,
            token,
            username,
            password,
            ca_pem: tls.ca_pem,
            insecure: tls.insecure,
        }
    }

    /// Senha a preservar no perfil: a digitada agora, ou a já guardada (edição
    /// sem retypar a senha).
    fn kept_password(&self) -> Option<String> {
        let typed = self.password.trim();
        if !typed.is_empty() {
            return Some(typed.to_string());
        }
        self.edit_id
            .as_ref()
            .and_then(|id| self.file.profiles.iter().find(|x| &x.id == id))
            .and_then(|p| p.password.clone())
    }

    fn store(&mut self, p: &Profile) -> Result<(), String> {
        self.file.profiles.retain(|x| x.id != p.id);
        self.file.profiles.push(p.clone());
        profiles::save(&self.file)
    }

    fn save_form(&mut self) -> Result<(), String> {
        let url = self.form_url()?;
        let username = Some(self.username.trim().to_string()).filter(|u| !u.is_empty());
        let pw = self.kept_password();
        let p = self.form_profile(url, self.token.trim().to_string(), username, pw);
        self.store(&p)?;
        self.clear_form();
        Ok(())
    }

    /// Dispara `POST /api/auth/login` com o que está no formulário.
    fn start_login(&mut self, cx: &mut Cx) {
        let url = match self.form_url() {
            Ok(u) => u,
            Err(e) => {
                cx.toasts.error(e);
                return;
            }
        };
        let user = self.username.trim().to_string();
        let pass = self.password.clone();
        if user.is_empty() || pass.is_empty() {
            cx.toasts.error(tr("Informe usuário e senha."));
            return;
        }
        self.https_hint(cx);
        let tls = self.form_tls();
        let custom_tls = self.form_is_https() && (tls.insecure || tls.ca_pem.is_some());
        // Função associada: ainda não há sessão, então o cliente recebido é ignorado.
        // Em https com certificado fixado/inseguro, o cliente de login precisa da
        // mesma política de TLS; a rota é pública e o token vazio é ignorado.
        self.login_task = Some(cx.svc.call(move |_c| async move {
            if custom_tls {
                Client::with_tls(&url, "", &tls)
                    .post(
                        "auth/login",
                        Some(json!({ "login": user, "password": pass, "client": CLIENT_NAME })),
                    )
                    .await
            } else {
                Client::login(&url, &user, &pass, Some(CLIENT_NAME)).await
            }
        }));
    }

    /// `GET /api/health` no servidor do formulário, sem validar o certificado:
    /// serve só para mostrar a impressão digital que o usuário vai conferir.
    fn start_fetch_health(&mut self, cx: &mut Cx) {
        let url = match self.form_url() {
            Ok(u) => u,
            Err(e) => {
                cx.toasts.error(e);
                return;
            }
        };
        let tls = TlsOptions {
            ca_pem: None,
            insecure: true,
        };
        self.health_task = Some(
            cx.svc
                .call(move |_c| async move { Client::with_tls(&url, "", &tls).health().await }),
        );
    }

    /// Login respondeu: guarda o token no perfil e conecta.
    fn finish_login(&mut self, cx: &mut Cx, v: Value) {
        let token = ui::s(&v, "token");
        if token.is_empty() {
            cx.toasts
                .error(tr("O servidor não devolveu um token de sessão."));
            return;
        }
        let username = v
            .get("user")
            .map(|u| ui::s(u, "username"))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.username.trim().to_string());
        let url = match self.form_url() {
            Ok(u) => u,
            Err(e) => {
                cx.toasts.error(e);
                return;
            }
        };
        let password = Some(self.password.clone()).filter(|p| !p.is_empty());
        let p = self.form_profile(url, token, Some(username.clone()), password);
        if let Err(e) = self.store(&p) {
            cx.toasts.error(e);
            return;
        }
        self.clear_form();
        self.tab = LandingTab::Profiles;
        profiles::remember_last(&p.id);
        cx.toasts.success(trf("Conectado como {}.", &[&username]));
        cx.svc.send_cmd(UiCommand::Connect(p));
    }

    /// Conectar num perfil escolhido na lista.
    fn connect_to(&mut self, cx: &mut Cx, p: Profile) {
        if p.is_https() && !p.insecure && p.ca_pem.is_none() {
            cx.toasts.info(tr(
                "https:// sem certificado fixado: a conexão só funciona se o sistema confiar no certificado do servidor. Cole o PEM em \"TLS (https)\" ou marque o modo inseguro.",
            ));
        }
        // Sem token de sessão (logoff ou perfil recém-criado sem entrar): se há
        // usuário e senha guardados, reautentica sozinho e conecta; senão, abre
        // o formulário para o usuário digitar a senha.
        if p.token.trim().is_empty() {
            let user = p.username.clone().unwrap_or_default();
            let pass = p.password.clone().unwrap_or_default();
            if !user.is_empty() && !pass.is_empty() {
                self.start_login_for(cx, p);
            } else {
                self.open_login(&p);
            }
            return;
        }
        self.last_error = None;
        self.tab = LandingTab::Profiles;
        profiles::remember_last(&p.id);
        cx.svc.send_cmd(UiCommand::Connect(p));
    }

    /// Autologin a partir de um perfil guardado (botão "Conectar"): usa
    /// usuário/senha/TLS do próprio perfil.
    fn start_login_for(&mut self, cx: &mut Cx, p: Profile) {
        let url = p.url.clone();
        let user = p.username.clone().unwrap_or_default();
        let pass = p.password.clone().unwrap_or_default();
        let tls = TlsOptions {
            ca_pem: p.ca_pem.clone(),
            insecure: p.insecure,
        };
        let custom_tls = p.is_https() && (tls.insecure || tls.ca_pem.is_some());
        self.last_error = None;
        self.login_profile = Some(p);
        self.login_task = Some(cx.svc.call(move |_c| async move {
            if custom_tls {
                Client::with_tls(&url, "", &tls)
                    .post(
                        "auth/login",
                        Some(json!({ "login": user, "password": pass, "client": CLIENT_NAME })),
                    )
                    .await
            } else {
                Client::login(&url, &user, &pass, Some(CLIENT_NAME)).await
            }
        }));
    }

    /// Autologin respondeu: grava o novo token no perfil guardado e conecta.
    fn finish_login_profile(&mut self, cx: &mut Cx, mut p: Profile, v: Value) {
        let token = ui::s(&v, "token");
        if token.is_empty() {
            cx.toasts
                .error(tr("O servidor não devolveu um token de sessão."));
            return;
        }
        p.token = token;
        if let Some(u) = v
            .get("user")
            .map(|u| ui::s(u, "username"))
            .filter(|u| !u.is_empty())
        {
            p.username = Some(u);
        }
        if let Err(e) = self.store(&p) {
            cx.toasts.error(e);
            return;
        }
        self.last_error = None;
        self.tab = LandingTab::Profiles;
        profiles::remember_last(&p.id);
        cx.svc.send_cmd(UiCommand::Connect(p));
    }

    fn poll(&mut self, cx: &mut Cx) {
        if let Some(t) = &self.login_task {
            if let Some(r) = t.take() {
                self.login_task = None;
                let lp = self.login_profile.take();
                match r {
                    Ok(v) => match lp {
                        Some(p) => self.finish_login_profile(cx, p, v),
                        None => self.finish_login(cx, v),
                    },
                    Err(e) => cx.toasts.error(trf("Não entrou: {}", &[&e])),
                }
            }
        }
        if let Some(t) = &self.health_task {
            if let Some(r) = t.take() {
                self.health_task = None;
                match r {
                    Ok(v) => {
                        self.fetched = Some(FetchedHealth {
                            url: self.form_url().unwrap_or_default(),
                            scheme: ui::s(&v, "scheme"),
                            fingerprint: v
                                .get("tls_fingerprint_sha256")
                                .and_then(|f| f.as_str())
                                .map(str::to_string)
                                .filter(|f| !f.is_empty()),
                        });
                    }
                    Err(e) => cx.toasts.error(trf("Não alcançou o servidor: {}", &[&e])),
                }
            }
        }
        self.poll_password(cx);
    }

    fn poll_password(&mut self, cx: &mut Cx) {
        if let Some(t) = &self.pw_task {
            if let Some(r) = t.take() {
                self.pw_task = None;
                match r {
                    Ok(_) => {
                        self.pw_new.clear();
                        self.pw_confirm.clear();
                        self.pw_done = true;
                        self.principal_dirty = true;
                        cx.toasts.success(tr("Senha alterada."));
                    }
                    Err(e) => cx.toasts.error(trf("Não alterou a senha: {}", &[&e])),
                }
            }
        }
    }

    /// Chamado pelo app a cada quadro enquanto conectado: mostra o modal de troca
    /// obrigatória de senha sobre qualquer página, até o `principal` recarregar
    /// sem `must_change_password`.
    pub fn password_gate(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        self.poll_password(cx);
        let must_change = cx
            .snap
            .principal
            .as_ref()
            .is_some_and(|p| p.must_change_password);
        if must_change && !self.pw_done && cx.snap.connected {
            self.password_modal(ctx, cx);
        }
    }

    /// Modal bloqueante do primeiro acesso: só sai com a senha trocada.
    fn password_modal(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        let busy = self.pw_task.as_ref().is_some_and(|t| t.pending());
        let mut submit = false;
        let _ = egui::Modal::new(egui::Id::unique("must-change-password")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(tr("Defina uma nova senha"));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr(
                    "Esta conta ainda usa a senha inicial gerada pelo servidor. Escolha uma senha sua para continuar.",
                ))
                .weak(),
            );
            ui.add_space(10.0);
            ui::field(ui, tr("Nova senha"), None, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.pw_new).margin(crate::ui::input_margin())
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
            });
            ui::field(ui, tr("Confirmar senha"), None, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.pw_confirm).margin(crate::ui::input_margin())
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            });
            ui.horizontal(|ui| {
                if busy {
                    ui::busy(ui, tr("salvando…"));
                } else if ui::primary_button(ui, tr("Salvar senha")).clicked() {
                    submit = true;
                }
            });
        });
        if submit && !busy {
            let new = self.pw_new.clone();
            if new.chars().count() < MIN_PASSWORD_LEN {
                cx.toasts.error(trf(
                    "A senha precisa ter pelo menos {} caracteres.",
                    &[&MIN_PASSWORD_LEN],
                ));
            } else if new != self.pw_confirm {
                cx.toasts.error(tr("As senhas não conferem."));
            } else {
                // Primeiro login obrigatório: a senha atual pode ir vazia.
                self.pw_task = Some(
                    cx.svc
                        .call(move |c| async move { c.change_password("", &new).await }),
                );
            }
        }
    }

    /// Subseção "TLS (https)" do formulário de perfil: PEM fixado, impressão
    /// digital, busca da impressão digital no servidor e modo inseguro.
    fn tls_form(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let dark = ui.visuals().dark_mode;
        ui.label(egui::RichText::new(tr("TLS (https)")).strong());
        if !self.form_is_https() {
            ui.label(
                egui::RichText::new(tr("Só vale para URLs https://."))
                    .small()
                    .weak(),
            );
        }
        let fetching = self.health_task.as_ref().is_some_and(|t| t.pending());
        let local_fp = pem_fingerprint(&self.ca_pem);
        ui::field(
            ui,
            tr("Certificado do servidor (PEM)"),
            Some(tr(
                "Cole aqui o certificado mostrado no servidor em Configurações → Rede. Vazio = confiar nos certificados do sistema.",
            )),
            |ui| {
                ui::text_area(ui, &mut self.ca_pem, 5);
                if !self.ca_pem.trim().is_empty() {
                    match &local_fp {
                        Some(fp) => {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(tr("Impressão digital SHA-256:"))
                                        .small()
                                        .weak(),
                                );
                                ui::mono(ui, fp);
                            });
                        }
                        None => ui::error_text(
                            ui,
                            tr("Não é um certificado PEM válido (esperado -----BEGIN CERTIFICATE-----)."),
                        ),
                    }
                }
            },
        );
        ui::field(
            ui,
            tr("Impressão digital no servidor"),
            Some(tr(
                "Lê GET /api/health sem validar o certificado, só para você conferir.",
            )),
            |ui| {
                ui.horizontal(|ui| {
                    if fetching {
                        ui::busy(ui, tr("consultando…"));
                    } else if ui.button(tr("Buscar do servidor")).clicked() {
                        self.start_fetch_health(cx);
                    }
                });
                if let Some(f) = &self.fetched {
                    ui.add_space(4.0);
                    if f.scheme != "https" {
                        ui.label(
                            egui::RichText::new(trf(
                                "{} responde em http (sem TLS): nenhum certificado a fixar.",
                                &[&f.url],
                            ))
                            .small()
                            .weak(),
                        );
                    } else if let Some(fp) = &f.fingerprint {
                        ui::mono(ui, fp);
                        match &local_fp {
                            Some(l) if norm_fp(l) == norm_fp(fp) => {
                                ui::badge(ui, tr("bate com o PEM colado"), Tone::Success)
                            }
                            Some(_) => ui::badge(ui, tr("não bate com o PEM colado"), Tone::Danger),
                            None => {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(tr(
                                            "Confira se esta impressão digital é a mesma que o servidor mostra em Configurações → Rede (ou `will-ia info`). Se bater, copie lá o PEM do certificado e cole no campo acima.",
                                        ))
                                        .small()
                                        .weak(),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(tr(
                                "O servidor usa https mas não informou a impressão digital (versão antiga?).",
                            ))
                            .small()
                            .weak(),
                        );
                    }
                }
            },
        );
        ui::field(
            ui,
            tr("Aceitar qualquer certificado (inseguro)"),
            Some(tr(
                "Só para testes: sem verificação, alguém no caminho pode ler tudo.",
            )),
            |ui| {
                ui::toggle(ui, &mut self.insecure);
                if self.insecure {
                    ui.colored_label(
                        Tone::Warning.color(dark),
                        tr("Inseguro: o app não vai verificar a identidade do servidor."),
                    );
                }
            },
        );
        if self.https_without_trust() {
            ui.colored_label(
                Tone::Warning.color(dark),
                tr("https:// sem certificado fixado: a conexão só funciona se o sistema confiar no certificado do servidor. Cole o PEM em \"TLS (https)\" ou marque o modo inseguro."),
            );
        }
    }

    /// Cartão "tentando reconectar": há perfil escolhido mas o servidor não
    /// responde. Cancelar desiste (o perfil continua salvo).
    fn reconnect_card(&mut self, ui: &mut egui::Ui, cx: &mut Cx, current: &Profile) {
        ui::section(ui, tr("Servidor atual"), |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(trf(
                    "Tentando reconectar em {} ({})…",
                    &[&current.name, &current.url],
                ));
            });
            if let Some(e) = &self.last_error {
                ui.add_space(4.0);
                ui::error_text(ui, e);
            }
            ui.add_space(8.0);
            if ui
                .button(tr("Cancelar"))
                .on_hover_text(tr("Desiste de reconectar; o perfil continua salvo."))
                .clicked()
            {
                cx.svc.disconnect();
                self.last_error = None;
            }
        });
    }

    /// Aba Perfis: "Esta máquina" + perfis salvos, com Conectar/Entrar,
    /// Editar e Remover.
    fn profiles_tab(&mut self, ui: &mut egui::Ui, cx: &mut Cx, current: &Profile) {
        let mut connect: Option<Profile> = None;
        let mut edit: Option<Profile> = None;
        // "Esta máquina" é detectado, não salvo: Editar/Entrar abrem o formulário
        // com URL e token locais e salvam como um perfil novo (editável).
        let mut edit_local: Option<Profile> = None;
        let local = self.local.clone();
        let saved = self.file.profiles.clone();
        ui::section(ui, tr("Perfis"), |ui| {
            match &local {
                Some(p) => {
                    ui.horizontal(|ui| {
                        ui::dot(ui, Tone::Info);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(tr("Esta máquina")).strong());
                            ui::mono(ui, &p.url);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_enabled_ui(current.id != p.id, |ui| {
                                if ui::primary_button(ui, tr("Conectar")).clicked() {
                                    connect = Some(p.clone());
                                }
                            });
                            if ui.button(tr("Editar")).on_hover_text(tr("Entrar com usuário e senha neste servidor; vira um perfil salvo.")).clicked() {
                                edit_local = Some(p.clone());
                            }
                        });
                    });
                }
                None => {
                    ui.label(
                        egui::RichText::new(tr(
                            "Nenhum servidor will-ia nesta máquina (inicie o `will-ia` para ele aparecer aqui).",
                        ))
                        .weak(),
                    );
                }
            }
            if saved.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(tr("Nenhum perfil salvo. Crie um na aba \"Novo perfil\"."))
                        .weak(),
                );
            }
            for p in &saved {
                ui.separator();
                ui.horizontal(|ui| {
                    ui::dot(
                        ui,
                        if current.id == p.id {
                            Tone::Success
                        } else {
                            Tone::Neutral
                        },
                    );
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&p.name).strong());
                        ui.horizontal_wrapped(|ui| {
                            ui::mono(ui, &p.url);
                            if let Some(u) = &p.username {
                                ui.label(egui::RichText::new(u).weak().small());
                            }
                            let no_pass =
                                p.password.as_deref().map(|s| s.is_empty()).unwrap_or(true);
                            if p.token.is_empty() && no_pass {
                                ui.label(
                                    egui::RichText::new(tr(
                                        "sem senha guardada: Conectar pede login",
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::danger_button(ui, tr("Remover")).clicked() {
                            self.remove_pending = Some(p.clone());
                        }
                        if ui.button(tr("Editar")).clicked() {
                            edit = Some(p.clone());
                        }
                        // "Conectar" sempre: se não há token guardado mas há
                        // usuário e senha, reautentica sozinho (ver connect_to).
                        ui.add_enabled_ui(current.id != p.id, |ui| {
                            if ui::primary_button(ui, tr("Conectar")).clicked() {
                                connect = Some(p.clone());
                            }
                        });
                    });
                });
            }
        });
        if let Some(p) = edit {
            self.open_login(&p);
        }
        if let Some(p) = edit_local {
            self.open_login(&p);
            // Novo perfil salvo (não sobrescreve o detectado): id novo, nome sugerido.
            self.edit_id = None;
            self.name = tr("Este servidor").into();
        }
        if let Some(p) = connect {
            self.connect_to(cx, p);
        }
    }

    /// Aba Novo perfil / Editar perfil: o formulário de login e de perfil.
    fn form_tab(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let logging_in = self.login_task.as_ref().is_some_and(|t| t.pending());
        let mut do_login = false;
        ui::section(
            ui,
            if self.edit_id.is_some() {
                tr("Editar perfil")
            } else {
                tr("Novo perfil")
            },
            |ui| {
                ui::field(ui, tr("Nome"), None, |ui| {
                    ui::text_input(ui, &mut self.name, tr("ex.: servidor da equipe"));
                });
                ui::field(
                    ui,
                    tr("URL do servidor"),
                    Some(tr("http://host:porta ou https://…")),
                    |ui| {
                        ui::text_input(ui, &mut self.url, "http://192.168.0.10:47300");
                    },
                );
                ui::field(
                    ui,
                    tr("Usuário"),
                    Some(tr("Conta criada pelo administrador do servidor.")),
                    |ui| {
                        ui::text_input(ui, &mut self.username, "admin");
                    },
                );
                ui::field(
                    ui,
                    tr("Senha"),
                    Some(tr("Guardada no perfil para reconectar sem digitar de novo (fica em claro no profiles.json do cliente).")),
                    |ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.password).margin(crate::ui::input_margin())
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_login = true;
                        }
                    },
                );
                ui.horizontal(|ui| {
                    if logging_in {
                        ui::busy(ui, tr("entrando…"));
                    } else if ui::primary_button(ui, tr("Entrar")).clicked() {
                        do_login = true;
                    }
                    if ui.button(tr("Salvar perfil")).clicked() {
                        match self.save_form() {
                            Ok(()) => {
                                cx.toasts.success(tr("Perfil salvo."));
                                self.tab = LandingTab::Profiles;
                            }
                            Err(e) => cx.toasts.error(e),
                        }
                    }
                    if ui.button(tr("Cancelar")).clicked() {
                        self.clear_form();
                        self.tab = LandingTab::Profiles;
                    }
                });
                ui.add_space(6.0);
                ui::field(
                    ui,
                    tr("Token de sessão"),
                    Some(tr("Opcional: o token local da API (`will-ia info`) para quem não usa conta. Entrar preenche este campo sozinho.")),
                    |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.token).margin(crate::ui::input_margin()).password(true).desired_width(f32::INFINITY));
                    },
                );
                ui.add_space(6.0);
                self.tls_form(ui, cx);
                ui.label(
                    egui::RichText::new(trf(
                        "Perfis ficam em {}",
                        &[&profiles::config_dir().join("profiles.json").display()],
                    ))
                    .small()
                    .weak(),
                );
            },
        );
        if do_login && !logging_in {
            self.start_login(cx);
        }
    }

    /// Modal de confirmação da remoção de um perfil salvo.
    fn remove_modal(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        let Some(p) = self.remove_pending.clone() else {
            return;
        };
        let c = Confirm {
            title: tr("Remover perfil").into(),
            body: trf(
                "Remover o perfil '{}' ({})? O token de sessão guardado nele é apagado.",
                &[&p.name, &p.url],
            ),
            confirm_label: tr("Remover").into(),
            danger: true,
            tag: "remove-profile".into(),
        };
        match ui::confirm_modal(ctx, &c) {
            Some(true) => {
                self.remove_pending = None;
                self.file.profiles.retain(|x| x.id != p.id);
                if let Err(e) = profiles::save(&self.file) {
                    cx.toasts.error(e);
                }
                if self.edit_id.as_deref() == Some(p.id.as_str()) {
                    self.clear_form();
                }
            }
            Some(false) => self.remove_pending = None,
            None => {}
        }
    }

    /// Tela de entrada inteira (título, abas e conteúdo), centralizada. O app
    /// desenha isto no painel central quando não há conexão.
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let current = cx.svc.profile();
        self.poll(cx);

        egui::ScrollArea::vertical()
            .id_salt("landing")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let full = ui.available_width();
                let w = full.min(LANDING_WIDTH);
                let pad = ((full - w) / 2.0).max(0.0);
                ui.add_space(28.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(pad);
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.set_min_width(w);
                        ui::heading(
                            ui,
                            tr("Entrar no will-ia"),
                            Some(tr(
                                "O app é um cliente: escolha o servidor will-ia ao qual conectar.",
                            )),
                        );
                        ui.add_space(6.0);
                        let form_label = if self.edit_id.is_some() {
                            tr("Editar perfil")
                        } else {
                            tr("Novo perfil")
                        };
                        let selected = match self.tab {
                            LandingTab::Profiles => "profiles",
                            LandingTab::Form => "form",
                        };
                        match ui::tab_bar(
                            ui,
                            &[
                                ui::Tab::new("profiles", tr("Perfis")),
                                ui::Tab::new("form", form_label),
                            ],
                            selected,
                        ) {
                            Some("form") => self.tab = LandingTab::Form,
                            Some(_) => self.tab = LandingTab::Profiles,
                            None => {}
                        }
                        match self.tab {
                            LandingTab::Profiles => {
                                if cx.svc.has_profile() {
                                    self.reconnect_card(ui, cx, &current);
                                }
                                self.profiles_tab(ui, cx, &current);
                            }
                            LandingTab::Form => self.form_tab(ui, cx),
                        }
                        ui.add_space(24.0);
                    });
                });
            });

        let ctx = ui.ctx().clone();
        self.remove_modal(&ctx, cx);
    }
}
