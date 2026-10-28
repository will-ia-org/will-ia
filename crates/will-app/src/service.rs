//! Ponte entre a GUI (thread principal, síncrona) e o servidor (via rede).
//!
//! O app é um **cliente** do `will-ia` (HTTP/2 + WebSocket, `docs/26`). A GUI
//! dispara chamadas assíncronas com [`Service::call`], que devolvem um
//! [`Task`] consultado a cada quadro; quando a tarefa termina ela pede um
//! `request_repaint`, então a tela nunca faz polling. Eventos chegam pelo WS
//! `/api/events` (com reconexão) como [`ClientEvent`].

use parking_lot::{Mutex, RwLock};
use std::future::Future;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use will_client::{Client, ClientEvent};

use crate::profiles::Profile;

/// Cliente da API, compartilhado pelas páginas (`Service::call`).
pub type Api = Arc<Client>;

/// Cliente da API para um perfil: URL, token de sessão e política de
/// certificado (`ca_pem`/`insecure`, só para `https://`).
pub fn client_for(profile: &Profile) -> Client {
    Client::with_tls(
        profile.url.clone(),
        profile.token.clone(),
        &profile.tls_options(),
    )
}

/// Comandos vindos de fora da GUI (ipc do app, atalho global, tela de entrada).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Show,
    /// Atalho global pressionado/solto (toggle ou push-to-talk conforme configuração).
    HotkeyDown,
    HotkeyUp,
    /// Captura a janela principal em PNG (diagnóstico/desenvolvimento), opcionalmente
    /// navegando antes para a página indicada.
    Screenshot(std::path::PathBuf, Option<String>),
    ScreenshotNow(std::path::PathBuf),
    /// Trocar de servidor (perfil escolhido na tela de entrada).
    Connect(Profile),
}

pub struct Service {
    rt: tokio::runtime::Runtime,
    api: RwLock<Api>,
    profile: RwLock<Profile>,
    ctx: Mutex<Option<egui::Context>>,
    cmd_tx: Sender<UiCommand>,
    ev_tx: Sender<ClientEvent>,
    events_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Service {
    /// Cria o runtime e conecta no perfil (o WS de eventos tenta até conseguir).
    pub fn new(
        profile: Profile,
    ) -> anyhow::Result<(Arc<Self>, Receiver<ClientEvent>, Receiver<UiCommand>)> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("will-app-rt")
            .build()?;
        let api = Arc::new(client_for(&profile));
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel::<ClientEvent>();
        let svc = Arc::new(Self {
            rt,
            api: RwLock::new(api),
            profile: RwLock::new(profile),
            ctx: Mutex::new(None),
            cmd_tx,
            ev_tx,
            events_task: Mutex::new(None),
        });
        // Sem perfil (tela de entrada) não há a quem conectar.
        if svc.has_profile() {
            svc.spawn_events();
        }
        Ok((svc, ev_rx, cmd_rx))
    }

    fn spawn_events(self: &Arc<Self>) {
        if let Some(t) = self.events_task.lock().take() {
            t.abort();
        }
        let weak = Arc::downgrade(self);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.repaint();
            }
        });
        let api = self.api();
        let task = api.spawn_events(self.rt.handle(), self.ev_tx.clone(), wake);
        *self.events_task.lock() = Some(task);
    }

    /// Troca de servidor: novo cliente, novo WS de eventos. As páginas devem
    /// ser recriadas pelo app (snapshot e caches são do servidor anterior).
    pub fn connect(self: &Arc<Self>, profile: Profile) {
        *self.api.write() = Arc::new(client_for(&profile));
        *self.profile.write() = profile;
        self.spawn_events();
    }

    /// Desiste do servidor atual (sair do perfil ou cancelar a reconexão): para
    /// o WS de eventos e troca o cliente por um que não aponta para lugar
    /// nenhum. O app volta para a tela de entrada.
    pub fn disconnect(&self) {
        if let Some(t) = self.events_task.lock().take() {
            t.abort();
        }
        *self.api.write() = Arc::new(Client::new("http://127.0.0.1:0", ""));
        *self.profile.write() = Profile::none();
    }

    /// Há um perfil escolhido (mesmo que o servidor esteja fora do ar).
    pub fn has_profile(&self) -> bool {
        !self.profile.read().is_none()
    }

    pub fn api(&self) -> Api {
        self.api.read().clone()
    }

    pub fn profile(&self) -> Profile {
        self.profile.read().clone()
    }

    pub fn set_ctx(&self, ctx: egui::Context) {
        *self.ctx.lock() = Some(ctx);
    }

    pub fn repaint(&self) {
        if let Some(c) = self.ctx.lock().as_ref() {
            c.request_repaint();
        }
    }

    pub fn send_cmd(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
        self.repaint();
    }

    pub fn rt(&self) -> &tokio::runtime::Handle {
        self.rt.handle()
    }

    #[allow(dead_code)]
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }

    /// Bloqueia a thread da GUI numa chamada ao servidor (só no arranque,
    /// para o snapshot inicial), com limite de tempo.
    pub fn block_on<T>(&self, fut: impl Future<Output = T>) -> Option<T> {
        self.rt
            .block_on(async move { tokio::time::timeout(Duration::from_secs(8), fut).await })
            .ok()
    }

    /// Dispara uma chamada assíncrona ao servidor. O closure recebe o cliente da API.
    pub fn call<T, Fut>(self: &Arc<Self>, f: impl FnOnce(Api) -> Fut + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        let task = Task::new();
        let inner = task.inner.clone();
        let state = self.api();
        let weak = Arc::downgrade(self);
        self.rt.spawn(async move {
            let r = f(state).await;
            *inner.lock() = TaskState::Done(r);
            if let Some(s) = weak.upgrade() {
                s.repaint();
            }
        });
        task
    }

    /// Encerra o laço de eventos (o servidor continua no ar: é residente).
    pub fn shutdown(&self) {
        if let Some(t) = self.events_task.lock().take() {
            t.abort();
        }
    }
}

enum TaskState<T> {
    Pending,
    Done(Result<T, String>),
    Taken,
}

/// Resultado de uma chamada assíncrona, consultado pela GUI a cada quadro.
pub struct Task<T> {
    inner: Arc<Mutex<TaskState<T>>>,
}

impl<T> Clone for Task<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Task<T> {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TaskState::Pending)),
        }
    }

    pub fn pending(&self) -> bool {
        matches!(*self.inner.lock(), TaskState::Pending)
    }

    /// Devolve o resultado uma única vez (depois disso `None`).
    pub fn take(&self) -> Option<Result<T, String>> {
        let mut g = self.inner.lock();
        match std::mem::replace(&mut *g, TaskState::Taken) {
            TaskState::Done(r) => Some(r),
            TaskState::Pending => {
                *g = TaskState::Pending;
                None
            }
            TaskState::Taken => None,
        }
    }
}

/// Dado carregado do serviço com recarga sob demanda.
pub struct Loader<T> {
    pub data: Option<T>,
    pub error: Option<String>,
    task: Option<Task<T>>,
    pub loaded_at: Option<std::time::Instant>,
}

impl<T> Default for Loader<T> {
    fn default() -> Self {
        Self {
            data: None,
            error: None,
            task: None,
            loaded_at: None,
        }
    }
}

impl<T> Loader<T> {
    pub fn loading(&self) -> bool {
        self.task.as_ref().is_some_and(|t| t.pending())
    }
    /// Há uma tarefa em curso ou concluída ainda não consumida por `poll`.
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    pub fn start(&mut self, task: Task<T>) {
        self.task = Some(task);
    }
    /// Deve ser chamado a cada quadro; devolve `true` quando chegou dado novo.
    pub fn poll(&mut self) -> bool {
        let Some(t) = &self.task else { return false };
        match t.take() {
            Some(Ok(v)) => {
                self.data = Some(v);
                self.error = None;
                self.loaded_at = Some(std::time::Instant::now());
                self.task = None;
                true
            }
            Some(Err(e)) => {
                self.error = Some(e);
                self.loaded_at = Some(std::time::Instant::now());
                self.task = None;
                true
            }
            None => false,
        }
    }
    pub fn stale(&self, max_age: Duration) -> bool {
        // `busy()` e não `loading()`: uma tarefa concluída entre dois quadros ainda
        // precisa ser consumida por `poll` antes de qualquer recarga.
        !self.busy()
            && self
                .loaded_at
                .map(|t| t.elapsed() > max_age)
                .unwrap_or(true)
    }
}
