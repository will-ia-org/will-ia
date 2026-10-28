//! Usuários (só administradores): contas do servidor (`/api/users`, `docs/26`
//! etapa 4). Lista em cartões com ações (editar, desativar/reativar) e um
//! formulário de criação/edição. As threads são compartilhadas: a conta serve
//! para dizer quem fez o quê e para revogar acesso, não para separar conversas.

use super::Cx;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::connect::role_label;
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use egui::RichText;
use serde_json::{json, Value};
use std::time::Duration;
use will_core::Role;

/// Tamanho mínimo da senha inicial / redefinida (o servidor pode exigir mais).
const MIN_PASSWORD_LEN: usize = 8;

#[derive(Default)]
pub struct UsersPage {
    list: Loader<Value>,
    form: Option<Form>,
    /// Ações em voo: (texto do toast de sucesso, tarefa).
    actions: Vec<(String, Task<Value>)>,
    /// (id, username) do usuário aguardando confirmação de desativação.
    confirm_deactivate: Option<(String, String)>,
}

/// Formulário de criação (`editing == None`) ou edição (`Some(id)`).
#[derive(Default)]
struct Form {
    editing: Option<String>,
    username: String,
    name: String,
    email: String,
    /// Senha inicial (criação) ou nova senha (edição; vazio = não mexe).
    password: String,
    /// `"admin"` ou `"member"`.
    role: String,
    active: bool,
    must_change_password: bool,
    save: Option<Task<Value>>,
}

impl Form {
    fn new() -> Self {
        Self {
            role: Role::Member.as_str().into(),
            active: true,
            must_change_password: true,
            ..Default::default()
        }
    }

    fn from_view(v: &Value) -> Self {
        Self {
            editing: Some(ui::s(v, "id")),
            username: ui::s(v, "username"),
            name: ui::s(v, "name"),
            email: ui::s(v, "email"),
            role: Role::parse(&ui::s(v, "role")).as_str().into(),
            active: ui::b(v, "active"),
            must_change_password: ui::b(v, "must_change_password"),
            ..Default::default()
        }
    }

    fn saving(&self) -> bool {
        self.save.as_ref().is_some_and(|t| t.pending())
    }

    /// Valida e monta o corpo: `POST /api/users` na criação, `PATCH` na edição.
    fn input(&self) -> Result<Value, String> {
        let password = self.password.trim();
        if !password.is_empty() && password.chars().count() < MIN_PASSWORD_LEN {
            return Err(trf(
                "A senha precisa ter pelo menos {} caracteres.",
                &[&MIN_PASSWORD_LEN],
            ));
        }
        let name = if self.name.trim().is_empty() {
            self.username.trim()
        } else {
            self.name.trim()
        };
        if self.editing.is_some() {
            let mut body = json!({
                "name": name,
                "email": self.email.trim(),
                "role": self.role,
                "active": self.active,
            });
            if !password.is_empty() {
                body["password"] = Value::String(password.to_string());
            }
            return Ok(body);
        }
        let username = self.username.trim();
        if username.is_empty() {
            return Err(tr("Informe o nome de usuário.").into());
        }
        if username
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        {
            return Err(
                tr("Nome de usuário: só letras, números, ponto, hífen e sublinhado.").into(),
            );
        }
        if password.is_empty() {
            return Err(tr("Informe a senha inicial.").into());
        }
        Ok(json!({
            "username": username,
            "name": name,
            "email": self.email.trim(),
            "password": password,
            "role": self.role,
            "must_change_password": self.must_change_password,
        }))
    }
}

impl UsersPage {
    fn reload(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|c| async move { c.users().await }));
    }

    fn push_action(&mut self, done_text: String, task: Task<Value>) {
        self.actions.push((done_text, task));
    }

    fn deactivate(&mut self, cx: &Cx, id: String, username: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.deactivate_user(&id).await });
        self.push_action(trf("\"{}\" desativado.", &[&username]), task);
    }

    fn reactivate(&mut self, cx: &Cx, id: String, username: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.patch_user(&id, json!({ "active": true })).await });
        self.push_action(trf("\"{}\" reativado.", &[&username]), task);
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.list.stale(Duration::from_secs(60)) {
            self.reload(cx);
        }
        self.list.poll();

        let mut results = Vec::new();
        self.actions.retain(|(text, t)| match t.take() {
            Some(r) => {
                results.push((text.clone(), r));
                false
            }
            None => true,
        });
        for (text, r) in results {
            match r {
                Ok(_) => cx.toasts.success(text),
                Err(e) => cx.toasts.error(e),
            }
            self.list.loaded_at = None;
        }

        if let Some(f) = &mut self.form {
            if let Some(t) = &f.save {
                if let Some(r) = t.take() {
                    f.save = None;
                    match r {
                        Ok(_) => {
                            cx.toasts.success(if f.editing.is_some() {
                                tr("Usuário salvo.")
                            } else {
                                tr("Usuário criado.")
                            });
                            self.form = None;
                            self.list.loaded_at = None;
                        }
                        Err(e) => cx.toasts.error(trf("Não salvou: {}", &[&e])),
                    }
                }
            }
        }
    }

    /// Lista devolvida pela API (aceita tanto `[...]` quanto `{ "users": [...] }`).
    fn users(&self) -> Vec<Value> {
        self.list
            .data
            .as_ref()
            .and_then(|v| {
                v.as_array()
                    .cloned()
                    .or_else(|| v.get("users").and_then(|u| u.as_array()).cloned())
            })
            .unwrap_or_default()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);

        ui::heading_help(
            ui,
            tr("Usuários"),
            Some(tr(
                "Contas deste servidor. Todos veem as mesmas threads; a conta diz quem fez o quê e pode ser revogada.",
            )),
            &help::USERS,
        );

        // ---- barra de ações ----
        ui.horizontal(|ui| {
            if ui::primary_button(ui, tr("Novo usuário")).clicked() {
                self.form = Some(Form::new());
            }
            if ui
                .add_enabled(!self.list.loading(), egui::Button::new(tr("Atualizar")))
                .clicked()
            {
                self.list.loaded_at = None;
            }
            if self.list.loading() {
                ui.spinner();
            }
        });

        if let Some(e) = &self.list.error {
            ui::error_text(ui, e);
        }

        let me = cx
            .snap
            .principal
            .as_ref()
            .map(|p| p.username.clone())
            .unwrap_or_default();
        let users = self.users();
        let mut edit: Option<Value> = None;
        let mut deactivate: Option<(String, String)> = None;
        let mut reactivate: Option<(String, String)> = None;

        ui::section(ui, tr("Contas"), |ui| {
            if users.is_empty() {
                ui.label(
                    RichText::new(if self.list.loading() {
                        tr("carregando…")
                    } else {
                        tr("Nenhum usuário cadastrado.")
                    })
                    .weak(),
                );
                return;
            }
            let dark = ui.visuals().dark_mode;
            let pal = ui::palette(dark);
            for u in &users {
                let id = ui::s(u, "id");
                let username = ui::s(u, "username");
                let name = ui::s(u, "name");
                let email = ui::s(u, "email");
                let role = Role::parse(&ui::s(u, "role"));
                let active = ui::b(u, "active");
                let is_me = username == me;
                egui::Frame::new()
                    .fill(pal.card)
                    .stroke(egui::Stroke::new(1.0, pal.border))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&username).strong());
                            if !name.is_empty() && name != username {
                                ui.label(RichText::new(&name).weak());
                            }
                            ui::badge(
                                ui,
                                role_label(role),
                                if role == Role::Admin {
                                    Tone::Info
                                } else {
                                    Tone::Neutral
                                },
                            );
                            if !active {
                                ui::badge(ui, tr("inativo"), Tone::Danger);
                            }
                            if ui::b(u, "must_change_password") {
                                ui::badge(ui, tr("troca de senha pendente"), Tone::Warning);
                            }
                            if is_me {
                                ui::badge(ui, tr("você"), Tone::Success);
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if !email.is_empty() {
                                ui.label(RichText::new(&email).small().weak());
                                ui.label(RichText::new("·").small().weak());
                            }
                            let last = ui::fmt_dt(&u["last_login_at"]);
                            ui.label(
                                RichText::new(trf(
                                    "último login: {}",
                                    &[&if last.is_empty() {
                                        tr("nunca").to_string()
                                    } else {
                                        last
                                    }],
                                ))
                                .small()
                                .weak(),
                            );
                            let created = ui::fmt_dt(&u["created_at"]);
                            if !created.is_empty() {
                                ui.label(RichText::new("·").small().weak());
                                ui.label(
                                    RichText::new(trf("criado em {}", &[&created]))
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.small_button(tr("Editar")).clicked() {
                                edit = Some(u.clone());
                            }
                            if active {
                                let r = ui.add_enabled(
                                    !is_me,
                                    egui::Button::new(tr("Desativar")).small(),
                                );
                                let r = if is_me {
                                    r.on_disabled_hover_text(tr(
                                        "Você não pode desativar a própria conta.",
                                    ))
                                } else {
                                    r.on_hover_text(tr(
                                        "A conta deixa de entrar e as sessões abertas são encerradas; o histórico fica.",
                                    ))
                                };
                                if r.clicked() {
                                    deactivate = Some((id.clone(), username.clone()));
                                }
                            } else if ui.small_button(tr("Reativar")).clicked() {
                                reactivate = Some((id.clone(), username.clone()));
                            }
                        });
                    });
                ui.add_space(6.0);
            }
        });

        if let Some(u) = edit {
            self.form = Some(Form::from_view(&u));
        }
        if let Some(d) = deactivate {
            self.confirm_deactivate = Some(d);
        }
        if let Some((id, username)) = reactivate {
            self.reactivate(cx, id, &username);
        }

        // ---- formulário ----
        let mut submit = false;
        let mut cancel = false;
        if let Some(f) = &mut self.form {
            let editing = f.editing.is_some();
            let saving = f.saving();
            ui::section(
                ui,
                if editing {
                    tr("Editar usuário")
                } else {
                    tr("Novo usuário")
                },
                |ui| {
                    ui::field(
                        ui,
                        tr("Nome de usuário"),
                        Some(tr(
                            "Curto, sem espaços; é o que aparece como autor nas threads.",
                        )),
                        |ui| {
                            if editing {
                                ui::mono(ui, &f.username);
                            } else {
                                ui::text_input(ui, &mut f.username, "maria");
                            }
                        },
                    );
                    ui::field(ui, tr("Nome"), None, |ui| {
                        ui::text_input(ui, &mut f.name, tr("ex.: Maria Silva"));
                    });
                    ui::field(ui, tr("E-mail"), Some(tr("Opcional.")), |ui| {
                        ui::text_input(ui, &mut f.email, "maria@exemplo.com");
                    });
                    ui::field(
                        ui,
                        tr("Papel"),
                        Some(tr(
                            "Administrador mexe em configurações, segredos e usuários; membro conversa, aprova, agenda e usa a voz.",
                        )),
                        |ui| {
                            ui::select(
                                ui,
                                "user-role",
                                &mut f.role,
                                &[
                                    (Role::Member.as_str(), role_label(Role::Member)),
                                    (Role::Admin.as_str(), role_label(Role::Admin)),
                                ],
                            );
                        },
                    );
                    if editing {
                        ui::field(ui, tr("Ativo"), None, |ui| {
                            ui::toggle(ui, &mut f.active);
                        });
                        ui::field(
                            ui,
                            tr("Redefinir senha"),
                            Some(tr("Deixe em branco para manter a senha atual.")),
                            |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut f.password)
                                        .password(true)
                                        .desired_width(f32::INFINITY),
                                );
                            },
                        );
                    } else {
                        ui::field(
                            ui,
                            tr("Senha inicial"),
                            Some(tr("Entregue à pessoa por um canal seguro.")),
                            |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut f.password)
                                        .password(true)
                                        .desired_width(f32::INFINITY),
                                );
                            },
                        );
                        ui::field(ui, tr("Exigir troca no primeiro acesso"), None, |ui| {
                            ui::toggle(ui, &mut f.must_change_password);
                        });
                    }
                    ui.horizontal(|ui| {
                        if saving {
                            ui::busy(ui, tr("salvando…"));
                        } else if ui::primary_button(
                            ui,
                            if editing {
                                tr("Salvar")
                            } else {
                                tr("Criar usuário")
                            },
                        )
                        .clicked()
                        {
                            submit = true;
                        }
                        if ui.button(tr("Cancelar")).clicked() {
                            cancel = true;
                        }
                    });
                },
            );
        }
        if cancel {
            self.form = None;
        }
        if submit {
            if let Some(f) = &mut self.form {
                match f.input() {
                    Ok(body) => {
                        let editing = f.editing.clone();
                        f.save = Some(cx.svc.call(move |c| async move {
                            match editing {
                                Some(id) => c.patch_user(&id, body).await,
                                None => c.create_user(body).await,
                            }
                        }));
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }

        // ---- confirmação de desativação ----
        if let Some((id, username)) = self.confirm_deactivate.clone() {
            let c = ui::Confirm {
                title: tr("Desativar usuário").into(),
                body: trf(
                    "Desativar \"{}\"? A pessoa não consegue mais entrar; as threads e o histórico ficam. Dá para reativar depois.",
                    &[&username],
                ),
                confirm_label: tr("Desativar").into(),
                danger: true,
                tag: "deactivate-user".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_deactivate = None;
                if ok {
                    self.deactivate(cx, id, &username);
                }
            }
        }
    }
}
