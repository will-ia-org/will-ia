//! Ícone de bandeja: `ksni` (StatusNotifierItem/D-Bus) no Linux e `tray-icon`
//! nos demais SOs. Ações viram [`UiCommand`] no canal da GUI.

use crate::service::{Service, UiCommand};
use std::sync::Arc;

pub struct Tray {
    #[cfg(target_os = "linux")]
    handle: Option<ksni::Handle<linux::WillTray>>,
    #[cfg(not(target_os = "linux"))]
    inner: Option<other::Inner>,
}

impl Tray {
    pub fn new(svc: &Arc<Service>) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                handle: linux::spawn(svc),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self {
                inner: other::build(svc),
            }
        }
    }

    /// Atualiza o rótulo/ícone conforme a sessão de voz.
    pub fn set_voice_active(&self, active: bool, svc: &Arc<Service>) {
        #[cfg(target_os = "linux")]
        {
            if let Some(h) = self.handle.clone() {
                svc.spawn(async move {
                    h.update(|t| t.voice_active = active).await;
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = svc;
            if let Some(i) = &self.inner {
                i.set_voice_active(active);
            }
        }
    }

    pub fn available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.handle.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.inner.is_some()
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::i18n::tr;
    use ksni::TrayMethods;
    use std::sync::mpsc::Sender;

    pub struct WillTray {
        pub tx: Sender<UiCommand>,
        pub svc: std::sync::Weak<Service>,
        pub voice_active: bool,
    }

    impl WillTray {
        fn send(&self, c: UiCommand) {
            let _ = self.tx.send(c);
            if let Some(s) = self.svc.upgrade() {
                s.repaint();
            }
        }
    }

    impl ksni::Tray for WillTray {
        fn id(&self) -> String {
            "will-ia".into()
        }
        fn title(&self) -> String {
            "will-ia".into()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "will-ia".into(),
                description: if self.voice_active {
                    tr("voz ativa").into()
                } else {
                    String::new()
                },
                ..Default::default()
            }
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22u32, 32, 48]
                .iter()
                .map(|&s| ksni::Icon {
                    width: s as i32,
                    height: s as i32,
                    data: crate::icon::argb_network(s, self.voice_active),
                })
                .collect()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(UiCommand::Show);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;
            vec![
                StandardItem {
                    label: tr("Abrir will-ia").into(),
                    activate: Box::new(|t: &mut Self| t.send(UiCommand::Show)),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: if self.voice_active {
                        tr("Desligar voz").into()
                    } else {
                        tr("Ligar voz").into()
                    },
                    activate: Box::new(|t: &mut Self| t.send(UiCommand::ToggleVoice)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Alguns desktops (Cinnamon/MATE) trazem o `xapp-sn-watcher`, mas ele só sobe
    /// com o applet de status; se o watcher não estiver no ar, tentamos iniciá-lo.
    fn try_start_watcher() -> bool {
        for p in [
            "/usr/lib/x86_64-linux-gnu/xapps/xapp-sn-watcher",
            "/usr/libexec/xapps/xapp-sn-watcher",
            "/usr/lib/xapps/xapp-sn-watcher",
        ] {
            if std::path::Path::new(p).exists() {
                match std::process::Command::new(p)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_) => {
                        tracing::info!(watcher = p, "iniciando xapp-sn-watcher para a bandeja");
                        return true;
                    }
                    Err(e) => tracing::debug!(error = %e, "xapp-sn-watcher não iniciou"),
                }
            }
        }
        false
    }

    async fn register(svc: &Arc<Service>) -> Result<ksni::Handle<WillTray>, ksni::Error> {
        let tray = WillTray {
            tx: svc.cmd_sender(),
            svc: Arc::downgrade(svc),
            voice_active: false,
        };
        tray.spawn().await
    }

    pub fn spawn(svc: &Arc<Service>) -> Option<ksni::Handle<WillTray>> {
        let rt = svc.rt().clone();
        let svc2 = svc.clone();
        let r = rt.block_on(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                match register(&svc2).await {
                    Ok(h) => Ok(h),
                    Err(e) => {
                        if e.to_string().contains("StatusNotifierWatcher") && try_start_watcher() {
                            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                            register(&svc2).await
                        } else {
                            Err(e)
                        }
                    }
                }
            })
            .await
        });
        match r {
            Ok(Ok(h)) => Some(h),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "bandeja indisponível (StatusNotifierItem)");
                None
            }
            Err(_) => {
                tracing::warn!("bandeja: tempo esgotado ao registrar no D-Bus");
                None
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    use super::*;
    use crate::i18n::tr;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Inner {
        tray: TrayIcon,
        voice_item: MenuItem,
    }

    impl Inner {
        pub fn set_voice_active(&self, active: bool) {
            self.voice_item.set_text(if active {
                tr("Desligar voz")
            } else {
                tr("Ligar voz")
            });
            if let Ok(icon) = Icon::from_rgba(crate::icon::rgba(32, active), 32, 32) {
                let _ = self.tray.set_icon(Some(icon));
            }
        }
    }

    pub fn build(svc: &Arc<Service>) -> Option<Inner> {
        let menu = Menu::new();
        let open = MenuItem::new(tr("Abrir will-ia"), true, None);
        let voice = MenuItem::new(tr("Ligar voz"), true, None);
        // Sem "Sair": o serviço é residente; encerrar só por kill forçado (docs/13).
        if let Err(e) = menu.append_items(&[&open, &voice]) {
            tracing::warn!(error = %e, "menu da bandeja");
            return None;
        }
        let icon = Icon::from_rgba(crate::icon::rgba(32, false), 32, 32).ok()?;
        let tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("will-ia")
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "bandeja indisponível");
                return None;
            }
        };
        let (open_id, voice_id) = (open.id().clone(), voice.id().clone());
        let weak = Arc::downgrade(svc);
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let Some(s) = weak.upgrade() else { return };
            let cmd = if e.id() == &open_id {
                UiCommand::Show
            } else if e.id() == &voice_id {
                UiCommand::ToggleVoice
            } else {
                return;
            };
            s.send_cmd(cmd);
        }));
        let weak = Arc::downgrade(svc);
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                if let Some(s) = weak.upgrade() {
                    s.send_cmd(UiCommand::Show);
                }
            }
        }));
        Some(Inner {
            tray,
            voice_item: voice,
        })
    }
}
