//! Ponte entre a GUI (thread principal, síncrona) e o serviço (tokio).
//!
//! O serviço (`will_server`) roda num runtime tokio em threads de fundo. A GUI
//! dispara chamadas assíncronas com [`Service::call`], que devolvem um
//! [`Task`] consultado a cada quadro; quando a tarefa termina ela pede um
//! `request_repaint`, então a tela nunca faz polling.

use parking_lot::Mutex;
use std::future::Future;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use will_client::{Client, ClientEvent};
use will_server::{RunOptions, ServerHandle};

/// Cliente da API local, compartilhado pelas páginas (`Service::call`).
pub type Api = Arc<Client>;

/// Comandos vindos de fora da GUI (bandeja, atalho global, segunda instância).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Show,
    ToggleVoice,
    /// Atalho global pressionado/solto (toggle ou push-to-talk conforme configuração).
    HotkeyDown,
    HotkeyUp,
    /// Captura a janela principal em PNG (diagnóstico/desenvolvimento), opcionalmente
    /// navegando antes para a página indicada.
    Screenshot(std::path::PathBuf, Option<String>),
    ScreenshotNow(std::path::PathBuf),
}

pub struct Service {
    rt: tokio::runtime::Runtime,
    /// Servidor em processo (etapa 1 da separação: ainda um binário só). `None`
    /// quando o app conecta num servidor externo.
    handle: Option<Arc<ServerHandle>>,
    /// Cliente HTTP/WS da API: **todo** acesso da UI ao serviço passa por ele,
    /// mesmo em loopback (`docs/26`).
    pub api: Api,
    ctx: Mutex<Option<egui::Context>>,
    cmd_tx: Sender<UiCommand>,
    events_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Service {
    /// Sobe o runtime e o serviço em processo; conecta o cliente em loopback.
    pub fn start(
        opts: RunOptions,
    ) -> anyhow::Result<(Arc<Self>, Receiver<ClientEvent>, Receiver<UiCommand>)> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("will-rt")
            .build()?;
        let handle = rt.block_on(will_server::run(opts))?;
        crate::ipc::write_port_file(&handle.state.paths, handle.addr.port());
        let api = Arc::new(Client::new(handle.url(), handle.ui_token.clone()));
        let (cmd_tx, cmd_rx) = channel();
        let svc = Arc::new(Self {
            rt,
            handle: Some(Arc::new(handle)),
            api,
            ctx: Mutex::new(None),
            cmd_tx,
            events_task: Mutex::new(None),
        });

        // Eventos do servidor (WS /api/events) → canal síncrono lido pela GUI.
        let (ev_tx, ev_rx) = channel::<ClientEvent>();
        let weak = Arc::downgrade(&svc);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.repaint();
            }
        });
        let task = svc.api.spawn_events(svc.rt.handle(), ev_tx, wake);
        *svc.events_task.lock() = Some(task);

        // Gancho para a segunda instância (`POST /api/ui/show`) enquanto o
        // servidor vive neste processo.
        if let Some(h) = &svc.handle {
            let weak = Arc::downgrade(&svc);
            let data_dir = h.state.paths.data_dir.clone();
            *h.state.ui_hook.write() = Some(Arc::new(move |cmd: &str| {
                if let Some(s) = weak.upgrade() {
                    if cmd == "show" {
                        s.send_cmd(UiCommand::Show);
                    } else if let Some(rest) = cmd.strip_prefix("screenshot") {
                        let nav = rest.strip_prefix(':').map(String::from);
                        s.send_cmd(UiCommand::Screenshot(data_dir.join("screenshot.png"), nav));
                    }
                }
            }));
        }
        // Serviço residente: SIGINT/SIGTERM/SIGHUP não derrubam o app (só kill
        // forçado). Registrar o handler substitui a ação padrão de terminar; os
        // filhos `claude` morrem junto mesmo no kill -9 (PDEATHSIG / Job Object).
        svc.rt.spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                tracing::warn!(
                    "SIGINT ignorado: o will-ia é residente; para encerrar use kill -9 (ou o Gerenciador de Tarefas)"
                );
            }
        });
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            for (kind, name) in [
                (SignalKind::terminate(), "SIGTERM"),
                (SignalKind::hangup(), "SIGHUP"),
            ] {
                svc.rt.spawn(async move {
                    let Ok(mut sig) = signal(kind) else { return };
                    while sig.recv().await.is_some() {
                        tracing::warn!(
                            "{name} ignorado: o will-ia é residente; para encerrar use kill -9"
                        );
                    }
                });
            }
        }
        Ok((svc, ev_rx, cmd_rx))
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

    /// Usado pela bandeja `ksni` (Linux); nos outros SOs a bandeja chama `send_cmd`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cmd_sender(&self) -> Sender<UiCommand> {
        self.cmd_tx.clone()
    }

    pub fn rt(&self) -> &tokio::runtime::Handle {
        self.rt.handle()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }

    /// Bloqueia a thread da GUI numa chamada ao servidor (só no arranque,
    /// para o snapshot inicial).
    pub fn block_on<T>(&self, fut: impl Future<Output = T>) -> T {
        self.rt.block_on(fut)
    }

    /// Dispara uma chamada assíncrona ao servidor. O closure recebe o cliente da API.
    pub fn call<T, Fut>(self: &Arc<Self>, f: impl FnOnce(Api) -> Fut + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        let task = Task::new();
        let inner = task.inner.clone();
        let state = self.api.clone();
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

    /// Encerra o servidor em processo (se houver) e o laço de eventos.
    pub fn shutdown(&self) {
        if let Some(t) = self.events_task.lock().take() {
            t.abort();
        }
        if let Some(h) = self.handle.clone() {
            let _ = self.rt.block_on(async move {
                tokio::time::timeout(Duration::from_secs(8), h.shutdown()).await
            });
        }
    }
}

enum TaskState<T> {
    Pending,
    Done(Result<T, String>),
    Taken,
}

/// Resultado futuro de uma chamada ao serviço, consultado pela GUI.
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
