//! O que o app pede ao sistema operacional (abrir pasta, notificar, ler e
//! gravar arquivo), com as duas versões: desktop (`will-platform`, `tokio::fs`)
//! e navegador, onde nada disso existe e cada função responde com um erro claro.

use std::path::{Path, PathBuf};

/// Mensagem das funções que dependem de arquivos ou do SO.
#[cfg(target_arch = "wasm32")]
const NO_OS: &str = "indisponível no navegador";

/// O app está rodando dentro do navegador (app web).
pub const WEB: bool = cfg!(target_arch = "wasm32");

#[cfg(not(target_arch = "wasm32"))]
pub fn open_path(path: &Path) -> Result<(), String> {
    will_platform::open_path(path)
}
#[cfg(target_arch = "wasm32")]
pub fn open_path(_path: &Path) -> Result<(), String> {
    Err(NO_OS.into())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn notify(title: &str, body: &str) {
    will_platform::notify(title, body);
}
#[cfg(target_arch = "wasm32")]
pub fn notify(_title: &str, _body: &str) {}

#[cfg(not(target_arch = "wasm32"))]
pub async fn read(path: &Path) -> Result<Vec<u8>, String> {
    tokio::fs::read(path).await.map_err(|e| e.to_string())
}
/// No navegador só dá para ler o que o usuário escolheu ou soltou na página
/// (ver [`web_register`]).
#[cfg(target_arch = "wasm32")]
pub async fn read(path: &Path) -> Result<Vec<u8>, String> {
    web_files()
        .lock()
        .get(path)
        .cloned()
        .ok_or_else(|| NO_OS.to_string())
}

/// Arquivos que o usuário entregou à página (diálogo ou arrastar-e-soltar),
/// por um caminho inventado: o resto do app continua lidando com caminhos.
#[cfg(target_arch = "wasm32")]
fn web_files() -> &'static parking_lot::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
    static FILES: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>,
    > = std::sync::OnceLock::new();
    FILES.get_or_init(Default::default)
}

/// Guarda os bytes de um arquivo e devolve o caminho inventado dele.
#[cfg(target_arch = "wasm32")]
pub fn web_register(name: &str, bytes: Vec<u8>) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = if name.trim().is_empty() {
        "arquivo"
    } else {
        name
    };
    let path = PathBuf::from(format!("anexo-web-{n}")).join(name);
    web_files().lock().insert(path.clone(), bytes);
    path
}

/// Escolhidos pelo diálogo do navegador, à espera de [`take_picked`].
#[cfg(target_arch = "wasm32")]
static PICKED: parking_lot::Mutex<Vec<PathBuf>> = parking_lot::Mutex::new(Vec::new());

/// Arquivos escolhidos desde a última chamada. No desktop o diálogo é
/// síncrono ([`pick_files`] já devolve a lista), então aqui vem sempre vazio.
pub fn take_picked() -> Vec<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        std::mem::take(&mut *PICKED.lock())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Vec::new()
    }
}

/// Caminhos dos arquivos soltos na janela. No navegador o conteúdo é lido de
/// forma assíncrona: devolve vazio e os arquivos chegam por [`take_picked`].
pub fn dropped_paths(ctx: &egui::Context, files: Vec<egui::DroppedFileHandle>) -> Vec<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        for f in files {
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = f
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("arquivo")
                    .to_string();
                if let Ok(bytes) = f.bytes_async().await {
                    PICKED.lock().push(web_register(&name, bytes));
                    ctx.request_repaint();
                }
            });
        }
        Vec::new()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ctx;
        files
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| p.is_file())
            .collect()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn create_dir_all(path: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| e.to_string())
}
#[cfg(target_arch = "wasm32")]
pub async fn create_dir_all(_path: &Path) -> Result<(), String> {
    Err(NO_OS.into())
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn write(path: &Path, bytes: Vec<u8>) -> Result<(), String> {
    tokio::fs::write(path, bytes)
        .await
        .map_err(|e| e.to_string())
}
#[cfg(target_arch = "wasm32")]
pub async fn write(_path: &Path, _bytes: Vec<u8>) -> Result<(), String> {
    Err(NO_OS.into())
}

/// Roda `f` numa thread própria. No navegador não há threads: `f` não roda
/// (quem chama são recursos que a versão web não oferece).
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(f);
}
#[cfg(target_arch = "wasm32")]
pub fn spawn_thread(_f: impl FnOnce() + Send + 'static) {}

/// Endereço de onde a página foi servida (`https://host[:porta]`): no app web
/// é o próprio servidor.
#[cfg(target_arch = "wasm32")]
pub fn web_origin() -> Option<String> {
    web_sys::window()?.location().origin().ok()
}

#[cfg(target_arch = "wasm32")]
pub fn storage_get(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()??
        .get_item(key)
        .ok()?
}

#[cfg(target_arch = "wasm32")]
pub fn storage_set(key: &str, value: &str) -> Result<(), String> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .ok_or("o navegador bloqueou o armazenamento local")?
        .set_item(key, value)
        .map_err(|_| "o navegador recusou gravar no armazenamento local".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn which(bin: &str) -> Option<PathBuf> {
    will_platform::which(bin)
}
#[cfg(target_arch = "wasm32")]
pub fn which(_bin: &str) -> Option<PathBuf> {
    None
}

// ----- diálogos de arquivo do SO (no navegador ainda não há: `None`) -----

#[cfg(not(target_arch = "wasm32"))]
pub fn pick_files(_ctx: &egui::Context, title: &str) -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new().set_title(title).pick_files()
}
/// No navegador o diálogo é assíncrono: devolve `None` na hora e os arquivos
/// chegam depois por [`take_picked`].
#[cfg(target_arch = "wasm32")]
pub fn pick_files(ctx: &egui::Context, title: &str) -> Option<Vec<PathBuf>> {
    let ctx = ctx.clone();
    let dialog = rfd::AsyncFileDialog::new().set_title(title);
    wasm_bindgen_futures::spawn_local(async move {
        let Some(files) = dialog.pick_files().await else {
            return;
        };
        for f in files {
            let bytes = f.read().await;
            let path = web_register(&f.file_name(), bytes);
            PICKED.lock().push(path);
        }
        ctx.request_repaint();
    });
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn pick_zip(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter("zip", &["zip"])
        .pick_file()
}
#[cfg(target_arch = "wasm32")]
pub fn pick_zip(_title: &str) -> Option<PathBuf> {
    None
}

/// "Salvar como" de um `.zip`, começando em Downloads (ou na pasta pessoal).
#[cfg(not(target_arch = "wasm32"))]
pub fn save_zip(title: &str, name: &str) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(name)
        .add_filter("zip", &["zip"]);
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        let dl = home.join("Downloads");
        dlg = dlg.set_directory(if dl.is_dir() { dl } else { home });
    }
    dlg.save_file()
}
#[cfg(target_arch = "wasm32")]
pub fn save_zip(_title: &str, _name: &str) -> Option<PathBuf> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn pick_folder(title: &str, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new().set_title(title);
    if let Some(p) = start {
        dlg = dlg.set_directory(p);
    }
    dlg.pick_folder()
}
#[cfg(target_arch = "wasm32")]
pub fn pick_folder(_title: &str, _start: Option<PathBuf>) -> Option<PathBuf> {
    None
}

/// Pasta temporária do SO (para abrir um anexo baixado no programa padrão).
#[cfg(not(target_arch = "wasm32"))]
pub fn temp_dir() -> Result<PathBuf, String> {
    Ok(std::env::temp_dir())
}
#[cfg(target_arch = "wasm32")]
pub fn temp_dir() -> Result<PathBuf, String> {
    Err("abrir anexos ainda não funciona no navegador".into())
}

/// Tamanho de um arquivo a anexar (no navegador, dos bytes guardados em memória).
pub fn file_size(path: &Path) -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    {
        web_files().lock().get(path).map(|b| b.len() as u64)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::metadata(path).ok().map(|m| m.len())
    }
}
