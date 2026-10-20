use crate::{fmt, now, parse_dt, Result, Storage};
use sqlx::Row;
use std::collections::BTreeMap;
use will_core::{ApprovalDecision, ApprovalId, ApprovalRequest, ThreadId, ToolPolicy};

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpCallRecord {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub thread_id: Option<String>,
    pub tool: String,
    pub args_redacted: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub thread_id: Option<String>,
    pub tool: String,
    pub summary: String,
    pub args_redacted: Option<serde_json::Value>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decision: Option<String>,
    pub decided_via: Option<String>,
}

impl Storage {
    pub async fn log_mcp_call(
        &self,
        thread_id: Option<ThreadId>,
        tool: &str,
        args_redacted: &str,
        ok: bool,
        error: Option<&str>,
        duration_ms: u64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO mcp_calls (at, thread_id, tool, args_redacted, ok, error, duration_ms) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(fmt(now()))
        .bind(thread_id.map(|t| t.to_string()))
        .bind(tool)
        .bind(args_redacted)
        .bind(ok as i64)
        .bind(error)
        .bind(duration_ms as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_mcp_calls(&self, limit: i64) -> Result<Vec<McpCallRecord>> {
        let rows = sqlx::query("SELECT * FROM mcp_calls ORDER BY id DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let at: String = r.get("at");
                McpCallRecord {
                    id: r.get("id"),
                    at: parse_dt(&at),
                    thread_id: r.get("thread_id"),
                    tool: r.get("tool"),
                    args_redacted: r.get("args_redacted"),
                    ok: r.get::<i64, _>("ok") != 0,
                    error: r.get("error"),
                    duration_ms: r.get("duration_ms"),
                }
            })
            .collect())
    }

    pub async fn tool_policies(&self) -> Result<BTreeMap<String, ToolPolicy>> {
        let rows = sqlx::query("SELECT tool, policy FROM tool_policies")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let p: String = r.get("policy");
                Some((r.get::<String, _>("tool"), ToolPolicy::parse(&p)?))
            })
            .collect())
    }

    pub async fn set_tool_policy(&self, tool: &str, policy: Option<ToolPolicy>) -> Result<()> {
        match policy {
            Some(p) => {
                sqlx::query(
                    "INSERT INTO tool_policies (tool, policy) VALUES (?, ?) ON CONFLICT(tool) DO UPDATE SET policy = excluded.policy",
                )
                .bind(tool)
                .bind(p.as_str())
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM tool_policies WHERE tool = ?")
                    .bind(tool)
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn record_approval_request(&self, req: &ApprovalRequest) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO approvals (id, requested_at, thread_id, tool, summary, args_redacted) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(req.id.to_string())
        .bind(fmt(req.requested_at))
        .bind(req.thread_id.map(|t| t.to_string()))
        .bind(&req.tool)
        .bind(&req.summary)
        .bind(req.args_redacted.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_approval_decision(
        &self,
        id: ApprovalId,
        decision: ApprovalDecision,
        via: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE approvals SET decided_at = ?, decision = ?, decided_via = ? WHERE id = ?",
        )
        .bind(fmt(now()))
        .bind(decision.as_str())
        .bind(via)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_approvals(&self, limit: i64) -> Result<Vec<ApprovalRecord>> {
        let rows = sqlx::query("SELECT * FROM approvals ORDER BY requested_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let at: String = r.get("requested_at");
                let dec: Option<String> = r.get("decided_at");
                let args: Option<String> = r.get("args_redacted");
                ApprovalRecord {
                    id: r.get("id"),
                    requested_at: parse_dt(&at),
                    thread_id: r.get("thread_id"),
                    tool: r.get("tool"),
                    summary: r.get("summary"),
                    args_redacted: args.and_then(|a| serde_json::from_str(&a).ok()),
                    decided_at: dec.as_deref().map(parse_dt),
                    decision: r.get("decision"),
                    decided_via: r.get("decided_via"),
                }
            })
            .collect())
    }

    pub async fn mcp_json_hash(&self) -> Result<Option<String>> {
        let r = sqlx::query("SELECT last_generated_hash FROM mcp_json_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.and_then(|r| r.get::<Option<String>, _>("last_generated_hash")))
    }

    pub async fn set_mcp_json_hash(&self, hash: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO mcp_json_state (id, last_generated_hash, last_generated_at) VALUES (1, ?, ?) ON CONFLICT(id) DO UPDATE SET last_generated_hash = excluded.last_generated_hash, last_generated_at = excluded.last_generated_at",
        )
        .bind(hash)
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
