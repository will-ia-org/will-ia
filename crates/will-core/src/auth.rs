//! Identidade de quem chama a API (`docs/26`, etapa 4). Threads são
//! compartilhadas; o `Principal` serve para autoria (turnos, aprovações,
//! custos) e para o portão de administrador nas rotas de configuração.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Member,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }
    pub fn parse(s: &str) -> Role {
        if s.eq_ignore_ascii_case("admin") {
            Role::Admin
        } else {
            Role::Member
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    /// `None` para o token local da API (scripts, app em loopback sem login).
    pub user_id: Option<String>,
    pub username: String,
    pub name: String,
    pub role: Role,
    /// Primeiro login do admin criado na partida: a UI força a troca de senha.
    #[serde(default)]
    pub must_change_password: bool,
}

impl Principal {
    /// Token local da API: administrador implícito desta máquina.
    pub fn local() -> Self {
        Self {
            user_id: None,
            username: "local".into(),
            name: "Token local".into(),
            role: Role::Admin,
            must_change_password: false,
        }
    }
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
    /// Rótulo de autoria gravado em turnos/aprovações.
    pub fn author(&self) -> String {
        self.username.clone()
    }
}
