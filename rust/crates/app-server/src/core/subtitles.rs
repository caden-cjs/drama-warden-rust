use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

const STATUSES: &[&str] = &[
    "pending",
    "processing",
    "completed",
    "skipped",
    "failed",
    "uncertain",
];

#[derive(Debug)]
pub enum SubtitleError {
    Invalid(&'static str),
    NotFound(&'static str),
    Conflict(&'static str),
    Unsupported,
    Database(sqlx::Error),
}

impl IntoResponse for SubtitleError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid subtitle request"),
            Self::NotFound(code) => (StatusCode::NOT_FOUND, code, "subtitle record not found"),
            Self::Conflict(code) => (StatusCode::CONFLICT, code, "subtitle job state changed"),
            Self::Unsupported => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "SIDECAR_WRITE_UNSUPPORTED",
                "storage provider cannot write subtitles",
            ),
            Self::Database(error) => {
                tracing::error!(error = %error, "subtitle operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "request failed",
                )
            }
        };
        (status, Json(json!({"error":message,"code":code}))).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueRequest {
    pub storage_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    pub languages: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleJob {
    pub id: i64,
    pub show_id: i64,
    pub storage_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    pub languages: Vec<String>,
    pub status: String,
    pub provider_id: Option<String>,
    pub output_path: Option<String>,
    pub attempt_count: i64,
    pub next_attempt_at: String,
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_provider_id: Option<String>,
}

fn text(row: &sqlx::any::AnyRow, column: &str) -> Result<String, SubtitleError> {
    row.try_get::<String, _>(column)
        .map_err(SubtitleError::Database)
}

fn optional_text(row: &sqlx::any::AnyRow, column: &str) -> Result<Option<String>, SubtitleError> {
    row.try_get::<Option<String>, _>(column)
        .map_err(SubtitleError::Database)
}

fn integer(row: &sqlx::any::AnyRow, column: &str) -> Result<i64, SubtitleError> {
    if let Ok(value) = row.try_get::<i64, _>(column) {
        return Ok(value);
    }
    row.try_get::<bool, _>(column)
        .map(i64::from)
        .map_err(SubtitleError::Database)
}

fn parse_languages(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value)
        .unwrap_or_else(|_| vec!["zh-CN".to_owned(), "zh-TW".to_owned()])
}

fn parse_job(row: &sqlx::any::AnyRow) -> Result<SubtitleJob, SubtitleError> {
    let status = text(row, "status")?;
    let write_started_at = optional_text(row, "write_started_at")?;
    let show_name = row.try_get::<Option<String>, _>("show_name").ok().flatten();
    let storage_provider_id = row
        .try_get::<Option<String>, _>("storage_provider_id")
        .ok()
        .flatten();
    Ok(SubtitleJob {
        id: integer(row, "id")?,
        show_id: integer(row, "show_id")?,
        storage_id: integer(row, "show_storage_id")?,
        season_number: integer(row, "season_number")?,
        episode_number: integer(row, "episode_number")?,
        languages: parse_languages(&text(row, "languages_json")?),
        retryable: status == "failed" && write_started_at.is_none(),
        status,
        provider_id: optional_text(row, "provider_id")?,
        output_path: optional_text(row, "output_path")?,
        attempt_count: integer(row, "attempt_count")?,
        next_attempt_at: text(row, "next_attempt_at")?,
        error_code: optional_text(row, "error_code")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
        completed_at: optional_text(row, "completed_at")?,
        show_name,
        storage_provider_id,
    })
}

fn positive(value: i64, field: &'static str) -> Result<i64, SubtitleError> {
    if value <= 0 {
        Err(SubtitleError::Invalid(field))
    } else {
        Ok(value)
    }
}

fn valid_language(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    let region = parts.next();
    parts.next().is_none()
        && (2..=3).contains(&language.len())
        && language.bytes().all(|byte| byte.is_ascii_lowercase())
        && region.is_none_or(|region| {
            region.len() == 2 && region.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
}

pub async fn queue(
    pool: &AnyPool,
    mysql: bool,
    show_id: i64,
    input: QueueRequest,
    plugins: &crate::plugins::PluginHost,
) -> Result<(SubtitleJob, bool), SubtitleError> {
    let show_id = positive(show_id, "invalid_show_id")?;
    let storage_id = positive(input.storage_id, "invalid_storage_id")?;
    let season = positive(input.season_number, "invalid_season_number")?;
    let episode = positive(input.episode_number, "invalid_episode_number")?;
    let mut languages = input
        .languages
        .unwrap_or_else(|| vec!["zh-CN".to_owned(), "zh-TW".to_owned()]);
    if languages.is_empty()
        || languages.len() > 5
        || languages.iter().any(|language| !valid_language(language))
    {
        return Err(SubtitleError::Invalid("invalid_languages"));
    }
    let mut unique = HashSet::new();
    languages.retain(|language| unique.insert(language.clone()));
    if serde_json::to_string(&languages).map_or(true, |encoded| encoded.len() > 200) {
        return Err(SubtitleError::Invalid("invalid_languages"));
    }
    let show = sqlx::query("SELECT id FROM shows WHERE id = ?")
        .bind(show_id)
        .fetch_optional(pool)
        .await
        .map_err(SubtitleError::Database)?;
    if show.is_none() {
        return Err(SubtitleError::NotFound("show_not_found"));
    }
    let storage = sqlx::query(
        "SELECT provider_id FROM show_storage WHERE id = ? AND show_id = ? AND enabled = 1",
    )
    .bind(storage_id)
    .bind(show_id)
    .fetch_optional(pool)
    .await
    .map_err(SubtitleError::Database)?
    .ok_or(SubtitleError::NotFound("storage_not_found"))?;
    let provider_id: String = storage
        .try_get("provider_id")
        .map_err(SubtitleError::Database)?;
    let supported = plugins.list().await.into_iter().any(|plugin| {
        plugin.id == provider_id
            && plugin.enabled
            && plugin.kind == "storage-provider"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "writeSidecar")
    });
    if !supported {
        return Err(SubtitleError::Unsupported);
    }
    let episode_exists = sqlx::query(
        "SELECT e.id FROM show_episode e JOIN show_season s ON s.id=e.season_id WHERE s.show_id=? AND s.season_number=? AND e.episode_number=?",
    )
    .bind(show_id)
    .bind(season)
    .bind(episode)
    .fetch_optional(pool)
    .await
    .map_err(SubtitleError::Database)?;
    if episode_exists.is_none() {
        return Err(SubtitleError::NotFound("episode_not_found"));
    }
    let encoded = serde_json::to_string(&languages)
        .map_err(|_| SubtitleError::Invalid("invalid_languages"))?;
    let insert = if mysql {
        "INSERT INTO subtitle_job (show_id, show_storage_id, season_number, episode_number, languages_json) VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE id=id"
    } else {
        "INSERT INTO subtitle_job (show_id, show_storage_id, season_number, episode_number, languages_json) VALUES (?, ?, ?, ?, ?) ON CONFLICT(show_storage_id, season_number, episode_number) DO NOTHING"
    };
    sqlx::query(insert)
        .bind(show_id)
        .bind(storage_id)
        .bind(season)
        .bind(episode)
        .bind(encoded)
        .execute(pool)
        .await
        .map_err(SubtitleError::Database)?;
    let row = sqlx::query("SELECT * FROM subtitle_job WHERE show_storage_id=? AND season_number=? AND episode_number=?")
        .bind(storage_id).bind(season).bind(episode)
        .fetch_one(pool).await.map_err(SubtitleError::Database)?;
    let job = parse_job(&row)?;
    let queued = job.status == "pending";
    Ok((job, queued))
}

pub async fn list(
    pool: &AnyPool,
    status: Option<&str>,
    show_id: Option<i64>,
    limit: i64,
) -> Result<Vec<SubtitleJob>, SubtitleError> {
    if status.is_some_and(|status| !STATUSES.contains(&status)) {
        return Err(SubtitleError::Invalid("invalid_subtitle_status"));
    }
    let show_id = show_id
        .map(|id| positive(id, "invalid_show_id"))
        .transpose()?;
    let limit = limit.clamp(1, 500);
    let base = "SELECT j.*, sh.name AS show_name, ss.provider_id AS storage_provider_id FROM subtitle_job j LEFT JOIN shows sh ON sh.id=j.show_id LEFT JOIN show_storage ss ON ss.id=j.show_storage_id ";
    let rows = match (status, show_id) {
        (Some(status), Some(show_id)) => sqlx::query(&format!(
            "{base}WHERE j.status=? AND j.show_id=? ORDER BY j.id DESC LIMIT ?"
        ))
        .bind(status)
        .bind(show_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(SubtitleError::Database)?,
        (Some(status), None) => sqlx::query(&format!(
            "{base}WHERE j.status=? ORDER BY j.id DESC LIMIT ?"
        ))
        .bind(status)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(SubtitleError::Database)?,
        (None, Some(show_id)) => sqlx::query(&format!(
            "{base}WHERE j.show_id=? ORDER BY j.id DESC LIMIT ?"
        ))
        .bind(show_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(SubtitleError::Database)?,
        (None, None) => sqlx::query(&format!("{base}ORDER BY j.id DESC LIMIT ?"))
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(SubtitleError::Database)?,
    };
    rows.iter().map(parse_job).collect()
}

pub async fn get(pool: &AnyPool, id: i64) -> Result<Option<SubtitleJob>, SubtitleError> {
    let id = positive(id, "invalid_subtitle_job_id")?;
    sqlx::query("SELECT * FROM subtitle_job WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(SubtitleError::Database)?
        .as_ref()
        .map(parse_job)
        .transpose()
}

static CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn claim_token() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{sequence}", std::process::id())
}

fn now_sql(mysql: bool) -> &'static str {
    if mysql {
        "UTC_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    }
}

fn lease_sql(mysql: bool) -> &'static str {
    if mysql {
        "DATE_ADD(UTC_TIMESTAMP(3), INTERVAL 5 MINUTE)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now','+5 minutes')"
    }
}

pub async fn claim_next(
    pool: &AnyPool,
    mysql: bool,
) -> Result<Option<(SubtitleJob, String)>, SubtitleError> {
    let mut transaction = pool.begin().await.map_err(SubtitleError::Database)?;
    let now = now_sql(mysql);
    let recovery = format!(
        "UPDATE subtitle_job SET status=CASE WHEN write_started_at IS NULL THEN 'pending' ELSE 'uncertain' END,locked_until=NULL,claim_token=NULL,error_code=CASE WHEN write_started_at IS NULL THEN error_code ELSE 'WORKER_LOST_DURING_WRITE' END,completed_at=CASE WHEN write_started_at IS NULL THEN NULL ELSE {now} END,updated_at={now} WHERE status='processing' AND (locked_until IS NULL OR locked_until < {now})"
    );
    sqlx::query(&recovery)
        .execute(&mut *transaction)
        .await
        .map_err(SubtitleError::Database)?;
    let selected = format!(
        "SELECT id FROM subtitle_job WHERE status='pending' AND next_attempt_at <= {now} ORDER BY id LIMIT 1"
    );
    let row = sqlx::query(&selected)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(SubtitleError::Database)?;
    let Some(row) = row else {
        transaction
            .commit()
            .await
            .map_err(SubtitleError::Database)?;
        return Ok(None);
    };
    let id: i64 = integer(&row, "id")?;
    let token = claim_token();
    let update = format!(
        "UPDATE subtitle_job SET status='processing',attempt_count=attempt_count+1,locked_until={},claim_token=?,write_started_at=NULL,error_code=NULL,updated_at={now} WHERE id=? AND status='pending'",
        lease_sql(mysql)
    );
    let result = sqlx::query(&update)
        .bind(&token)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(SubtitleError::Database)?;
    if result.rows_affected() != 1 {
        transaction
            .commit()
            .await
            .map_err(SubtitleError::Database)?;
        return Ok(None);
    }
    let row = sqlx::query("SELECT * FROM subtitle_job WHERE id=?")
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SubtitleError::Database)?;
    let job = parse_job(&row)?;
    transaction
        .commit()
        .await
        .map_err(SubtitleError::Database)?;
    Ok(Some((job, token)))
}

pub async fn context(pool: &AnyPool, id: i64) -> Result<Option<Value>, SubtitleError> {
    let Some(job) = get(pool, id).await? else {
        return Ok(None);
    };
    let show = sqlx::query("SELECT id,name,aliases_json FROM shows WHERE id=?")
        .bind(job.show_id)
        .fetch_optional(pool)
        .await
        .map_err(SubtitleError::Database)?;
    let Some(show) = show else { return Ok(None) };
    let storage = sqlx::query(
        "SELECT id,provider_id,root_path,enabled FROM show_storage WHERE id=? AND show_id=?",
    )
    .bind(job.storage_id)
    .bind(job.show_id)
    .fetch_optional(pool)
    .await
    .map_err(SubtitleError::Database)?;
    let Some(storage) = storage else {
        return Ok(None);
    };
    let assets = sqlx::query("SELECT a.relative_path,a.file_name,a.file_size FROM show_episode_asset a JOIN show_episode e ON e.id=a.episode_id JOIN show_season s ON s.id=e.season_id WHERE a.show_storage_id=? AND a.is_present=1 AND s.show_id=? AND s.season_number=? AND e.episode_number=? ORDER BY a.relative_path")
        .bind(job.storage_id).bind(job.show_id).bind(job.season_number).bind(job.episode_number)
        .fetch_all(pool).await.map_err(SubtitleError::Database)?;
    let aliases: Value =
        serde_json::from_str(&text(&show, "aliases_json")?).unwrap_or_else(|_| json!([]));
    let asset_values = assets.into_iter().map(|asset| {
        Ok(json!({
            "relativePath": text(&asset,"relative_path")?,
            "fileName": text(&asset,"file_name")?,
            "fileSize": asset.try_get::<Option<i64>,_>("file_size").map_err(SubtitleError::Database)?,
        }))
    }).collect::<Result<Vec<_>, SubtitleError>>()?;
    Ok(Some(json!({
        "job":job,
        "show":{"id":integer(&show,"id")?,"name":text(&show,"name")?,"aliases":aliases},
        "storage":{"id":integer(&storage,"id")?,"providerId":text(&storage,"provider_id")?,"rootPath":text(&storage,"root_path")?,"enabled":integer(&storage,"enabled")? != 0},
        "assets":asset_values,
    })))
}

pub async fn mark_write_started(
    pool: &AnyPool,
    id: i64,
    provider_id: &str,
    token: &str,
    mysql: bool,
) -> Result<(), SubtitleError> {
    let now = now_sql(mysql);
    let update = format!(
        "UPDATE subtitle_job SET write_started_at={now},provider_id=?,locked_until={},updated_at={now} WHERE id=? AND status='processing' AND claim_token=? AND write_started_at IS NULL",
        lease_sql(mysql)
    );
    let result = sqlx::query(&update)
        .bind(provider_id)
        .bind(id)
        .bind(token)
        .execute(pool)
        .await
        .map_err(SubtitleError::Database)?;
    if result.rows_affected() != 1 {
        return Err(SubtitleError::Conflict("subtitle_job_lease_lost"));
    }
    Ok(())
}

pub async fn complete(
    pool: &AnyPool,
    id: i64,
    provider_id: &str,
    output_path: &str,
    token: &str,
    mysql: bool,
) -> Result<(), SubtitleError> {
    if provider_id.len() > 64 || output_path.len() > 1024 {
        return Err(SubtitleError::Invalid("invalid_subtitle_result"));
    }
    let now = now_sql(mysql);
    let update = format!(
        "UPDATE subtitle_job SET status='completed',provider_id=?,output_path=?,locked_until=NULL,claim_token=NULL,completed_at={now},updated_at={now} WHERE id=? AND status='processing' AND claim_token=? AND write_started_at IS NOT NULL"
    );
    let result = sqlx::query(&update)
        .bind(provider_id)
        .bind(output_path)
        .bind(id)
        .bind(token)
        .execute(pool)
        .await
        .map_err(SubtitleError::Database)?;
    if result.rows_affected() != 1 {
        return Err(SubtitleError::Conflict("subtitle_job_state_changed"));
    }
    Ok(())
}

pub async fn skip(
    pool: &AnyPool,
    id: i64,
    code: &str,
    token: &str,
    mysql: bool,
) -> Result<(), SubtitleError> {
    let now = now_sql(mysql);
    let code = safe_error_code(code);
    let update = format!(
        "UPDATE subtitle_job SET status='skipped',error_code=?,locked_until=NULL,claim_token=NULL,completed_at={now},updated_at={now} WHERE id=? AND status='processing' AND claim_token=? AND write_started_at IS NULL"
    );
    let result = sqlx::query(&update)
        .bind(code)
        .bind(id)
        .bind(token)
        .execute(pool)
        .await
        .map_err(SubtitleError::Database)?;
    if result.rows_affected() != 1 {
        return Err(SubtitleError::Conflict("subtitle_job_state_changed"));
    }
    Ok(())
}

pub async fn fail(
    pool: &AnyPool,
    id: i64,
    code: &str,
    token: &str,
    mysql: bool,
) -> Result<(), SubtitleError> {
    let now = now_sql(mysql);
    let mut transaction = pool.begin().await.map_err(SubtitleError::Database)?;
    let row = sqlx::query(
        "SELECT status,attempt_count,write_started_at,claim_token FROM subtitle_job WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(SubtitleError::Database)?
    .ok_or(SubtitleError::Conflict("subtitle_job_lease_lost"))?;
    if text(&row, "status")? != "processing"
        || optional_text(&row, "claim_token")?.as_deref() != Some(token)
    {
        return Err(SubtitleError::Conflict("subtitle_job_lease_lost"));
    }
    let write_started = optional_text(&row, "write_started_at")?.is_some();
    let error_code = safe_error_code(code);
    if write_started {
        let update = format!(
            "UPDATE subtitle_job SET status='uncertain',error_code=?,locked_until=NULL,claim_token=NULL,completed_at={now},updated_at={now} WHERE id=? AND claim_token=?"
        );
        sqlx::query(&update)
            .bind(error_code)
            .bind(id)
            .bind(token)
            .execute(&mut *transaction)
            .await
            .map_err(SubtitleError::Database)?;
    } else {
        let attempts = integer(&row, "attempt_count")?;
        let terminal = attempts >= 3;
        let delay = 30_i64 * 2_i64.pow((attempts - 1).clamp(0, 8) as u32);
        let next = if mysql {
            format!("DATE_ADD(UTC_TIMESTAMP(3), INTERVAL {delay} SECOND)")
        } else {
            format!("strftime('%Y-%m-%dT%H:%M:%fZ','now','+{delay} seconds')")
        };
        let status = if terminal { "failed" } else { "pending" };
        let completed = if terminal { now } else { "NULL" };
        let update = format!(
            "UPDATE subtitle_job SET status='{status}',error_code=?,next_attempt_at={},locked_until=NULL,claim_token=NULL,completed_at={completed},updated_at={now} WHERE id=? AND claim_token=? AND write_started_at IS NULL",
            if terminal {
                "next_attempt_at".to_owned()
            } else {
                next
            }
        );
        sqlx::query(&update)
            .bind(error_code)
            .bind(id)
            .bind(token)
            .execute(&mut *transaction)
            .await
            .map_err(SubtitleError::Database)?;
    }
    transaction.commit().await.map_err(SubtitleError::Database)
}

fn safe_error_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        code.to_ascii_uppercase()
    } else {
        "SUBTITLE_JOB_FAILED".to_owned()
    }
}

pub async fn retry(pool: &AnyPool, id: i64, mysql: bool) -> Result<SubtitleJob, SubtitleError> {
    let id = positive(id, "invalid_subtitle_job_id")?;
    let now = now_sql(mysql);
    let update = format!(
        "UPDATE subtitle_job SET status='pending',next_attempt_at={now},completed_at=NULL,write_started_at=NULL,locked_until=NULL,claim_token=NULL,error_code=NULL,updated_at={now} WHERE id=? AND status='failed' AND write_started_at IS NULL"
    );
    let result = sqlx::query(&update)
        .bind(id)
        .execute(pool)
        .await
        .map_err(SubtitleError::Database)?;
    if result.rows_affected() != 1 {
        return Err(SubtitleError::Conflict("subtitle_job_not_retryable"));
    }
    get(pool, id)
        .await?
        .ok_or(SubtitleError::NotFound("subtitle_job_not_found"))
}
