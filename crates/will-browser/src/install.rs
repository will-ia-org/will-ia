//! Instalação do agent-browser e da skill na base de conhecimento, com
//! trabalhos em segundo plano acompanhados pela UI (`/api/browser/install/*`).

use crate::detect::{find_node_tool, path_with};
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use will_core::EventBus;

const SKILL_REPO: &str = "vercel-labs/agent-browser";
const OUTPUT_TAIL: usize = 6_000;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct InstallStatus {
    pub node: Option<String>,
    pub node_version: Option<String>,
    pub npm: Option<String>,
    pub npm_version: Option<String>,
    pub agent_browser: Option<String>,
    pub agent_browser_version: Option<String>,
    /// Chromium baixado pelo próprio agent-browser (ou Chrome que ele enxerga), via `doctor --json`.
    pub agent_browser_chrome: Option<String>,
    pub kb_path: Option<String>,
    pub skill_in_kb: bool,
    pub skill_in_kb_path: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Job {
    pub name: String,
    pub running: bool,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ok: Option<bool>,
    pub output: String,
}

pub struct Installer {
    job: Arc<RwLock<Job>>,
    bus: EventBus,
}

async fn version_of(bin: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args)
        .env("PATH", path_with(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    will_core::process::hide_console(&mut cmd);
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output())
        .await
        .ok()?
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| {
        l.trim_start_matches("agent-browser ")
            .trim_start_matches('v')
            .to_string()
    })
}

/// Caminho do Chrome que o agent-browser usa (`doctor --json` → check `chrome.installed`).
pub async fn agent_browser_chrome(bin: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["doctor", "--offline", "--quick", "--json"])
        .env("PATH", path_with(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    will_core::process::hide_console(&mut cmd);
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .ok()?
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let checks = v.get("checks")?.as_array()?;
    let msg = checks
        .iter()
        .find(|c| c.get("id").and_then(|i| i.as_str()) == Some("chrome.installed"))?
        .get("message")?
        .as_str()?;
    // "Google Chrome 151.0 at /usr/bin/google-chrome"
    let p = msg.rsplit(" at ").next()?.trim();
    (!p.is_empty() && Path::new(p).exists()).then(|| p.to_string())
}

/// Seção que o will-ia acrescenta ao final da SKILL.md copiada: o que já
/// está configurado pelo serviço e onde guardar capturas/downloads.
const WILL_IA_NOTES: &str = r#"

## Neste ambiente (will-ia)

- O CDP já está configurado pelo serviço (`AGENT_BROWSER_CDP`): **não** passe `--cdp`, `--auto-connect`
  nem `--headed`, não rode `agent-browser install` e não tente subir um navegador. Se ninguém estiver
  escutando, o will-ia sobe o navegador sozinho no primeiro comando (pode demorar alguns segundos).
- Capturas de tela e downloads vão para `inbox/browser/` (já definido por `AGENT_BROWSER_SCREENSHOT_DIR`
  e `AGENT_BROWSER_DOWNLOAD_PATH`). Prefira `agent-browser screenshot` **sem caminho**; se precisar
  nomear o arquivo, use `inbox/browser/<nome>.png`. Nunca grave imagens na raiz da base de conhecimento.
- Use `--if-changed` em capturas repetidas e `agent-browser close` ao terminar a tarefa.
"#;

fn append_will_ia_notes(skill: &Path) -> Result<(), String> {
    let cur = std::fs::read_to_string(skill).map_err(|e| e.to_string())?;
    if cur.contains("## Neste ambiente (will-ia)") {
        return Ok(());
    }
    std::fs::write(skill, format!("{}{}", cur.trim_end(), WILL_IA_NOTES)).map_err(|e| e.to_string())
}

pub fn skill_path_in_kb(kb: &Path) -> PathBuf {
    kb.join(".claude")
        .join("skills")
        .join("agent-browser")
        .join("SKILL.md")
}

impl Installer {
    pub fn new(bus: EventBus) -> Self {
        Self {
            job: Arc::new(RwLock::new(Job::default())),
            bus,
        }
    }

    pub fn job(&self) -> Job {
        self.job.read().clone()
    }

    pub async fn status(&self, kb: Option<&Path>) -> InstallStatus {
        let mut st = InstallStatus::default();
        if let Some(n) = find_node_tool("node") {
            st.node_version = version_of(&n, &["--version"]).await;
            st.node = Some(n.to_string_lossy().to_string());
        } else {
            st.warnings
                .push("Node.js não encontrado (instale em nodejs.org; o npm vem junto).".into());
        }
        if let Some(n) = find_node_tool("npm") {
            st.npm_version = version_of(&n, &["--version"]).await;
            st.npm = Some(n.to_string_lossy().to_string());
        }
        if let Some(ab) = find_node_tool("agent-browser") {
            st.agent_browser_version = version_of(&ab, &["--version"]).await;
            st.agent_browser_chrome = agent_browser_chrome(&ab).await;
            st.agent_browser = Some(ab.to_string_lossy().to_string());
        }
        if let Some(kb) = kb {
            st.kb_path = Some(kb.to_string_lossy().to_string());
            let p = skill_path_in_kb(kb);
            st.skill_in_kb = p.is_file();
            st.skill_in_kb_path = Some(p.to_string_lossy().to_string());
        } else {
            st.warnings.push(
                "Pasta da base de conhecimento não configurada: a skill não tem para onde ir."
                    .into(),
            );
        }
        st
    }

    fn start(&self, name: &str) -> Result<(), String> {
        let mut j = self.job.write();
        if j.running {
            return Err(format!("já existe um trabalho em andamento: {}", j.name));
        }
        *j = Job {
            name: name.to_string(),
            running: true,
            started_at: Some(chrono::Utc::now()),
            finished_at: None,
            ok: None,
            output: String::new(),
        };
        Ok(())
    }

    fn append(job: &Arc<RwLock<Job>>, line: &str) {
        let mut j = job.write();
        j.output.push_str(line);
        j.output.push('\n');
        if j.output.len() > OUTPUT_TAIL {
            let cut = j.output.len() - OUTPUT_TAIL;
            let idx = j
                .output
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= cut)
                .unwrap_or(cut);
            j.output.drain(..idx);
        }
    }

    fn finish(job: &Arc<RwLock<Job>>, ok: bool) {
        let mut j = job.write();
        j.running = false;
        j.ok = Some(ok);
        j.finished_at = Some(chrono::Utc::now());
    }

    /// Roda um comando em segundo plano, copiando stdout/stderr para o trabalho.
    fn spawn_job(
        &self,
        name: &str,
        bin: PathBuf,
        args: Vec<String>,
        cwd: Option<PathBuf>,
    ) -> Result<(), String> {
        self.start(name)?;
        let job = self.job.clone();
        let bus = self.bus.clone();
        let label = name.to_string();
        tokio::spawn(async move {
            Self::append(&job, &format!("$ {} {}", bin.display(), args.join(" ")));
            let mut cmd = tokio::process::Command::new(&bin);
            cmd.args(&args)
                .env("PATH", path_with(&bin))
                .env("CI", "1")
                .env("npm_config_yes", "true")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(c) = &cwd {
                cmd.current_dir(c);
            }
            will_core::process::hide_console(&mut cmd);
            will_platform::process::die_with_parent(&mut cmd);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    Self::append(&job, &format!("erro ao executar: {e}"));
                    Self::finish(&job, false);
                    return;
                }
            };
            will_platform::process::attach_to_job(&child);
            let (out, err) = (child.stdout.take(), child.stderr.take());
            let j2 = job.clone();
            let t1 = tokio::spawn(async move {
                if let Some(o) = out {
                    let mut lines = BufReader::new(o).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        Self::append(&j2, &l);
                    }
                }
            });
            let j3 = job.clone();
            let t2 = tokio::spawn(async move {
                if let Some(e) = err {
                    let mut lines = BufReader::new(e).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        Self::append(&j3, &l);
                    }
                }
            });
            let status =
                tokio::time::timeout(std::time::Duration::from_secs(900), child.wait()).await;
            let _ = t1.await;
            let _ = t2.await;
            let ok = matches!(status, Ok(Ok(s)) if s.success());
            match &status {
                Ok(Ok(s)) => Self::append(&job, &format!("[saída: {s}]")),
                Ok(Err(e)) => Self::append(&job, &format!("[erro: {e}]")),
                Err(_) => {
                    Self::append(&job, "[tempo esgotado: 15 min]");
                    let _ = child.kill().await;
                }
            }
            Self::finish(&job, ok);
            bus.log(
                if ok { "info" } else { "warn" },
                "browser",
                format!(
                    "{label}: {}",
                    if ok {
                        "concluído"
                    } else {
                        "falhou (veja a saída na página Browser CDP)"
                    }
                ),
            );
        });
        Ok(())
    }

    /// `npm install -g agent-browser` (instala ou atualiza).
    pub fn install_agent_browser(&self) -> Result<(), String> {
        let npm = find_node_tool("npm")
            .ok_or("npm não encontrado: instale o Node.js (nodejs.org) e tente de novo")?;
        self.spawn_job(
            "npm install -g agent-browser",
            npm,
            vec!["install".into(), "-g".into(), "agent-browser@latest".into()],
            None,
        )
    }

    /// `agent-browser install`: baixa o Chromium do próprio agent-browser.
    pub fn install_chromium(&self) -> Result<(), String> {
        let ab = find_node_tool("agent-browser")
            .ok_or("agent-browser não instalado; instale-o primeiro")?;
        self.spawn_job("agent-browser install", ab, vec!["install".into()], None)
    }

    /// Copia a skill que acompanha o CLI para `<kb>/.claude/skills/agent-browser/`.
    /// Sem o CLI, cai em `npx skills add vercel-labs/agent-browser -a claude-code` na KB.
    pub async fn install_skill(&self, kb: &Path) -> Result<String, String> {
        if !kb.is_dir() {
            return Err("pasta da base de conhecimento não existe".into());
        }
        if let Some(ab) = find_node_tool("agent-browser") {
            let mut cmd = tokio::process::Command::new(&ab);
            cmd.args(["skills", "path"])
                .env("PATH", path_with(&ab))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            will_core::process::hide_console(&mut cmd);
            if let Ok(Ok(out)) =
                tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await
            {
                let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let src = Path::new(&dir).join("agent-browser").join("SKILL.md");
                if src.is_file() {
                    let dst = skill_path_in_kb(kb);
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::copy(&src, &dst).map_err(|e| format!("copiar skill: {e}"))?;
                    append_will_ia_notes(&dst)?;
                    let _ = std::fs::create_dir_all(kb.join("inbox").join("browser"));
                    self.bus.log(
                        "info",
                        "browser",
                        format!("skill agent-browser copiada para {}", dst.display()),
                    );
                    return Ok(dst.to_string_lossy().to_string());
                }
            }
        }
        let npx = find_node_tool("npx").ok_or(
            "nem agent-browser nem npx encontrados; instale o Node.js e o agent-browser primeiro",
        )?;
        self.spawn_job(
            "npx skills add",
            npx,
            vec![
                "-y".into(),
                "skills".into(),
                "add".into(),
                SKILL_REPO.into(),
                "-a".into(),
                "claude-code".into(),
                "-y".into(),
            ],
            Some(kb.to_path_buf()),
        )?;
        Ok(skill_path_in_kb(kb).to_string_lossy().to_string())
    }
}
