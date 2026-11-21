//! Arquivo `api-port` no `data_dir`: como o app (e `will-ia info`) descobrem a
//! porta da API local sem abrir o banco.

/// Grava a porta efetiva para o app e a próxima execução encontrarem o servidor,
/// e em `api-scheme` se ela atende em `http` ou `https` (TLS ligado em
/// Configurações › Rede): sem isso o app e o instalador batiam em `http://` numa
/// porta TLS e davam o servidor por morto.
pub fn write_port_file(paths: &will_core::AppPaths, port: u16, scheme: &str) {
    let _ = std::fs::write(paths.data_dir.join("api-port"), port.to_string());
    let _ = std::fs::write(paths.data_dir.join("api-scheme"), scheme);
}

/// `http` ou `https`, como gravado por [`write_port_file`] (`http` se o arquivo
/// não existe: servidor antigo).
pub fn read_scheme(paths: &will_core::AppPaths) -> &'static str {
    match std::fs::read_to_string(paths.data_dir.join("api-scheme")) {
        Ok(s) if s.trim() == "https" => "https",
        _ => "http",
    }
}
