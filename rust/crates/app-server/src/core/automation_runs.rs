use serde::Serialize;
use serde_json::Value;
use sqlx::{AnyPool, Row};

#[derive(Debug)]
pub enum AutomationRunError {
    Invalid(&'static str),
    Database(sqlx::Error),
}

impl axum::response::IntoResponse for AutomationRunError {
    fn into_response(self) -> axum::response::Response {
        use axum::{Json, http::StatusCode};

        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid scan history query"),
            Self::Database(error) => {
                tracing::error!(error = %error, "automation scan history query failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "request failed",
                )
            }
        };
        (
            status,
            Json(serde_json::json!({"error":message,"code":code})),
        )
            .into_response()
    }
}

fn text(row: &sqlx::any::AnyRow, key: &str) -> Result<String, AutomationRunError> {
    if let Ok(value) = row.try_get::<String, _>(key) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(key).map_err(AutomationRunError::Database)?;
    String::from_utf8(bytes)
        .map_err(|error| AutomationRunError::Database(sqlx::Error::Decode(Box::new(error))))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub id: i64,
    pub run_type: String,
    pub started_at: String,
    pub completed_at: String,
    pub shows_checked: i64,
    pub storages_checked: i64,
    pub candidates_found: i64,
    pub eligible_candidates: i64,
    pub events_queued: i64,
    pub events_processed: i64,
    pub events_failed: i64,
    pub storage_limit_reached: bool,
    pub error_count: i64,
    pub errors: Value,
}

pub struct NewAutomationRun<'a> {
    pub run_type: &'a str,
    pub started_at: &'a str,
    pub completed_at: &'a str,
    pub shows_checked: i64,
    pub storages_checked: i64,
    pub candidates_found: i64,
    pub eligible_candidates: i64,
    pub events_queued: i64,
    pub events_processed: i64,
    pub events_failed: i64,
    pub storage_limit_reached: bool,
    pub error_count: i64,
    pub errors_json: &'a str,
}

pub async fn insert(pool: &AnyPool, run: NewAutomationRun<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO automation_scan_run (
          run_type, started_at, completed_at, shows_checked, storages_checked,
          candidates_found, eligible_candidates, events_queued, events_processed,
          events_failed, storage_limit_reached, error_count, errors_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run.run_type)
    .bind(run.started_at)
    .bind(run.completed_at)
    .bind(run.shows_checked)
    .bind(run.storages_checked)
    .bind(run.candidates_found)
    .bind(run.eligible_candidates)
    .bind(run.events_queued)
    .bind(run.events_processed)
    .bind(run.events_failed)
    .bind(i64::from(run.storage_limit_reached))
    .bind(run.error_count)
    .bind(run.errors_json)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list(
    pool: &AnyPool,
    limit: i64,
    offset: i64,
) -> Result<Vec<AutomationRun>, AutomationRunError> {
    if !(1..=500).contains(&limit) {
        return Err(AutomationRunError::Invalid("invalid_limit"));
    }
    if !(0..=1_000_000).contains(&offset) {
        return Err(AutomationRunError::Invalid("invalid_offset"));
    }
    let rows = sqlx::query(
        "SELECT id, CAST(run_type AS CHAR) AS run_type,
          CAST(started_at AS CHAR) AS started_at, CAST(completed_at AS CHAR) AS completed_at,
          CAST(shows_checked AS SIGNED) AS shows_checked,
          CAST(storages_checked AS SIGNED) AS storages_checked,
          CAST(candidates_found AS SIGNED) AS candidates_found,
          CAST(eligible_candidates AS SIGNED) AS eligible_candidates,
          CAST(events_queued AS SIGNED) AS events_queued,
          CAST(events_processed AS SIGNED) AS events_processed,
          CAST(events_failed AS SIGNED) AS events_failed,
          CAST(storage_limit_reached AS SIGNED) AS storage_limit_reached,
          CAST(error_count AS SIGNED) AS error_count,
          CAST(errors_json AS CHAR) AS errors_json
        FROM automation_scan_run
        ORDER BY started_at DESC, id DESC
        LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(AutomationRunError::Database)?;

    rows.iter()
        .map(|row| {
            let errors_json = text(row, "errors_json")?;
            let errors = serde_json::from_str(&errors_json).map_err(|error| {
                AutomationRunError::Database(sqlx::Error::Decode(Box::new(error)))
            })?;
            Ok(AutomationRun {
                id: row.try_get("id").map_err(AutomationRunError::Database)?,
                run_type: text(row, "run_type")?,
                started_at: text(row, "started_at")?,
                completed_at: text(row, "completed_at")?,
                shows_checked: row
                    .try_get("shows_checked")
                    .map_err(AutomationRunError::Database)?,
                storages_checked: row
                    .try_get("storages_checked")
                    .map_err(AutomationRunError::Database)?,
                candidates_found: row
                    .try_get("candidates_found")
                    .map_err(AutomationRunError::Database)?,
                eligible_candidates: row
                    .try_get("eligible_candidates")
                    .map_err(AutomationRunError::Database)?,
                events_queued: row
                    .try_get("events_queued")
                    .map_err(AutomationRunError::Database)?,
                events_processed: row
                    .try_get("events_processed")
                    .map_err(AutomationRunError::Database)?,
                events_failed: row
                    .try_get("events_failed")
                    .map_err(AutomationRunError::Database)?,
                storage_limit_reached: row
                    .try_get::<i64, _>("storage_limit_reached")
                    .map_err(AutomationRunError::Database)?
                    != 0,
                error_count: row
                    .try_get("error_count")
                    .map_err(AutomationRunError::Database)?,
                errors,
            })
        })
        .collect()
}
