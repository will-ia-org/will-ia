//! Arquivo `api-port` no `data_dir`: como o app (e `will-ia info`) descobrem a
//! porta da API local sem abrir o banco.

/// Grava a porta efetiva para o app e a próxima execução encontrarem o servidor.
pub fn write_port_file(paths: &will_core::AppPaths, port: u16) {
    let _ = std::fs::write(paths.data_dir.join("api-port"), port.to_string());
}
