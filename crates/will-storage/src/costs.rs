//! Custos de IA: tabela genérica `ai_costs` (voz, Claude, …) com resumo por período.

use crate::{fmt, now, parse_dt, Result, Storage};
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiCost {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub source: String,
    pub r#ref: Option<String>,
    pub model: Option<String>,
    pub units: serde_json::Value,
    pub usd: f64,
    pub estimated: bool,
    pub duration_secs: Option<i64>,
    pub note: Option<String>,
}

/// Novo registro de custo.
#[derive(Debug, Clone, Default)]
pub struct NewAiCost {
    pub source: String,
    pub r#ref: Option<String>,
    pub model: Option<String>,
    pub units: serde_json::Value,
    pub usd: f64,
    pub estimated: bool,
    pub duration_secs: Option<i64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CostBucket {
    pub source: String,
    pub usd: f64,
    pub count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CostSummary {
    pub today: Vec<CostBucket>,
    pub last_7d: Vec<CostBucket>,
    pub last_30d: Vec<CostBucket>,
    pub all_time: Vec<CostBucket>,
    pub today_usd: f64,
    pub last_7d_usd: f64,
    pub last_30d_usd: f64,
    pub all_time_usd: f64,
}

fn row(r: &sqlx::sqlite::SqliteRow) -> AiCost {
    let at: String = r.get("at");
    let units: String = r.get("units_json");
    AiCost {
        id: r.get("id"),
        at: parse_dt(&at),
        source: r.get("source"),
        r#ref: r.get("ref"),
        model: r.get("model"),
        units: serde_json::from_str(&units).unwrap_or(serde_json::Value::Null),
        usd: r.get("usd"),
        estimated: r.get::<i64, _>("estimated") != 0,
        duration_secs: r.get("duration_secs"),
        note: r.get("note"),
    }
}

impl Storage {
    pub async fn add_ai_cost(&self, c: &NewAiCost) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO ai_costs (at, source, ref, model, units_json, usd, estimated, duration_secs, note) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(fmt(now()))
        .bind(&c.source)
        .bind(&c.r#ref)
        .bind(&c.model)
        .bind(c.units.to_string())
        .bind(c.usd)
        .bind(c.estimated as i64)
        .bind(c.duration_secs)
        .bind(&c.note)
        .execute(&self.pool)
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn list_ai_costs(&self, limit: i64, source: Option<&str>) -> Result<Vec<AiCost>> {
        let rows = match source {
            Some(s) => {
                sqlx::query(
                    "SELECT * FROM ai_costs WHERE source = ? ORDER BY at DESC, id DESC LIMIT ?",
                )
                .bind(s)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query("SELECT * FROM ai_costs ORDER BY at DESC, id DESC LIMIT ?")
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.iter().map(row).collect())
    }

    async fn buckets_since(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
        source: Option<&str>,
    ) -> Result<Vec<CostBucket>> {
        let since_s = since.map(fmt).unwrap_or_else(|| "0000".to_string());
        let src = source.unwrap_or("");
        let rows = sqlx::query(
            "SELECT source, SUM(usd) AS usd, COUNT(*) AS c FROM ai_costs WHERE at >= ? AND (? = '' OR source = ?) GROUP BY source ORDER BY usd DESC",
        )
        .bind(since_s)
        .bind(src)
        .bind(src)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| CostBucket {
                source: r.get("source"),
                usd: r.get::<f64, _>("usd"),
                count: r.get::<i64, _>("c"),
            })
            .collect())
    }

    pub async fn ai_cost_summary(&self, source: Option<&str>) -> Result<CostSummary> {
        let now = now();
        // "Hoje" no fuso local do usuário (o banco guarda UTC): às 21h em UTC-3 a
        // meia-noite UTC já virou o dia seguinte e zeraria o total indevidamente.
        let today_start = chrono::Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| d.and_local_timezone(chrono::Local).single())
            .map(|d| d.with_timezone(&chrono::Utc))
            .unwrap_or(now);
        let today = self.buckets_since(Some(today_start), source).await?;
        let last_7d = self
            .buckets_since(Some(now - chrono::Duration::days(7)), source)
            .await?;
        let last_30d = self
            .buckets_since(Some(now - chrono::Duration::days(30)), source)
            .await?;
        let all_time = self.buckets_since(None, source).await?;
        // `fold` a partir de 0.0 (o `Sum` de f64 começa em -0.0 e imprimiria "-0.0000").
        let sum = |b: &[CostBucket]| b.iter().fold(0.0_f64, |a, x| a + x.usd);
        Ok(CostSummary {
            today_usd: sum(&today),
            last_7d_usd: sum(&last_7d),
            last_30d_usd: sum(&last_30d),
            all_time_usd: sum(&all_time),
            today,
            last_7d,
            last_30d,
            all_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_and_summarizes() {
        let st = Storage::open_memory().await.unwrap();
        st.add_ai_cost(&NewAiCost {
            source: "voice".into(),
            usd: 0.25,
            units: serde_json::json!({"audio_in": 100}),
            ..Default::default()
        })
        .await
        .unwrap();
        st.add_ai_cost(&NewAiCost {
            source: "claude".into(),
            usd: 0.05,
            estimated: false,
            ..Default::default()
        })
        .await
        .unwrap();
        let list = st.list_ai_costs(10, None).await.unwrap();
        assert_eq!(list.len(), 2);
        let only_voice = st.list_ai_costs(10, Some("voice")).await.unwrap();
        assert_eq!(only_voice.len(), 1);
        assert_eq!(only_voice[0].units["audio_in"], 100);
        let s = st.ai_cost_summary(None).await.unwrap();
        assert!((s.all_time_usd - 0.30).abs() < 1e-9);
        assert_eq!(s.today.len(), 2);
        let v = st.ai_cost_summary(Some("voice")).await.unwrap();
        assert!((v.all_time_usd - 0.25).abs() < 1e-9);
        assert_eq!(v.today.len(), 1);
    }
}
