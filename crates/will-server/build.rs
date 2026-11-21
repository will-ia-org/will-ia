//! Embute o app web (`docs/33`) no servidor, quando disponível.
//!
//! A pasta vem de `WILL_APP_WEB_DIR` (a `dist/app-web` gerada por
//! `tools/app-web/build.sh`; relativa = a partir da raiz do workspace). Sem a
//! variável, ou com a pasta incompleta, o servidor compila sem a página e `/`
//! responde 404 como antes: `cargo build`/`cargo test` não dependem do WASM.

use std::path::PathBuf;

/// Arquivo na pasta do app web → nome embutido em `OUT_DIR`.
const FILES: &[&str] = &[
    "index.html",
    "manifest.webmanifest",
    "icon-180.png",
    "icon-512.png",
    "will-ia-app.js.gz",
    "will-ia-app_bg.wasm.gz",
];

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-env-changed=WILL_APP_WEB_DIR");
    println!("cargo:rustc-check-cfg=cfg(app_web)");
    let dir = std::env::var_os("WILL_APP_WEB_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| {
            let p = PathBuf::from(d);
            if p.is_absolute() {
                p
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(p)
            }
        });
    let mut complete = dir.is_some();
    // FNV-1a de tudo: vira o ETag, para o navegador só baixar de novo o que mudou.
    let mut tag: u64 = 0xcbf2_9ce4_8422_2325;
    for name in FILES {
        let dst = out.join(format!("app-web-{name}"));
        let src = dir.as_ref().map(|d| d.join(name)).filter(|p| p.is_file());
        match src {
            Some(src) => {
                let bytes = std::fs::read(&src).expect("ler app web");
                for b in &bytes {
                    tag = (tag ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
                }
                std::fs::write(&dst, bytes).expect("copiar app web");
                println!("cargo:rerun-if-changed={}", src.display());
            }
            None => {
                std::fs::write(&dst, b"").expect("app web vazio");
                complete = false;
            }
        }
    }
    println!("cargo:rustc-env=WILL_APP_WEB_TAG={tag:016x}");
    if complete {
        println!("cargo:rustc-cfg=app_web");
    } else if dir.is_some() {
        println!("cargo:warning=will-server: WILL_APP_WEB_DIR incompleta; compilado SEM o app web (rode tools/app-web/build.sh)");
    }
}
