//! Instância única por diretório de dados.
//!
//! A checagem pela API local (`POST /api/ui/show`) resolve o caso comum, mas
//! falha na corrida (duas execuções antes de a API subir) e com arquivo de
//! porta velho. O trinco é um arquivo `instance.lock` no `data_dir` travado
//! pelo SO: `flock` exclusivo (Unix) ou abertura sem compartilhamento (Windows).
//! O SO solta o trinco quando o processo morre, inclusive por `kill -9`.

use std::fs::File;
use std::path::Path;

/// Mantém o trinco enquanto vivo. Guarde até o fim do `main`.
pub struct InstanceLock {
    #[cfg(unix)]
    _lock: nix::fcntl::Flock<File>,
    #[cfg(not(unix))]
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum InstanceLockError {
    #[error("já existe uma instância do will-ia em execução")]
    Busy,
    #[error("não foi possível criar o trinco: {0}")]
    Io(#[from] std::io::Error),
}

pub const LOCK_FILE: &str = "instance.lock";

/// Tenta tomar o trinco sem bloquear.
pub fn try_lock(data_dir: &Path) -> Result<InstanceLock, InstanceLockError> {
    let res = try_lock_in(data_dir);
    match &res {
        Ok(_) => {
            tracing::info!(name: "platform", op = "instance_lock", ok = true, pid = std::process::id(), data_dir = %data_dir.display(), "trinco de instância única tomado")
        }
        Err(InstanceLockError::Busy) => {
            tracing::warn!(name: "platform", op = "instance_lock", ok = false, data_dir = %data_dir.display(), "já existe uma instância em execução")
        }
        Err(e) => {
            tracing::error!(name: "platform", op = "instance_lock", ok = false, data_dir = %data_dir.display(), error = %e, "não foi possível criar o trinco")
        }
    }
    res
}

fn try_lock_in(data_dir: &Path) -> Result<InstanceLock, InstanceLockError> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(LOCK_FILE);
    #[cfg(unix)]
    {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
            Ok(lock) => {
                use std::io::Write;
                let mut f = &*lock;
                let _ = f.set_len(0);
                let _ = write!(f, "{}", std::process::id());
                Ok(InstanceLock { _lock: lock })
            }
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => Err(InstanceLockError::Busy),
            Err((_, e)) => Err(InstanceLockError::Io(std::io::Error::from(e))),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // share_mode(0): nenhuma outra abertura é permitida enquanto este handle existir.
        match File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .share_mode(0)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.set_len(0);
                let _ = write!(file, "{}", std::process::id());
                Ok(InstanceLock { _file: file })
            }
            // ERROR_SHARING_VIOLATION (32) ou ERROR_LOCK_VIOLATION (33)
            Err(e) if matches!(e.raw_os_error(), Some(32) | Some(33)) => {
                Err(InstanceLockError::Busy)
            }
            Err(e) => Err(InstanceLockError::Io(e)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        Ok(InstanceLock { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_in_same_process_is_busy_or_ok_by_platform() {
        let dir = std::env::temp_dir().join(format!("will-ia-lock-{}", std::process::id()));
        let a = try_lock(&dir).expect("primeiro trinco");
        // flock é por descrição de arquivo aberta: segunda abertura no mesmo
        // processo também conflita (comportamento que queremos).
        let b = try_lock(&dir);
        assert!(matches!(b, Err(InstanceLockError::Busy)));
        drop(a);
        let c = try_lock(&dir);
        assert!(c.is_ok());
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
