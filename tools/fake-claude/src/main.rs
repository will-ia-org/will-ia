//! fake-claude — simula o `claude -p --input-format stream-json --output-format stream-json`
//! para testes do motor. Comportamentos por prompt:
//!   "sleep N"  → demora N segundos antes do result
//!   "crash"    → sai com código 1 no meio do turno
//!   "tool"     → emite tool_use + tool_result antes do result
//!   "fail"     → result com is_error
//! Env: FAKE_CLAUDE_STALE=1 com --resume → emite result de sessão expirada e sai 1.

use std::io::{BufRead, Write};

fn out(v: serde_json::Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut session = None;
    let mut resumed = false;
    for (i, a) in args.iter().enumerate() {
        if a == "--session-id" {
            session = args.get(i + 1).cloned();
        }
        if a == "--resume" {
            session = args.get(i + 1).cloned();
            resumed = true;
        }
        if a == "-p"
            && args
                .get(i + 1)
                .map(|s| !s.starts_with("--"))
                .unwrap_or(false)
            && !args.iter().any(|x| x == "--input-format")
        {
            // modo one-shot (teste de token)
            let token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").unwrap_or_default();
            if token.starts_with("bad") {
                eprintln!("Error: Invalid authentication credentials (401)");
                std::process::exit(1);
            }
            println!("OK");
            return;
        }
    }
    let session = session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if resumed && std::env::var("FAKE_CLAUDE_STALE").ok().as_deref() == Some("1") {
        out(
            serde_json::json!({"type":"result","subtype":"error_during_execution","is_error":true,"errors":[format!("No conversation found with session ID: {session}")],"session_id":session}),
        );
        eprintln!("No conversation found with session ID: {session}");
        std::process::exit(1);
    }
    out(
        serde_json::json!({"type":"system","subtype":"init","session_id":session,"model":"fake","permissionMode":"acceptEdits","mcp_servers":[{"name":"will_ia","status":"connected"}],"tools":["Read"]}),
    );
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("control_request") {
            continue;
        }
        let text = v
            .get("message")
            .and_then(|m| m.get("content"))
            .map(|c| match c {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            })
            .unwrap_or_default();
        // replay
        out(
            serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":text}]},"session_id":session}),
        );
        let t0 = std::time::Instant::now();
        if let Some(n) = text.strip_prefix("sleep ") {
            let secs: f64 = n.trim().parse().unwrap_or(1.0);
            out(
                serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"vou demorar um pouco…"}]},"session_id":session}),
            );
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
        }
        if text.starts_with("crash") {
            out(
                serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ai…"}]},"session_id":session}),
            );
            std::process::exit(1);
        }
        if text.starts_with("tool") {
            out(
                serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"x.md"}}]},"session_id":session}),
            );
            out(
                serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"conteúdo"}]},"tool_use_result":{"stdout":"conteúdo","stderr":""},"session_id":session}),
            );
        }
        if text.starts_with("tools") {
            for (name, input, result) in [
                (
                    "Glob",
                    serde_json::json!({"pattern": "**/*.md"}),
                    "docs/01.md\ndocs/02.md",
                ),
                (
                    "Grep",
                    serde_json::json!({"pattern": "TODO", "path": "docs"}),
                    "docs/02.md:14: TODO revisar",
                ),
                (
                    "Bash",
                    serde_json::json!({"command": "ls tools"}),
                    "tools/\n  fake-claude",
                ),
            ] {
                out(
                    serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":name,"input":input}]},"session_id":session}),
                );
                std::thread::sleep(std::time::Duration::from_millis(900));
                out(
                    serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":result}]},"tool_use_result":{"stdout":result,"stderr":""},"session_id":session}),
                );
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
        let is_error = text.starts_with("fail");
        let reply = if is_error {
            "deu erro".to_string()
        } else if text.starts_with("md") {
            "## Resumo\n\nEncontrei **3 arquivos** relevantes na base:\n\n1. `docs/01.md` — visão geral\n2. `docs/02.md` — requisitos (tem um *TODO* pendente)\n3. `docs/03.md` — não funcionais\n\n```bash\ngrep -rn TODO docs/\n```\n\n| Arquivo | Linhas |\n|---|---|\n| 01.md | 120 |\n| 02.md | 340 |\n\n> Quer que eu resolva o TODO agora?".to_string()
        } else {
            format!("eco: {text}")
        };
        out(
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":reply}]},"session_id":session}),
        );
        out(
            serde_json::json!({"type":"result","subtype": if is_error {"error_during_execution"} else {"success"},"is_error":is_error,"result":reply,"total_cost_usd":0.001,"duration_ms":t0.elapsed().as_millis() as u64,"session_id":session,"usage":{"input_tokens":1,"output_tokens":1},"modelUsage":{"fake":{}}}),
        );
    }
}
