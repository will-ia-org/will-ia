//! Contas, senhas e sessões (`docs/26`, etapa 4): argon2 para senhas, token
//! de sessão aleatório guardado por hash (sha256), rate limit de login e
//! bootstrap do primeiro administrador.

use crate::app::AppState;
use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use will_core::{Principal, Role};
use will_storage::users::UserRecord;

/// Validade de uma sessão (renovada pelo uso? não: fixa, para revogação previsível).
pub const SESSION_TTL_DAYS: i64 = 30;
/// Arquivo com a senha inicial do admin (apagado na primeira troca).
pub const INITIAL_PASSWORD_FILE: &str = "admin-initial-password.txt";

pub fn hash_password(password: &str) -> Result<String, String> {
    // Argon2id v19 com sal aleatório (feature `getrandom`); saída em PHC string.
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn random_token() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_password() -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    (0..16)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

pub fn token_hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn principal_of(u: &UserRecord) -> Principal {
    Principal {
        user_id: Some(u.id.clone()),
        username: u.username.clone(),
        name: u.name.clone(),
        role: Role::parse(&u.role),
        must_change_password: u.must_change_password,
    }
}

/// Senha fraca demais?
pub fn check_password_strength(p: &str) -> Result<(), String> {
    if p.chars().count() < 8 {
        return Err("a senha precisa ter pelo menos 8 caracteres".into());
    }
    Ok(())
}

/// Primeira subida sem usuários: cria `admin` com senha aleatória, grava o
/// arquivo com a senha (0600) e avisa no log. `will-ia info` mostra o arquivo.
pub async fn bootstrap_admin(state: &Arc<AppState>) {
    let n = match state.storage.count_users().await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "não consegui contar usuários");
            return;
        }
    };
    if n > 0 {
        return;
    }
    let password = random_password();
    let hash = match hash_password(&password) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "falha ao gerar hash da senha inicial");
            return;
        }
    };
    let id = will_core::ThreadId::new().to_string();
    if let Err(e) = state
        .storage
        .create_user(&id, "admin", "Administrador", None, &hash, "admin", true)
        .await
    {
        tracing::error!(error = %e, "falha ao criar o admin inicial");
        return;
    }
    let path = state.paths.data_dir.join(INITIAL_PASSWORD_FILE);
    let _ = std::fs::write(&path, format!("admin\n{password}\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::warn!(
        "primeiro administrador criado: usuário `admin`, senha inicial em {} (também em `will-ia info`); troque no primeiro login",
        path.display()
    );
    state.bus.log(
        "warn",
        "auth",
        format!(
            "Administrador inicial criado (usuário admin). Senha em {}",
            path.display()
        ),
    );
}

/// Rate limit de login por IP: no máximo 10 tentativas por 15 minutos.
pub struct LoginLimiter {
    map: dashmap::DashMap<IpAddr, (u32, Instant)>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self {
            map: dashmap::DashMap::new(),
        }
    }
}

impl LoginLimiter {
    const MAX: u32 = 10;
    const WINDOW: Duration = Duration::from_secs(15 * 60);

    /// `false` = bloqueado.
    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut e = self.map.entry(ip).or_insert((0, now));
        if now.duration_since(e.1) > Self::WINDOW {
            *e = (0, now);
        }
        if e.0 >= Self::MAX {
            return false;
        }
        e.0 += 1;
        true
    }

    pub fn reset(&self, ip: IpAddr) {
        self.map.remove(&ip);
    }
}
