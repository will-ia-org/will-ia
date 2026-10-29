//! Embute os binários `will-ia` e `will-ia-app` no instalador.
//!
//! A pasta vem de `WILL_SETUP_PAYLOAD_DIR` (ex.: `target/release` ou
//! `target/<triple>/release`). Sem a variável (ou sem os arquivos), o instalador
//! compila com payload vazio: útil para `cargo check`/CI; em execução ele avisa
//! que foi compilado sem os binários e não instala.
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let ext = if target_os == "windows" { ".exe" } else { "" };
    println!("cargo:rerun-if-env-changed=WILL_SETUP_PAYLOAD_DIR");
    println!("cargo:rustc-check-cfg=cfg(payload)");
    // Caminho relativo seria resolvido contra crates/will-setup (cwd do build.rs): aceita só absoluto
    // ou, por conveniência, relativo à raiz do workspace (duas pastas acima).
    let dir = std::env::var_os("WILL_SETUP_PAYLOAD_DIR").map(|d| {
        let p = PathBuf::from(d);
        if p.is_absolute() {
            p
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(p)
        }
    });
    let mut complete = true;
    for name in ["will-ia", "will-ia-app"] {
        let dst = out.join(format!("{name}.payload"));
        let src = dir
            .as_ref()
            .map(|d| d.join(format!("{name}{ext}")))
            .filter(|p| p.is_file());
        match src {
            Some(src) => {
                std::fs::copy(&src, &dst).expect("copiar payload");
                println!("cargo:rerun-if-changed={}", src.display());
            }
            None => {
                std::fs::write(&dst, b"").expect("payload vazio");
                complete = false;
            }
        }
    }
    if complete {
        println!("cargo:rustc-cfg=payload");
    } else {
        println!("cargo:warning=will-setup: compilado SEM payload (defina WILL_SETUP_PAYLOAD_DIR com will-ia e will-ia-app)");
    }
}
