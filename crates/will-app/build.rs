//! Windows: embute o ícone (`assets/icon/will-ia.ico`) e os metadados de versão
//! no `.exe` (Explorer, barra de tarefas, atalhos). Nos outros SOs não faz nada.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon/will-ia.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/will-ia.ico");
        res.set("ProductName", "will-ia");
        res.set("FileDescription", env!("CARGO_PKG_DESCRIPTION"));
        res.set("LegalCopyright", "MIT");
        if let Err(e) = res.compile() {
            println!("cargo:warning=recurso do Windows (ícone) não compilado: {e}");
        }
    }
}
