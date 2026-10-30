//! Processos filhos amarrados à vida do serviço.
//!
//! O will-ia é residente e só cai por kill forçado (`kill -9`, Gerenciador de
//! Tarefas). Nesse caso o shutdown limpo (`kill_tree`) não roda, então os
//! processos `claude` precisam morrer sozinhos:
//! - **Linux**: `PR_SET_PDEATHSIG` = SIGKILL no filho direto (via `pre_exec`).
//!   Netos (MCPs stdio do próprio claude) ficam por conta do grupo de processos
//!   no shutdown normal.
//! - **Windows**: um Job Object por processo do will-ia com
//!   `KILL_ON_JOB_CLOSE`; toda a árvore (claude.cmd → node → filhos) entra no job
//!   e morre quando o handle some, isto é, quando o will-ia termina de qualquer jeito.
//! - **macOS/BSD**: sem equivalente simples; fica o grupo de processos.

/// Marca o comando para o filho morrer junto com o processo pai (Linux).
#[allow(unused_variables)]
pub fn die_with_parent(cmd: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a closure roda entre fork e exec e só chama `prctl`, que é
        // async-signal-safe e não aloca.
        unsafe {
            cmd.pre_exec(|| {
                nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)
                    .map_err(std::io::Error::from)
            });
        }
    }
}

/// Coloca o filho recém-criado no Job Object do processo (Windows). Em outros
/// SOs não faz nada.
#[allow(unused_variables)]
pub fn attach_to_job(child: &tokio::process::Child) {
    #[cfg(windows)]
    {
        windows::attach(child);
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::RawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Handle do job como inteiro (HANDLE é ponteiro, não é `Sync`).
    static JOB: OnceLock<usize> = OnceLock::new();

    fn job() -> Option<HANDLE> {
        let h = *JOB.get_or_init(|| {
            // SAFETY: chamadas Win32 com argumentos válidos; o handle vive até o
            // fim do processo (nunca fechamos de propósito: fechar mataria o job).
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return 0;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    tracing::warn!(
                        name: "platform",
                        op = "job_object",
                        ok = false,
                        "SetInformationJobObject falhou; filhos não morrem com o serviço"
                    );
                    return 0;
                }
                job as usize
            }
        });
        (h != 0).then_some(h as HANDLE)
    }

    pub fn attach(child: &tokio::process::Child) {
        let (Some(job), Some(h)) = (job(), child.raw_handle()) else {
            return;
        };
        let h: RawHandle = h;
        // SAFETY: handles válidos; o filho acabou de ser criado por nós.
        let ok = unsafe { AssignProcessToJobObject(job, h as HANDLE) };
        if ok == 0 {
            tracing::warn!(name: "platform", op = "job_object", ok = false, pid = ?child.id(), "AssignProcessToJobObject falhou");
        } else {
            tracing::debug!(name: "platform", op = "job_object", ok = true, pid = ?child.id(), "filho anexado ao Job Object");
        }
    }
}
