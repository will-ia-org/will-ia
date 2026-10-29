//! macOS: política de ativação (Dock × bandeja), run loop do AppKit e ativação
//! do app pelo LaunchServices (`open`), que é o caminho que o sistema aceita sem
//! fazer o ícone do Dock "pular" (ADR-0022).
//!
//! - O **servidor** (`will-ia`) vive só na barra de menus: `become_accessory()`
//!   antes de criar o `NSStatusItem` e `run_main_loop()` na thread principal
//!   (o `tray-icon` exige o run loop do AppKit para desenhar e receber cliques).
//! - Trazer o **app** para frente: `activate_bundle`/`activate_self` usam
//!   `/usr/bin/open`, isto é, o LaunchServices ativa (ou lança) o bundle. Desde o
//!   macOS 14 a ativação é cooperativa: `activateIgnoringOtherApps:` vindo de um
//!   app que não está na frente é negado e vira um pedido de atenção (ícone
//!   pulando no Dock); o `open` não tem esse problema.

use objc2::msg_send;
use objc2::runtime::{AnyClass, Bool};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_foundation::NSString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// `AVCaptureDevice` (status/pedido do microfone) vem do AVFoundation; a classe é
// resolvida em tempo de execução, mas o framework precisa estar ligado.
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// Estado de uma permissão do TCC (espelha `AVAuthorizationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    /// Nunca pedida: o próximo uso (ou `request_microphone`) mostra o diálogo.
    NotDetermined,
    /// Bloqueada por política (MDM/controle parental); não há diálogo.
    Restricted,
    /// O usuário negou: só em Ajustes → Privacidade e Segurança.
    Denied,
    Authorized,
    Unknown,
}

impl Authorization {
    pub fn label(self) -> &'static str {
        match self {
            Authorization::NotDetermined => "ainda não pedida",
            Authorization::Restricted => "bloqueada por política",
            Authorization::Denied => "negada",
            Authorization::Authorized => "concedida",
            Authorization::Unknown => "desconhecida",
        }
    }
}

const AV_MEDIA_TYPE_AUDIO: &str = "soun";

/// Permissão de microfone deste processo (`AVCaptureDevice.authorizationStatus`).
pub fn microphone_status() -> Authorization {
    let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
        return Authorization::Unknown;
    };
    let media = NSString::from_str(AV_MEDIA_TYPE_AUDIO);
    // SAFETY: seletor de classe documentado do AVFoundation; argumento é um NSString vivo.
    let st: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
    match st {
        0 => Authorization::NotDetermined,
        1 => Authorization::Restricted,
        2 => Authorization::Denied,
        3 => Authorization::Authorized,
        _ => Authorization::Unknown,
    }
}

/// Pede a permissão de microfone agora (diálogo do sistema se `NotDetermined`;
/// senão o callback recebe o estado atual). O callback roda numa thread do sistema.
pub fn request_microphone(on_done: impl Fn(bool) + Send + Sync + 'static) {
    let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
        on_done(false);
        return;
    };
    let media = NSString::from_str(AV_MEDIA_TYPE_AUDIO);
    let block = block2::RcBlock::new(move |granted: Bool| on_done(granted.as_bool()));
    // SAFETY: seletor de classe documentado; o bloco é retido pelo AVFoundation até a resposta.
    unsafe {
        let _: () = msg_send![cls, requestAccessForMediaType: &*media, completionHandler: &*block];
    }
}

/// Painéis de Ajustes do Sistema ligados às permissões que o will-ia usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPane {
    Microphone,
    Notifications,
    FilesAndFolders,
    LoginItems,
}

impl SettingsPane {
    pub fn url(self) -> &'static str {
        match self {
            SettingsPane::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            SettingsPane::Notifications => {
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
            }
            SettingsPane::FilesAndFolders => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"
            }
            SettingsPane::LoginItems => {
                "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
            }
        }
    }
}

/// Abre o painel de Ajustes correspondente.
pub fn open_settings(pane: SettingsPane) -> Result<(), String> {
    crate::open_url(pane.url())
}

/// Esconde o processo do Dock e do Cmd+Tab (ícone só na barra de menus).
/// Precisa rodar na thread principal; devolve `false` fora dela ou se o AppKit recusar.
pub fn become_accessory() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("become_accessory fora da thread principal");
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let ok = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    if !ok {
        tracing::warn!("AppKit recusou a política de ativação Accessory");
    }
    ok
}

/// Entrega a thread principal ao run loop do AppKit (`[NSApp run]`). Não volta:
/// o `tray-icon` precisa dele para desenhar o ícone e despachar cliques/menus.
/// Quem quiser encerrar o processo chama `std::process::exit` de outra thread.
pub fn run_main_loop() -> ! {
    match MainThreadMarker::new() {
        Some(mtm) => {
            NSApplication::sharedApplication(mtm).run();
            // `run` só retorna depois de `stop:`; não é o nosso caso.
            std::process::exit(0)
        }
        None => {
            tracing::error!("run_main_loop fora da thread principal; aguardando para sempre");
            loop {
                std::thread::park();
            }
        }
    }
}

/// O processo atual é o app ativo (na frente)?
pub fn is_active() -> bool {
    NSRunningApplication::currentApplication().isActive()
}

/// Bundle `.app` que contém `exe` (ex.: `…/will-ia.app/Contents/MacOS/will-ia`).
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| {
            p.extension().is_some_and(|e| e == "app") && p.join("Contents/Info.plist").is_file()
        })
        .map(Path::to_path_buf)
}

/// Bundle do executável atual, se ele estiver dentro de um `.app`.
pub fn current_bundle() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|e| bundle_of(&e))
}

/// `open -a <bundle> [--args …]`: o LaunchServices ativa o app se já roda (sem
/// novo processo) ou o lança. Não espera o app abrir.
pub fn activate_bundle(bundle: &Path, args: &[String]) -> std::io::Result<()> {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-a").arg(bundle);
    if !args.is_empty() {
        cmd.arg("--args").args(args);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Traz o próprio app para frente pelo LaunchServices. `false` se não estamos
/// num bundle (execução de desenvolvimento) ou se o `open` falhou — o chamador
/// então decide se usa o foco do winit.
pub fn activate_self() -> bool {
    let Some(bundle) = current_bundle() else {
        return false;
    };
    match activate_bundle(&bundle, &[]) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "open -a falhou");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_of_finds_enclosing_app() {
        let dir = std::env::temp_dir().join(format!("will-bundle-{}", std::process::id()));
        let exe = dir.join("x.app/Contents/MacOS/bin");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(dir.join("x.app/Contents/Info.plist"), "<plist/>").unwrap();
        assert_eq!(bundle_of(&exe), Some(dir.join("x.app")));
        assert_eq!(bundle_of(&dir.join("solto/bin")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
