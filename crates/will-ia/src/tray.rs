//! Ícone de bandeja do servidor: `ksni` (StatusNotifierItem/D-Bus) no Linux e
//! `tray-icon` nos demais SOs. Só uma ação: abrir o app. Sem "Sair" (o
//! serviço é residente, `docs/13`).

/// Mantém a bandeja viva enquanto existir.
pub struct TrayHandle {
    #[cfg(target_os = "linux")]
    _handle: ksni::Handle<linux::ServerTray>,
    #[cfg(not(target_os = "linux"))]
    _inner: other::Inner,
}

pub fn start(rt: &tokio::runtime::Runtime) -> Option<TrayHandle> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn(rt).map(|h| TrayHandle { _handle: h })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = rt;
        other::build().map(|i| TrayHandle { _inner: i })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use ksni::TrayMethods;

    pub struct ServerTray;

    impl ksni::Tray for ServerTray {
        fn id(&self) -> String {
            "will-ia".into()
        }
        fn title(&self) -> String {
            "will-ia".into()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "will-ia".into(),
                description: "servidor no ar".into(),
                ..Default::default()
            }
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22u32, 32, 48]
                .iter()
                .map(|&s| ksni::Icon {
                    width: s as i32,
                    height: s as i32,
                    data: crate::icon::argb_network(s, false),
                })
                .collect()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = crate::launcher::open_app(&[]);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;
            vec![StandardItem {
                label: "Abrir will-ia".into(),
                activate: Box::new(|_t: &mut Self| {
                    let _ = crate::launcher::open_app(&[]);
                }),
                ..Default::default()
            }
            .into()]
        }
    }

    /// Em desktops sem StatusNotifierWatcher (ex.: Cinnamon sem o applet), tenta
    /// subir o `xapp-sn-watcher`.
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

    pub fn spawn(rt: &tokio::runtime::Runtime) -> Option<ksni::Handle<ServerTray>> {
        let r = rt.block_on(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                match ServerTray.spawn().await {
                    Ok(h) => Ok(h),
                    Err(e) => {
                        if e.to_string().contains("StatusNotifierWatcher") && try_start_watcher() {
                            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                            ServerTray.spawn().await
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
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct Inner {
        _tray: TrayIcon,
    }

    pub fn build() -> Option<Inner> {
        let menu = Menu::new();
        let open = MenuItem::new("Abrir will-ia", true, None);
        if let Err(e) = menu.append_items(&[&open]) {
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
        let open_id = open.id().clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id() == &open_id {
                let _ = crate::launcher::open_app(&[]);
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                let _ = crate::launcher::open_app(&[]);
            }
        }));
        Some(Inner { _tray: tray })
    }
}
