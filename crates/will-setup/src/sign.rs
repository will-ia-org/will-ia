//! macOS: assinatura de código com identidade **estável** (ADR-0023).
//!
//! Binários "ad-hoc" (o padrão do linker) têm requisito designado = `cdhash`, que
//! muda a cada build. TCC (microfone, arquivos, notificações…) e o Keychain
//! amarram a concessão a esse requisito, então toda atualização voltava a pedir
//! permissão. Aqui o instalador cria, uma vez, um certificado autoassinado de
//! *code signing* num keychain próprio (sem senha do usuário, sem diálogo) e
//! assina `will-ia`, `will-ia-app` e o bundle com ele: o requisito passa a ser
//! `identifier "dev.will.will-ia" and certificate leaf = H"…"`, igual em toda
//! versão. O certificado não precisa ser confiável pelo sistema: `codesign`
//! assina mesmo assim e o TCC/Keychain só comparam o requisito.

use crate::os::{run, Log};
use crate::plan::Plan;
use std::path::{Path, PathBuf};

pub const IDENTITY_NAME: &str = "will-ia Local Signing";
pub const APP_ID: &str = "dev.will.will-ia";
pub const SERVER_ID: &str = "dev.will.will-ia.server";
const KEYCHAIN_FILE: &str = "will-ia-signing.keychain-db";
const PASS_FILE: &str = "keychain.pass";

pub struct Signed {
    /// `None` = caiu no ad-hoc com identificador fixo (requisito ainda por cdhash).
    pub identity: Option<String>,
    pub designated: String,
}

fn signing_dir(plan: &Plan) -> PathBuf {
    plan.data_dir.join("signing")
}

fn random_hex(n: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// O keychain já tem a identidade?
fn identity_present(keychain: &Path) -> bool {
    run(
        "security",
        &[
            "find-identity",
            "-p",
            "codesigning",
            &keychain.to_string_lossy(),
        ],
    )
    .map(|out| out.contains(IDENTITY_NAME))
    .unwrap_or(false)
}

/// Garante o keychain `<data_dir>/signing/` com a identidade; devolve (keychain, senha).
fn ensure_identity(plan: &Plan, log: &Log<'_>) -> Result<(PathBuf, String), String> {
    let dir = signing_dir(plan);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let keychain = dir.join(KEYCHAIN_FILE);
    let pass_path = dir.join(PASS_FILE);
    if keychain.is_file() {
        if let Ok(pass) = std::fs::read_to_string(&pass_path) {
            let pass = pass.trim().to_string();
            let _ = run(
                "security",
                &["unlock-keychain", "-p", &pass, &keychain.to_string_lossy()],
            );
            if identity_present(&keychain) {
                return Ok((keychain, pass));
            }
        }
        log("keychain de assinatura inválido; recriando");
        let _ = run(
            "security",
            &["delete-keychain", &keychain.to_string_lossy()],
        );
        let _ = std::fs::remove_file(&keychain);
    }

    let pass = random_hex(24)?;
    write_private(&pass_path, &pass)?;
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let cnf = tmp.join("cert.cnf");
    std::fs::write(
        &cnf,
        format!(
            "[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN={IDENTITY_NAME}\n\
             [v3]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\n\
             extendedKeyUsage=critical,codeSigning\nsubjectKeyIdentifier=hash\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    let key = tmp.join("key.pem");
    let cert = tmp.join("cert.pem");
    let p12 = tmp.join("identity.p12");
    let s = |p: &Path| p.to_string_lossy().to_string();
    run(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "7300",
            "-keyout",
            &s(&key),
            "-out",
            &s(&cert),
            "-config",
            &s(&cnf),
        ],
    )
    .map_err(|e| format!("openssl req: {e}"))?;
    let p12_pass = random_hex(12)?;
    let (key_s, cert_s, p12_s) = (s(&key), s(&cert), s(&p12));
    let pw = format!("pass:{p12_pass}");
    let export = |legacy: bool| {
        let mut args: Vec<&str> = vec![
            "pkcs12", "-export", "-inkey", &key_s, "-in", &cert_s, "-out", &p12_s, "-passout", &pw,
        ];
        if legacy {
            args.push("-legacy");
        }
        run("openssl", &args)
    };
    if let Err(e1) = export(false) {
        export(true).map_err(|e2| format!("openssl pkcs12: {e1} / {e2}"))?;
    }
    let kc = s(&keychain);
    run("security", &["create-keychain", "-p", &pass, &kc])
        .map_err(|e| format!("create-keychain: {e}"))?;
    // sem auto-bloqueio (por tempo ou ao dormir): o instalador roda sem o usuário por perto
    let _ = run("security", &["set-keychain-settings", &kc]);
    run("security", &["unlock-keychain", "-p", &pass, &kc])
        .map_err(|e| format!("unlock-keychain: {e}"))?;
    run(
        "security",
        &[
            "import",
            &s(&p12),
            "-k",
            &kc,
            "-P",
            &p12_pass,
            "-T",
            "/usr/bin/codesign",
            "-A",
        ],
    )
    .map_err(|e| format!("security import: {e}"))?;
    // libera a chave para o codesign sem o diálogo "deseja permitir o acesso?"
    run(
        "security",
        &[
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-s",
            "-k",
            &pass,
            &kc,
        ],
    )
    .map_err(|e| format!("set-key-partition-list: {e}"))?;
    let _ = std::fs::copy(&cert, dir.join("cert.pem"));
    let _ = std::fs::remove_dir_all(&tmp);
    if !identity_present(&keychain) {
        return Err("identidade criada, mas o keychain não a lista para code signing".into());
    }
    log(&format!(
        "identidade de assinatura criada: \"{IDENTITY_NAME}\" em {}",
        keychain.display()
    ));
    Ok((keychain, pass))
}

fn designated(path: &Path) -> String {
    run("codesign", &["-dr", "-", &path.to_string_lossy()])
        .map(|o| {
            o.lines()
                .find(|l| l.contains("designated"))
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

fn codesign(
    identity: &str,
    keychain: Option<&Path>,
    ident: &str,
    target: &Path,
) -> Result<(), String> {
    let kc;
    let mut args = vec!["--force", "--timestamp=none", "-s", identity, "-i", ident];
    if let Some(k) = keychain {
        kc = k.to_string_lossy().to_string();
        args.push("--keychain");
        args.push(&kc);
    }
    let t = target.to_string_lossy().to_string();
    args.push(&t);
    run("codesign", &args)
        .map(|_| ())
        .map_err(|e| format!("codesign {}: {e}", target.display()))
}

/// Assina servidor, app e bundle. Sem `openssl`/`security` funcionais cai no
/// ad-hoc com identificador fixo (o re-prompt continua, mas nada quebra).
pub fn sign_install(plan: &Plan, log: &Log<'_>) -> Result<Signed, String> {
    let before = designated(&plan.app_path);
    let (identity, keychain) = match ensure_identity(plan, log) {
        Ok((kc, _pass)) => (IDENTITY_NAME.to_string(), Some(kc)),
        Err(e) => {
            log(&format!(
                "aviso: sem identidade estável ({e}); assinando ad-hoc com identificador fixo"
            ));
            ("-".to_string(), None)
        }
    };
    let kc = keychain.as_deref();
    // nested primeiro: o bundle auxiliar do servidor (`Contents/Library/LoginItems/
    // will-ia Server.app`, id próprio), depois o app e o bundle externo, que sela os
    // Resources e o nested já assinado.
    let server_bundle = plan
        .server_path
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| plan.server_path.clone());
    codesign(&identity, kc, SERVER_ID, &server_bundle)?;
    codesign(&identity, kc, APP_ID, &plan.app_path)?;
    codesign(&identity, kc, APP_ID, &plan.install_dir)?;
    run(
        "codesign",
        &["--verify", "--strict", &plan.install_dir.to_string_lossy()],
    )
    .map_err(|e| format!("verificação do bundle: {e}"))?;
    let after = designated(&plan.app_path);
    log(&format!("requisito designado antes: {before}"));
    log(&format!("requisito designado depois: {after}"));
    Ok(Signed {
        identity: keychain.map(|_| identity),
        designated: after,
    })
}
