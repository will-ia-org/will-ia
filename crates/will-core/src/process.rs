//! Utilitários para processos filhos compartilhados pelos módulos.
//!
//! No Windows, um app com subsistema GUI (sem console) que cria um processo
//! filho de console (`cmd.exe`, `claude.cmd`, `node`, `taskkill`…) faz o
//! sistema abrir uma janela de console nova para o filho: é a "tela preta que
//! pisca". A flag `CREATE_NO_WINDOW` evita isso sem afetar os pipes de
//! stdin/stdout/stderr. Em outros sistemas as funções não fazem nada.

/// `CREATE_NO_WINDOW` (winbase.h). Constante literal para não depender do
/// crate `windows-sys` aqui.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Impede que o processo filho abra uma janela de console no Windows.
pub fn hide_console(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Versão de [`hide_console`] para `std::process::Command`.
pub fn hide_console_std(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
