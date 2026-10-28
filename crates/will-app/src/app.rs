//! Janela principal (eframe::App): navegação lateral, páginas, eventos do
//! serviço, bandeja, atalho global, voz e indicador flutuante.

use crate::hotkey::Hotkeys;
use crate::i18n::{tr, trf};
use crate::pages::{Cx, Nav, Pages, SettingsSection, Snapshot};
use crate::profiles::Profile;
use crate::service::{Service, UiCommand};
use crate::ui::{self, Toasts, Tone};
use crate::voice::VoiceController;
use serde_json::Value;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};
use will_client::ClientEvent;
use will_core::{Event, Module, Principal, Settings};

pub fn run(profile: Option<Profile>, visible: bool) -> anyhow::Result<()> {
    // Perfil sem token (nunca entrou, ou saiu do perfil): nada responde nele,
    // então o app abre na tela de entrada com o formulário já preenchido.
    let (profile, prefill) = match profile {
        Some(p) if !p.token.is_empty() => (p, None),
        Some(p) => (Profile::none(), Some(p)),
        None => (Profile::none(), None),
    };
    let (svc, ev_rx, cmd_rx) = Service::new(profile)?;
    crate::app_ipc::serve(svc.clone());
    // Snapshot inicial pela API (o app é cliente mesmo em loopback).
    // Sem perfil o snapshot nasce vazio (`connected = false`): tela de entrada.
    let snap = if svc.has_profile() {
        initial_snapshot(&svc)
    } else {
        Snapshot::default()
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title("will-ia")
        .with_app_id("will-ia")
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([900.0, 600.0])
        .with_icon(crate::icon::egui_icon())
        // Barra de título desenhada pelo app (integrada ao fundo); ver `title_bar`.
        .with_decorations(false)
        .with_visible(visible);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let svc2 = svc.clone();
    let result = eframe::run_native(
        "will-ia",
        options,
        Box::new(move |cc| {
            Ok(Box::new(WillApp::new(
                cc, svc2, snap, ev_rx, cmd_rx, visible, prefill,
            )))
        }),
    );
    tracing::info!("janela encerrada");
    svc.shutdown();
    result.map_err(|e| anyhow::anyhow!("GUI: {e}"))
}

/// Lê `/api/status`, `/api/settings` e `/api/approvals` de uma vez (bloqueante;
/// só no arranque). Falhas viram snapshot vazio com `connected = false`.
fn initial_snapshot(svc: &Arc<Service>) -> Snapshot {
    let api = svc.api();
    let api2 = api.clone();
    let mut snap = Snapshot {
        api_url: api.base_url().to_string(),
        server_is_local: api.is_local(),
        ..Default::default()
    };
    let Some((status, settings, approvals, me)) = svc.block_on(async move {
        tokio::join!(api2.status(), api2.settings(), api2.approvals(), api2.me())
    }) else {
        return snap;
    };
    if let Ok(s) = settings {
        if let Ok(st) = serde_json::from_value::<Settings>(s) {
            snap.settings = st;
        }
    }
    if let Ok(st) = status {
        apply_status(&mut snap, &st);
        snap.connected = true;
    }
    if let Ok(a) = approvals {
        if let Ok(list) = serde_json::from_value(a.get("pending").cloned().unwrap_or(a.clone())) {
            snap.pending_approvals = list;
        }
    }
    if let Ok(m) = me {
        snap.principal = serde_json::from_value::<Principal>(m).ok();
    }
    snap
}

/// Copia para o snapshot o que vem de `/api/status`: módulos e caminhos do servidor.
pub fn apply_status(snap: &mut Snapshot, st: &Value) {
    if let Some(m) = st.get("modules").and_then(|m| m.as_object()) {
        for (k, v) in m {
            if let (Some(module), Ok(status)) = (
                Module::all().iter().find(|x| x.as_str() == k).copied(),
                serde_json::from_value::<will_core::ModuleStatus>(v.clone()),
            ) {
                snap.modules.insert(module, status);
            }
        }
    }
    let path = |k: &str| {
        st.get(k)
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
    };
    if let Some(p) = path("data_dir") {
        snap.data_dir = p;
    }
    if let Some(p) = path("logs_dir") {
        snap.logs_dir = p;
    }
    if let Some(p) = path("db_path") {
        snap.db_path = p;
    }
    snap.portable_root = path("portable_root");
}

pub struct WillApp {
    svc: Arc<Service>,
    ev_rx: Receiver<ClientEvent>,
    cmd_rx: Receiver<UiCommand>,
    pub nav: Nav,
    pub snap: Snapshot,
    pub pages: Pages,
    pub toasts: Toasts,
    pub voice: VoiceController,
    hotkeys: Hotkeys,
    window_visible: bool,
    last_status_refresh: Instant,
    theme_applied: Option<String>,
    hotkey_applied: Option<String>,
    pending_hide: bool,
    /// Captura agendada (caminho, quadros restantes) para dar tempo à navegação.
    pending_screenshot: Option<(std::path::PathBuf, u8)>,
    /// Recarga das settings do snapshot depois de `SettingsChanged`.
    settings_task: Option<crate::service::Task<Value>>,
    /// Recarga de status/aprovações depois de reconectar.
    resync_task: Option<crate::service::Task<(Value, Value)>>,
    /// `GET /api/auth/me` depois de conectar (e depois de trocar a senha).
    me_task: Option<crate::service::Task<Value>>,
    /// Acabou de conectar: quando as settings chegarem, escolhe Início ou o
    /// onboarding (Motor Claude).
    pick_nav_on_settings: bool,
    /// "Sair do perfil" clicado no menu; tratado no fim do quadro.
    pending_leave: bool,
}

impl WillApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        svc: Arc<Service>,
        snap: Snapshot,
        ev_rx: Receiver<ClientEvent>,
        cmd_rx: Receiver<UiCommand>,
        visible: bool,
        prefill: Option<Profile>,
    ) -> Self {
        svc.set_ctx(cc.egui_ctx.clone());
        egui_extras::install_image_loaders(&cc.egui_ctx);
        crate::ui::setup(&cc.egui_ctx);
        let hotkeys = Hotkeys::new(&svc);
        let settings = snap.settings.clone();
        crate::i18n::set_language(&settings.general.language);
        let mut voice = VoiceController::new();
        voice.apply_settings(&settings.realtime);
        let mut pages = Pages::default();
        if svc.has_profile() {
            pages.settings.state.load(&svc);
        }
        if let Some(p) = &prefill {
            pages.connect.open_login(p);
        }
        // Sem conexão a tela de entrada cobre tudo; `nav` só importa depois
        // (o modal de troca de senha aparece sobre qualquer página).
        let nav = if !snap.connected || settings.general.onboarding_done || !snap.is_admin() {
            Nav::Dashboard
        } else {
            Nav::Settings(SettingsSection::Engine)
        };
        let mut app = Self {
            svc,
            ev_rx,
            cmd_rx,
            nav,
            snap,
            pages,
            toasts: Toasts::default(),
            voice,
            hotkeys,
            window_visible: visible,
            last_status_refresh: Instant::now() - Duration::from_secs(60),
            theme_applied: None,
            hotkey_applied: None,
            pending_hide: false,
            pending_screenshot: None,
            settings_task: None,
            resync_task: None,
            me_task: None,
            pick_nav_on_settings: false,
            pending_leave: false,
        };
        app.apply_theme(&cc.egui_ctx);
        app
    }

    /// Sair do perfil: encerra a sessão da conta no servidor (se houver),
    /// esquece o token do perfil salvo, desconecta e volta à tela de entrada.
    fn leave_profile(&mut self) {
        let profile = self.svc.profile();
        if !self.svc.has_profile() {
            return;
        }
        if self
            .snap
            .principal
            .as_ref()
            .is_some_and(|p| p.user_id.is_some())
        {
            // Resultado ignorado: localmente o token some de qualquer jeito.
            let _ = self.svc.call(|c| async move { c.logout().await });
        }
        // "Esta máquina" usa o token que o servidor local grava; só perfis
        // salvos têm token próprio a apagar.
        if profile.id != Profile::LOCAL_ID {
            let mut pf = crate::profiles::load();
            if let Some(saved) = pf.profiles.iter_mut().find(|x| x.id == profile.id) {
                saved.token.clear();
                saved.username = None;
                if let Err(e) = crate::profiles::save(&pf) {
                    self.toasts.error(e);
                }
            }
        }
        self.voice.stop(&self.svc);
        self.svc.disconnect();
        self.snap = Snapshot::default();
        self.pages = Pages::default();
        self.settings_task = None;
        self.resync_task = None;
        self.me_task = None;
        self.pick_nav_on_settings = false;
        self.nav = Nav::Dashboard;
        self.toasts.info(tr("Sessão encerrada."));
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let scale = self.snap.settings.general.ui_scale.clamp(0.6, 2.0);
        if (ctx.zoom_factor() - scale).abs() > 0.01 {
            ctx.set_zoom_factor(scale);
        }
        let theme = self.snap.settings.general.theme.clone();
        if self.theme_applied.as_deref() == Some(theme.as_str()) {
            return;
        }
        ctx.set_theme(match theme.as_str() {
            "dark" => egui::ThemePreference::Dark,
            "light" => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        });
        self.theme_applied = Some(theme);
    }

    fn apply_hotkey(&mut self) {
        let hk = if self.snap.settings.realtime.enabled {
            self.snap.settings.realtime.hotkey.clone()
        } else {
            String::new()
        };
        if self.hotkey_applied.as_deref() == Some(hk.as_str()) {
            return;
        }
        self.hotkeys.apply(&hk);
        if let Some(e) = &self.hotkeys.error {
            self.toasts.error(trf("Atalho global: {}", &[e]));
        }
        self.hotkey_applied = Some(hk);
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_visible = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.window_visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn handle_event(&mut self, ev: Event) {
        match &ev {
            Event::ModuleStatus { module, status } => {
                // `Running` em Voz significa "pronta", não "em sessão": a bandeja
                // acompanha os eventos `Voice` (abaixo).
                self.snap.modules.insert(*module, status.clone());
            }
            Event::ApprovalRequested { approval } => {
                self.snap.pending_approvals.retain(|a| a.id != approval.id);
                self.snap.pending_approvals.push(approval.clone());
                self.toasts.push(
                    Tone::Warning,
                    trf("Aprovação pedida: {}", &[&approval.summary]),
                );
            }
            Event::ApprovalDecided { approval_id, .. } => {
                self.snap.pending_approvals.retain(|a| a.id != *approval_id);
            }
            Event::SettingsChanged { .. } => {
                self.settings_task = Some(self.svc.call(|c| async move { c.settings().await }));
                self.pages.settings.state.load(&self.svc);
            }
            Event::Voice {
                state,
                detail,
                user,
            } => {
                // Uma sessão de voz por usuário: só reage à própria.
                let me = self
                    .snap
                    .principal
                    .as_ref()
                    .map(|p| p.username.clone())
                    .unwrap_or_else(|| "local".into());
                if user.as_deref().map(|u| u == me).unwrap_or(true) {
                    self.voice.on_voice_event(state, detail.as_deref());
                }
            }
            Event::Notification { title, body } => {
                self.toasts.info(format!("{title}: {body}"));
                // Janela oculta: notificação nativa na máquina do app (o servidor
                // só notifica quando tem desktop).
                if !self.window_visible {
                    will_platform::notify(title, body);
                }
            }
            Event::CostRecorded {
                source, summary, ..
            } => {
                if source == "voice" {
                    self.toasts.info(summary.clone());
                }
            }
            Event::StaleSession { .. } => {
                self.toasts.info(tr(
                    "Sessão do Claude expirou; a thread recomeçou e reenviou as pendências.",
                ));
            }
            Event::TurnFailed { error, .. } => {
                self.toasts.error(error.clone());
            }
            Event::ScheduleFired {
                name, ok, detail, ..
            } => {
                if *ok {
                    self.toasts
                        .success(trf("Agendamento \"{}\" disparou", &[name]));
                } else {
                    let why = detail
                        .clone()
                        .unwrap_or_else(|| tr("erro desconhecido").into());
                    self.toasts
                        .error(trf("Agendamento \"{}\" falhou: {}", &[name, &why]));
                }
            }
            Event::Browser { state, detail } => {
                // Só os marcos que interessam a quem não está na página; o resto
                // (ws_open/ws_closed, listening) aparece na própria página.
                let text = match state.as_str() {
                    "launched" => Some(tr("Browser CDP: navegador lançado.")),
                    "browser_up" => Some(tr("Browser CDP: navegador respondendo.")),
                    "closed" => Some(tr("Browser CDP: navegador fechado.")),
                    _ => None,
                };
                if let Some(t) = text {
                    match detail {
                        Some(d) if !d.is_empty() => self.toasts.info(format!("{t} {d}")),
                        _ => self.toasts.info(t),
                    }
                }
            }
            _ => {}
        }
        self.pages.on_event(&ev);
    }

    fn handle_cmd(&mut self, ctx: &egui::Context, cmd: UiCommand) {
        match cmd {
            UiCommand::Show => self.show_window(ctx),
            UiCommand::Connect(profile) => {
                self.voice.stop(&self.svc);
                self.svc.connect(profile.clone());
                self.snap = Snapshot {
                    api_url: profile.url.clone(),
                    server_is_local: self.svc.api().is_local(),
                    ..Default::default()
                };
                self.pages = Pages::default();
                self.pages.settings.state.load(&self.svc);
                self.settings_task = None;
                self.resync_task = None;
                self.me_task = None;
                // A tela de entrada fica até `ClientEvent::Connected`; depois
                // Início (ou onboarding, decidido quando as settings chegarem).
                self.nav = Nav::Dashboard;
                self.toasts.info(trf("Conectando em {}…", &[&profile.name]));
            }
            UiCommand::HotkeyDown => {
                if self.voice.ptt_mode {
                    if !self.voice.session_active && !self.voice.busy() {
                        self.voice.start(&self.svc);
                    } else {
                        self.voice.set_ptt(&self.svc, true);
                    }
                } else {
                    self.voice.toggle(&self.svc);
                }
            }
            UiCommand::HotkeyUp => {
                if self.voice.ptt_mode {
                    self.voice.set_ptt(&self.svc, false);
                }
            }
            UiCommand::Screenshot(path, nav) => {
                if let Some(n) = nav.as_deref().and_then(Nav::parse) {
                    self.nav = n;
                } else if let Some(id) = nav.as_deref().and_then(|n| n.strip_prefix("threads/")) {
                    // diagnóstico: abre a página de threads já com a thread selecionada
                    if let Ok(t) = will_core::ThreadId::parse(id) {
                        self.nav = Nav::Threads;
                        self.pages.threads.select(t);
                    }
                }
                self.pending_screenshot = Some((path, 3));
            }
            UiCommand::ScreenshotNow(path) => {
                let cb =
                    egui::ScreenshotCallback::new(move |img: std::sync::Arc<egui::ColorImage>| {
                        let [w, h] = img.size;
                        let mut rgba = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            rgba.extend_from_slice(&px.to_srgba_unmultiplied());
                        }
                        match image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                            Some(im) => {
                                if let Err(e) = im.save(&path) {
                                    tracing::warn!(error = %e, "screenshot: falha ao salvar");
                                } else {
                                    tracing::info!(path = %path.display(), "screenshot salvo");
                                }
                            }
                            None => tracing::warn!("screenshot: imagem inválida"),
                        }
                    });
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
            }
        }
    }

    /// Pede ao servidor quem somos (`GET /api/auth/me`).
    fn load_principal(&mut self) {
        self.me_task = Some(self.svc.call(|c| async move { c.me().await }));
    }

    /// Aplica a identidade carregada: troca obrigatória de senha abre o modal
    /// (`ConnectPage::password_gate`, sobre qualquer página); membro não fica
    /// numa página de administrador.
    fn apply_principal(&mut self, p: Principal) {
        let must_change = p.must_change_password;
        let admin = p.is_admin();
        self.snap.principal = Some(p);
        if must_change {
            self.toasts
                .info(tr("Primeiro acesso: defina uma nova senha para continuar."));
        }
        if !admin && matches!(self.nav, Nav::Settings(_) | Nav::Users) {
            self.nav = Nav::Dashboard;
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        while let Ok(ev) = self.ev_rx.try_recv() {
            events.push(ev);
        }
        for ev in events {
            match ev {
                ClientEvent::Event(ev) => self.handle_event(ev),
                ClientEvent::Connected => {
                    if !self.svc.has_profile() {
                        // Evento atrasado de um WS já abortado (saiu do perfil).
                        continue;
                    }
                    let was = self.snap.connected;
                    self.snap.connected = true;
                    self.pages.connect.last_error = None;
                    self.pages.connect.refresh();
                    if !was {
                        self.pick_nav_on_settings = true;
                        // Ressincroniza o que pode ter mudado enquanto estávamos fora.
                        self.resync_task = Some(self.svc.call(|c| async move {
                            let st = c.status().await?;
                            let ap = c.approvals().await?;
                            Ok((st, ap))
                        }));
                        self.settings_task =
                            Some(self.svc.call(|c| async move { c.settings().await }));
                        self.load_principal();
                    }
                }
                ClientEvent::Disconnected(why) => {
                    if !self.svc.has_profile() {
                        continue;
                    }
                    if self.snap.connected {
                        self.toasts
                            .error(trf("Conexão com o servidor perdida: {}", &[&why]));
                    }
                    self.snap.connected = false;
                    self.pages.connect.last_error = Some(why);
                }
            }
        }
        if let Some(t) = &self.settings_task {
            if let Some(r) = t.take() {
                self.settings_task = None;
                if let Ok(v) = r {
                    if let Ok(st) = serde_json::from_value::<Settings>(v) {
                        self.snap.settings = st;
                        self.voice.apply_settings(&self.snap.settings.realtime);
                    }
                }
                if std::mem::take(&mut self.pick_nav_on_settings)
                    && !self.snap.settings.general.onboarding_done
                    && self.snap.is_admin()
                {
                    // Primeira conexão num servidor ainda sem onboarding.
                    self.nav = Nav::Settings(SettingsSection::Engine);
                }
            }
        }
        if let Some(t) = &self.resync_task {
            if let Some(r) = t.take() {
                self.resync_task = None;
                if let Ok((st, ap)) = r {
                    apply_status(&mut self.snap, &st);
                    if let Ok(list) =
                        serde_json::from_value(ap.get("pending").cloned().unwrap_or(ap))
                    {
                        self.snap.pending_approvals = list;
                    }
                }
            }
        }
        if self.pages.connect.principal_dirty {
            self.pages.connect.principal_dirty = false;
            self.load_principal();
        }
        if let Some(t) = &self.me_task {
            if let Some(r) = t.take() {
                self.me_task = None;
                if let Ok(v) = r {
                    if let Ok(p) = serde_json::from_value::<Principal>(v) {
                        self.apply_principal(p);
                    }
                }
            }
        }
        let mut cmds = Vec::new();
        while let Ok(c) = self.cmd_rx.try_recv() {
            cmds.push(c);
        }
        for c in cmds {
            self.handle_cmd(ctx, c);
        }
        if let Some((path, n)) = self.pending_screenshot.take() {
            if n == 0 {
                self.handle_cmd(ctx, UiCommand::ScreenshotNow(path));
            } else {
                self.pending_screenshot = Some((path, n - 1));
                ctx.request_repaint();
            }
        }
        if let Some(r) = self.voice.tick(&self.svc) {
            match r {
                Ok(m) => self.toasts.success(m),
                Err(e) => self.toasts.error(e),
            }
        }
        if self.svc.has_profile() && self.last_status_refresh.elapsed() > Duration::from_secs(15) {
            self.last_status_refresh = Instant::now();
            self.pages.dashboard.refresh(&self.svc);
        }
        self.apply_hotkey();
    }

    fn nav_panel(&mut self, ui: &mut egui::Ui) {
        let voice_active = self.voice.session_active;
        let pending = self.snap.pending_approvals.len();
        // Rodapé fixo: voz, ocultar/sair, versão.
        egui::Panel::bottom("nav-bottom")
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(6.0);
                let (label, tone) = if voice_active {
                    (tr("Desligar voz"), Tone::Danger)
                } else {
                    (tr("Ligar voz"), Tone::Success)
                };
                let btn = egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                    .fill(tone.color(ui.visuals().dark_mode))
                    .min_size(egui::vec2(ui.available_width(), 34.0));
                if ui
                    .add_enabled(
                        !self.voice.busy() && self.snap.settings.realtime.enabled,
                        btn,
                    )
                    .clicked()
                {
                    self.voice.toggle(&self.svc);
                }
                if self.voice.session_active {
                    let (label, color) = crate::voice::overlay::label_for(&self.voice.state);
                    ui.label(egui::RichText::new(label).color(color).small());
                }
                ui.add_space(6.0);
                // Perfil/conta em uso (o servidor e quem está logado).
                {
                    let p = self.svc.profile();
                    let who = match &self.snap.principal {
                        Some(pr) if pr.user_id.is_some() => format!("{} · {}", p.name, pr.username),
                        _ => p.name.clone(),
                    };
                    ui.add(egui::Label::new(egui::RichText::new(who).small().weak()).truncate())
                        .on_hover_text(&p.url);
                }
                // Sair do perfil: encerra a sessão da conta, esquece o token do
                // perfil e volta à tela de entrada (o servidor continua no ar).
                if ui
                    .add_sized(
                        [ui.available_width(), 30.0],
                        egui::Button::new(tr("Sair do perfil")),
                    )
                    .on_hover_text(tr(
                        "Encerra a sessão no servidor e apaga o token deste perfil.",
                    ))
                    .clicked()
                {
                    self.pending_leave = true;
                }
                ui.add_space(4.0);
                // Sem botões Ocultar/Sair: o serviço é residente (bandeja). Fechar a
                // janela só oculta; encerrar é só por kill forçado (docs/13).
                let mut info = format!("v{}", env!("CARGO_PKG_VERSION"));
                if self.snap.portable_root.is_some() {
                    info.push_str(tr(" · portátil"));
                }
                ui.label(egui::RichText::new(info).small().weak());
                ui.add_space(4.0);
            });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("will-ia").font(ui::semibold(18.0)));
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("nav-scroll")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 12.0).max(80.0))
            .show(ui, |ui| {
                let mut items: Vec<(Nav, String)> = vec![
                    (Nav::Dashboard, tr("Início").into()),
                    (Nav::Threads, tr("Threads").into()),
                    (Nav::Schedules, tr("Agendamentos").into()),
                    (Nav::Browser, tr("Browser CDP").into()),
                    (
                        Nav::Approvals,
                        if pending > 0 {
                            trf("Aprovações ({})", &[&pending])
                        } else {
                            tr("Aprovações").into()
                        },
                    ),
                    (Nav::Logs, tr("Logs").into()),
                    (Nav::Costs, tr("Custos").into()),
                ];
                // Usuários: só quando sabemos que quem está logado é administrador.
                if self.snap.principal.as_ref().is_some_and(|p| p.is_admin()) {
                    items.push((Nav::Users, tr("Usuários").into()));
                }
                for (n, label) in items {
                    if ui::nav_item(ui, self.nav == n, &label, None).clicked() {
                        self.nav = n;
                    }
                }
                // Configurações: rotas exigem administrador (`docs/26`). Sem
                // `principal` carregado ainda, mostra como antes.
                if !self.snap.is_admin() {
                    return;
                }
                ui.add_space(14.0);
                ui.label(egui::RichText::new(tr("CONFIGURAÇÕES")).small().weak());
                ui.add_space(2.0);
                for (sec, label) in SettingsSection::ALL.iter().map(|s| (*s, s.label())) {
                    let selected = self.nav == Nav::Settings(sec);
                    let status: Option<(Module, Option<&will_core::ModuleStatus>)> =
                        sec.module().map(|m| (m, self.snap.modules.get(&m)));
                    let dot_tone =
                        status.map(|(_, st)| st.map(ui::tone_for_module).unwrap_or(Tone::Neutral));
                    let mut resp = ui::nav_item(ui, selected, label, dot_tone);
                    if let Some((m, st)) = status {
                        // A bolinha explica a cor ao passar o mouse.
                        let text = match st {
                            Some(st) => ui::module_label(st),
                            None => tr("sem estado ainda").to_string(),
                        };
                        resp = resp.on_hover_text(format!("{}: {text}", ui::module_name(m)));
                    }
                    if resp.clicked() {
                        self.nav = Nav::Settings(sec);
                    }
                }
            });
    }

    /// Barra de título integrada: arrastar move a janela, duplo clique maximiza,
    /// botões minimizar / maximizar / fechar (fechar = ocultar para a bandeja).
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let h = 38.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
        let dark = ui.visuals().dark_mode;
        let bg = ui::palette(dark).titlebar;
        ui.painter().rect_filled(rect, 0.0, bg);
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        // área de arrastar (tudo menos os botões)
        let btn_w = 46.0;
        let drag_rect =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - btn_w * 3.0, rect.max.y));
        let drag = ui.interact(
            drag_rect,
            egui::Id::unique("titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        // título
        let icon_c = egui::pos2(rect.left() + 20.0, rect.center().y);
        ui.painter()
            .circle_filled(icon_c, 7.0, egui::Color32::from_rgb(31, 42, 68));
        ui.painter().circle_stroke(
            icon_c,
            4.2,
            egui::Stroke::new(1.4, egui::Color32::from_rgb(150, 170, 220)),
        );
        ui.painter()
            .circle_filled(icon_c, 1.6, egui::Color32::WHITE);
        let title_color = if dark {
            egui::Color32::from_rgb(200, 205, 215)
        } else {
            egui::Color32::from_rgb(40, 44, 54)
        };
        ui.painter().text(
            egui::pos2(rect.left() + 34.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "will-ia",
            egui::FontId::proportional(14.0),
            title_color,
        );
        if self.voice.session_active {
            let (label, color) = crate::voice::overlay::label_for(&self.voice.state);
            ui.painter().circle_filled(
                egui::pos2(rect.left() + 100.0, rect.center().y),
                4.0,
                color,
            );
            ui.painter().text(
                egui::pos2(rect.left() + 110.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                trf("voz · {}", &[&label]),
                egui::FontId::proportional(12.5),
                color,
            );
        }

        // botões
        let mut x = rect.max.x - btn_w;
        let stroke_color = title_color;
        for (i, kind) in ["close", "max", "min"].iter().enumerate() {
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(btn_w, h));
            let resp = ui.interact(
                r,
                egui::Id::unique(("titlebar-btn", i)),
                egui::Sense::click(),
            );
            if resp.hovered() {
                let fill = if *kind == "close" {
                    egui::Color32::from_rgb(200, 60, 60)
                } else if dark {
                    egui::Color32::from_rgb(45, 48, 58)
                } else {
                    egui::Color32::from_rgb(220, 223, 230)
                };
                ui.painter().rect_filled(r, 0.0, fill);
            }
            let c = r.center();
            let sc = if resp.hovered() && *kind == "close" {
                egui::Color32::WHITE
            } else {
                stroke_color
            };
            let st = egui::Stroke::new(1.3, sc);
            match *kind {
                "min" => {
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y + 0.5),
                            egui::pos2(c.x + 5.0, c.y + 0.5),
                        ],
                        st,
                    );
                }
                "max" => {
                    if maximized {
                        ui.painter().rect_stroke(
                            egui::Rect::from_center_size(
                                egui::pos2(c.x - 1.0, c.y + 1.0),
                                egui::vec2(8.0, 8.0),
                            ),
                            1.0,
                            st,
                            egui::StrokeKind::Middle,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x - 2.0, c.y - 3.0),
                                egui::pos2(c.x - 2.0, c.y - 4.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x - 2.0, c.y - 4.0),
                                egui::pos2(c.x + 5.0, c.y - 4.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x + 5.0, c.y - 4.0),
                                egui::pos2(c.x + 5.0, c.y + 3.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x + 5.0, c.y + 3.0),
                                egui::pos2(c.x + 4.0, c.y + 3.0),
                            ],
                            st,
                        );
                    } else {
                        ui.painter().rect_stroke(
                            egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
                            1.0,
                            st,
                            egui::StrokeKind::Middle,
                        );
                    }
                }
                _ => {
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y - 5.0),
                            egui::pos2(c.x + 5.0, c.y + 5.0),
                        ],
                        st,
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y + 5.0),
                            egui::pos2(c.x + 5.0, c.y - 5.0),
                        ],
                        st,
                    );
                }
            }
            if resp.clicked() {
                match *kind {
                    "min" => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    "max" => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => self.pending_hide = true,
                }
            }
            let tip = match *kind {
                "min" => tr("Minimizar"),
                "max" => {
                    if maximized {
                        tr("Restaurar")
                    } else {
                        tr("Maximizar")
                    }
                }
                _ => tr("Fechar (continua na bandeja)"),
            };
            resp.on_hover_text(tip);
            x -= btn_w;
        }
    }

    /// Redimensionar pelas bordas (a janela não tem moldura do sistema).
    fn resize_edges(&self, ctx: &egui::Context) {
        use egui::{CursorIcon, ResizeDirection as D, ViewportCommand as VC};
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
            return;
        };
        let r = ctx.content_rect();
        let m = 6.0;
        let left = pos.x <= r.left() + m;
        let right = pos.x >= r.right() - m;
        let top = pos.y <= r.top() + m;
        let bottom = pos.y >= r.bottom() - m;
        let dir = match (left, right, top, bottom) {
            (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNwSe)),
            (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNeSw)),
            (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeNeSw)),
            (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeNwSe)),
            (true, _, _, _) => Some((D::West, CursorIcon::ResizeHorizontal)),
            (_, true, _, _) => Some((D::East, CursorIcon::ResizeHorizontal)),
            (_, _, true, _) => Some((D::North, CursorIcon::ResizeVertical)),
            (_, _, _, true) => Some((D::South, CursorIcon::ResizeVertical)),
            _ => None,
        };
        if let Some((d, cursor)) = dir {
            ctx.set_cursor_icon(cursor);
            let pressed = ctx.input(|i| i.pointer.primary_pressed());
            if pressed {
                ctx.send_viewport_cmd(VC::BeginResize(d));
            }
        }
    }
}

impl eframe::App for WillApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);
        // Idioma da UI vem do snapshot (atualizado por `SettingsChanged`): salvar
        // a seção Geral troca todos os textos no quadro seguinte.
        crate::i18n::set_language(&self.snap.settings.general.language);
        if self.voice.overlay_visible() {
            crate::voice::overlay::show(ctx, &self.voice.overlay);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);

        // Fechar a janela nunca encerra o serviço: ele continua na bandeja (ou, sem
        // bandeja, em segundo plano; rodar `will-ia` de novo reabre a janela).
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(&ctx);
        }

        egui::Panel::top("titlebar")
            .exact_size(38.0)
            .show_separator_line(false)
            .show(ui, |ui| self.title_bar(ui));

        // Sem conexão (sem perfil, servidor fora do ar ou depois de sair do
        // perfil): só a tela de entrada, sem menu nem páginas. Ela some sozinha
        // quando `ClientEvent::Connected` chegar.
        if !self.snap.connected {
            let mut requested_nav: Option<Nav> = None;
            egui::CentralPanel::default().show(ui, |ui| {
                let mut cx = Cx {
                    svc: &self.svc,
                    snap: &self.snap,
                    toasts: &mut self.toasts,
                    voice: &mut self.voice,
                    go: &mut requested_nav,
                };
                self.pages.connect.ui(ui, &mut cx);
            });
            if self.pending_hide {
                self.pending_hide = false;
                self.hide_window(&ctx);
            }
            self.toasts.show(&ctx);
            self.resize_edges(&ctx);
            return;
        }

        egui::Panel::left("nav")
            .exact_size(232.0)
            .resizable(false)
            .show(ui, |ui| self.nav_panel(ui));

        // Faixa de aprovações pendentes.
        if !self.snap.pending_approvals.is_empty() && self.nav != Nav::Approvals {
            let n = self.snap.pending_approvals.len();
            egui::Panel::top("approvals-banner").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui::dot(ui, Tone::Warning);
                    ui.label(trf("{} aprovação(ões) pendente(s).", &[&n]));
                    if ui.button(tr("Ver")).clicked() {
                        self.nav = Nav::Approvals;
                    }
                });
            });
        }

        let mut requested_nav: Option<Nav> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            if matches!(self.nav, Nav::Threads | Nav::Logs) {
                // Threads e Logs ocupam a janela exatamente (cabeçalho, caixa de
                // entrada e barra de filtros fixos); só o histórico do chat ou o
                // painel de log rola, dentro da página.
                ui.set_clip_rect(ui.max_rect());
                ui.add_space(4.0);
                let mut cx = Cx {
                    svc: &self.svc,
                    snap: &self.snap,
                    toasts: &mut self.toasts,
                    voice: &mut self.voice,
                    go: &mut requested_nav,
                };
                self.pages.ui(ui, self.nav, &mut cx);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("page", self.nav))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let full = ui.available_width();
                    let max_w = full.min(ui::CONTENT_MAX_WIDTH);
                    let pad = ((full - max_w) / 2.0).max(0.0);
                    ui.add_space(4.0);
                    ui.horizontal_top(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                            ui.set_width(max_w);
                            ui.set_min_width(max_w);
                            let mut cx = Cx {
                                svc: &self.svc,
                                snap: &self.snap,
                                toasts: &mut self.toasts,
                                voice: &mut self.voice,
                                go: &mut requested_nav,
                            };
                            self.pages.ui(ui, self.nav, &mut cx);
                            ui.add_space(24.0);
                        });
                    });
                });
        });
        if let Some(n) = requested_nav {
            self.nav = n;
        }
        // Troca obrigatória de senha: modal bloqueante sobre qualquer página.
        {
            let mut go: Option<Nav> = None;
            let mut cx = Cx {
                svc: &self.svc,
                snap: &self.snap,
                toasts: &mut self.toasts,
                voice: &mut self.voice,
                go: &mut go,
            };
            self.pages.connect.password_gate(&ctx, &mut cx);
        }
        if self.pending_leave {
            self.pending_leave = false;
            self.leave_profile();
        }
        if self.pending_hide {
            self.pending_hide = false;
            self.hide_window(&ctx);
        }
        self.toasts.show(&ctx);
        self.resize_edges(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tracing::info!("saindo do will-ia");
    }
}
