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
        // Decorações do próprio SO (barra de título, minimizar/maximizar/fechar).
        .with_decorations(true)
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
        // macOS: microfone + notificações pedidos de uma vez na primeira abertura.
        pages.settings.status.permissions.first_run_pass();
        if svc.has_profile() {
            pages.settings.state.load(&svc);
        }
        if let Some(p) = &prefill {
            pages.connect.open_login(p);
        }
        // Sem conexão a tela de entrada cobre tudo; `nav` só importa depois
        // (o modal de troca de senha aparece sobre qualquer página).
        let nav = if !snap.connected || settings.general.onboarding_done || !snap.is_admin() {
            Nav::Threads
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
                // Só o token de sessão (morto após o logout) é apagado; usuário e
                // senha ficam para o botão "Conectar" reautenticar sozinho.
                saved.token.clear();
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
        self.nav = Nav::Threads;
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
        // macOS 14+: `Focus` vira `activateIgnoringOtherApps:`, que o sistema nega
        // quando outro app está na frente e converte em ícone pulando no Dock.
        // Fora do foco, quem ativa é o LaunchServices (`open -a` no próprio
        // bundle); `Focus` fica para quando já estamos ativos ou fora do bundle.
        #[cfg(target_os = "macos")]
        let focus = will_platform::macos::is_active() || !will_platform::macos::activate_self();
        #[cfg(not(target_os = "macos"))]
        let focus = true;
        if focus {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        ctx.request_repaint();
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
                // Janela oculta: notificação nativa na máquina do app. Com o
                // servidor nesta mesma máquina ele já notificou (tem desktop);
                // só um servidor remoto precisa do app para isso.
                if !self.window_visible && !self.snap.server_is_local {
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
                // Threads (ou onboarding, decidido quando as settings chegarem).
                self.nav = Nav::Threads;
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
                // `pagina` ou `pagina/aba` (ex.: `google/gmail`, `browser/traffic`).
                let (page, tab) = match nav.as_deref().and_then(|n| n.split_once('/')) {
                    Some((p, t)) if p != "threads" => (Some(p), Some(t)),
                    _ => (nav.as_deref(), None),
                };
                if let Some(n) = page.and_then(Nav::parse) {
                    self.nav = n;
                    if let (Some(p), Some(t)) = (page, tab) {
                        ui::select_tab(ctx, p, t);
                    } else if matches!(page, Some("dashboard") | Some("status")) {
                        // antigo "Início": aba Status de Configurações → Geral
                        ui::select_tab(ctx, "general", "status");
                    }
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
            self.nav = Nav::Threads;
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
            self.pages.settings.status.refresh(&self.svc);
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

        // Fechar a janela (botão nativo do SO) encerra o app: ele é só o cliente;
        // o servidor `will-ia` continua residente e reabrir o app reconecta.

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
            self.toasts.show(&ctx);
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
                        ui::select_tab(ui.ctx(), "approvals", "pending");
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
        self.toasts.show(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tracing::info!("saindo do will-ia");
    }
}
