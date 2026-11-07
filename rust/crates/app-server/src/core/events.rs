use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{AnyPool, Row};

#[derive(Debug)]
pub enum EventError {
    Invalid(&'static str),
    NotFound,
    Conflict(&'static str),
    Database(sqlx::Error),
}

impl IntoResponse for EventError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid event request"),
            Self::NotFound => (StatusCode::NOT_FOUND, "event_not_found", "event not found"),
            Self::Conflict(code) => (
                StatusCode::CONFLICT,
                code,
                "event state does not allow this operation",
            ),
            Self::Database(error) => {
                tracing::error!(error = %error, "event database operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "request failed",
                )
            }
        };
        (
            status,
            Json(serde_json::json!({"error": message, "code": code})),
        )
            .into_response()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: i64,
    pub show_id: Option<i64>,
    pub kind: String,
    pub status: String,
    pub source: String,
    pub payload: Value,
    pub correlation_id: Option<String>,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub retryable: bool,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Build the small, safe event envelope exposed to notification plugins.
/// Stored payloads can contain share URLs and provider metadata, so only
/// status and episode identifiers cross the plugin boundary.
pub fn notification_event(event: &Event) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("status".to_owned(), Value::String(event.status.clone()));
    payload.insert("kind".to_owned(), Value::String(event.kind.clone()));
    if let Some(show_id) = event.show_id {
        payload.insert("showId".to_owned(), Value::from(show_id));
    }
    if let Some(season) = event.payload.get("seasonNumber").and_then(Value::as_i64) {
        payload.insert("seasonNumber".to_owned(), Value::from(season));
    }
    let episodes: Vec<Value> = event
        .payload
        .get("selectedFiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("episodeNumber").and_then(Value::as_i64))
        .filter(|episode| *episode > 0)
        .map(Value::from)
        .collect();
    if !episodes.is_empty() {
        payload.insert("episodeNumbers".to_owned(), Value::Array(episodes));
    }
    serde_json::json!({
        "type": "workflow.event.status-changed",
        "correlationId": event.correlation_id,
        "id": event.id,
        "showId": event.show_id,
        "payload": Value::Object(payload),
    })
}

#[cfg(test)]
mod notification_event_tests {
    use super::{Event, notification_event};
    use serde_json::json;

    #[test]
    fn notification_event_contains_status_and_episode_ids_only() {
        let event = Event {
            id: 42,
            show_id: Some(7),
            kind: "new_ep".to_owned(),
            status: "processed".to_owned(),
            source: "automation".to_owned(),
            payload: json!({
                "seasonNumber": 2,
                "shareUrl": "https://secret.invalid/share?token=abc",
                "selectedFiles": [{"episodeNumber": 9, "url": "https://secret.invalid/file"}]
            }),
            correlation_id: Some("corr-1".to_owned()),
            attempt_count: 1,
            last_error: None,
            retryable: false,
            started_at: None,
            completed_at: None,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        };

        let notification = notification_event(&event);
        assert_eq!(notification["type"], "workflow.event.status-changed");
        assert_eq!(notification["payload"]["status"], "processed");
        assert_eq!(notification["payload"]["seasonNumber"], 2);
        assert_eq!(notification["payload"]["episodeNumbers"], json!([9]));
        assert!(!notification.to_string().contains("secret.invalid"));
        assert!(!notification.to_string().contains("abc"));
    }
}

#[derive(Clone, Debug)]
pub struct FollowupWorkItem {
    pub id: i64,
    pub show_id: i64,
    pub correlation_id: Option<String>,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub struct FollowupRecoveryItem {
    pub id: i64,
    pub show_id: i64,
    pub status: String,
    pub last_error: Option<String>,
    pub correlation_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEvent {
    pub show_id: Option<i64>,
    pub kind: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_payload")]
    pub payload: Value,
    pub correlation_id: Option<String>,
}

fn default_source() -> String {
    "manual".to_owned()
}
fn default_payload() -> Value {
    serde_json::json!({})
}

fn redact_payload(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(redact_payload).collect()),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, child)| {
                    let sensitive = [
                        "url",
                        "uri",
                        "link",
                        "token",
                        "cookie",
                        "password",
                        "secret",
                        "authorization",
                    ]
                    .iter()
                    .any(|needle| key.to_lowercase().contains(needle));
                    (
                        key,
                        if sensitive {
                            Value::String("[redacted]".to_owned())
                        } else {
                            redact_payload(child)
                        },
                    )
                })
                .collect(),
        ),
        scalar => scalar,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionEvent {
    pub status: String,
    pub error: Option<String>,
}

fn text(row: &sqlx::any::AnyRow, key: &str) -> Result<String, EventError> {
    if let Ok(value) = row.try_get::<String, _>(key) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(key).map_err(EventError::Database)?;
    String::from_utf8(bytes)
        .map_err(|error| EventError::Database(sqlx::Error::Decode(Box::new(error))))
}

fn optional_text(row: &sqlx::any::AnyRow, key: &str) -> Result<Option<String>, EventError> {
    let value: Option<String> = match row.try_get(key) {
        Ok(value) => value,
        Err(_) => match row.try_get::<Option<Vec<u8>>, _>(key) {
            Ok(Some(bytes)) => Some(
                String::from_utf8(bytes)
                    .map_err(|error| EventError::Database(sqlx::Error::Decode(Box::new(error))))?,
            ),
            Ok(None) => None,
            Err(error) => return Err(EventError::Database(error)),
        },
    };
    Ok(value)
}

fn parse_event(row: &sqlx::any::AnyRow) -> Result<Event, EventError> {
    let payload_text = text(row, "payload_json")?;
    let payload = redact_payload(
        serde_json::from_str(&payload_text).unwrap_or_else(|_| serde_json::json!({})),
    );
    let status = text(row, "status")?;
    let last_error = optional_text(row, "last_error")?;
    let claims: i64 = row
        .try_get("write_claim_count")
        .map_err(EventError::Database)?;
    let non_reserved: i64 = row
        .try_get("non_reserved_write_claim_count")
        .map_err(EventError::Database)?;
    let retryable = status == "failed"
        && (claims == 0
            || (last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("INTERRUPTED_REVIEW_CONFIRMED_NO_OUTPUT"))
                && non_reserved == 0));
    Ok(Event {
        id: row.try_get("id").map_err(EventError::Database)?,
        show_id: row.try_get("show_id").map_err(EventError::Database)?,
        kind: text(row, "kind")?,
        status,
        source: text(row, "source")?,
        payload,
        correlation_id: optional_text(row, "correlation_id")?,
        attempt_count: row.try_get("attempt_count").map_err(EventError::Database)?,
        last_error,
        retryable,
        started_at: optional_text(row, "started_at")?,
        completed_at: optional_text(row, "completed_at")?,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
    })
}

const SELECT_EVENT: &str = "SELECT e.id, e.show_id, CAST(e.kind AS CHAR) AS kind, CAST(e.status AS CHAR) AS status, CAST(e.source AS CHAR) AS source, CAST(e.payload_json AS CHAR) AS payload_json, CAST(e.correlation_id AS CHAR) AS correlation_id, e.attempt_count, CAST(e.last_error AS CHAR) AS last_error, CAST(e.started_at AS CHAR) AS started_at, CAST(e.completed_at AS CHAR) AS completed_at, CAST(e.created_at AS CHAR) AS created_at, CAST(e.updated_at AS CHAR) AS updated_at, (SELECT COUNT(*) FROM show_episode_write_claim c WHERE c.event_id=e.id) AS write_claim_count, (SELECT COUNT(*) FROM show_episode_write_claim c WHERE c.event_id=e.id AND c.status <> 'reserved') AS non_reserved_write_claim_count FROM update_event e";

pub async fn list(
    pool: &AnyPool,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Event>, EventError> {
    if status.is_some_and(|value| {
        !["pending", "processing", "processed", "skipped", "failed"].contains(&value)
    }) {
        return Err(EventError::Invalid("invalid_event_status"));
    }
    let query = format!(
        "{SELECT_EVENT}{} ORDER BY e.created_at DESC, e.id DESC LIMIT ? OFFSET ?",
        if status.is_some() {
            " WHERE e.status = ?"
        } else {
            ""
        }
    );
    let mut request = sqlx::query(&query);
    if let Some(status) = status {
        request = request.bind(status);
    }
    let rows = request
        .bind(limit.clamp(1, 1000))
        .bind(offset.max(0))
        .fetch_all(pool)
        .await
        .map_err(EventError::Database)?;
    rows.iter().map(parse_event).collect()
}

pub async fn get(pool: &AnyPool, id: i64) -> Result<Event, EventError> {
    let row = sqlx::query(&format!("{SELECT_EVENT} WHERE e.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(EventError::Database)?;
    row.as_ref()
        .map(parse_event)
        .transpose()?
        .ok_or(EventError::NotFound)
}

pub async fn begin_followup_process(
    pool: &AnyPool,
    id: i64,
    mysql: bool,
) -> Result<FollowupWorkItem, EventError> {
    if id <= 0 {
        return Err(EventError::Invalid("invalid_event_id"));
    }
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let query = format!(
        "SELECT id, show_id, kind, status, payload_json, correlation_id FROM update_event WHERE id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&query)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let kind: String = row.try_get("kind").map_err(EventError::Database)?;
    let status: String = row.try_get("status").map_err(EventError::Database)?;
    if kind != "new_ep" {
        return Err(EventError::Conflict("followup_event_required"));
    }
    if status != "pending" {
        return Err(EventError::Conflict("event_not_processable"));
    }
    let payload_text: String = row
        .try_get("payload_json")
        .or_else(|_| {
            row.try_get::<Vec<u8>, _>("payload_json").and_then(|bytes| {
                String::from_utf8(bytes).map_err(|error| sqlx::Error::Decode(Box::new(error)))
            })
        })
        .map_err(EventError::Database)?;
    let payload = serde_json::from_str(&payload_text)
        .map_err(|_| EventError::Invalid("invalid_followup_payload"))?;
    let show_id: i64 = row.try_get("show_id").map_err(EventError::Database)?;
    let correlation_id = optional_text(&row, "correlation_id")?;
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    let update = sqlx::query(&format!("UPDATE update_event SET status='processing', attempt_count=attempt_count+1, started_at={timestamp}, completed_at=NULL, last_error=NULL, updated_at={timestamp} WHERE id=? AND status='pending'"))
        .bind(id).execute(&mut *tx).await.map_err(EventError::Database)?;
    if update.rows_affected() != 1 {
        return Err(EventError::Conflict("event_changed_concurrently"));
    }
    tx.commit().await.map_err(EventError::Database)?;
    Ok(FollowupWorkItem {
        id,
        show_id,
        correlation_id,
        payload,
    })
}

pub async fn verify_followup_claims(
    pool: &AnyPool,
    event_id: i64,
    storage_id: i64,
    season_number: i64,
    episode_numbers: &[i64],
) -> Result<(), EventError> {
    let rows = sqlx::query("SELECT show_storage_id, season_number, episode_number, status FROM show_episode_write_claim WHERE event_id = ? ORDER BY episode_number")
        .bind(event_id).fetch_all(pool).await.map_err(EventError::Database)?;
    let mut actual = Vec::with_capacity(rows.len());
    for row in &rows {
        let storage: i64 = row
            .try_get("show_storage_id")
            .map_err(EventError::Database)?;
        let season: i64 = row.try_get("season_number").map_err(EventError::Database)?;
        let episode: i64 = row
            .try_get("episode_number")
            .map_err(EventError::Database)?;
        let status: String = row
            .try_get("status")
            .or_else(|_| {
                row.try_get::<Vec<u8>, _>("status").and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|error| sqlx::Error::Decode(Box::new(error)))
                })
            })
            .map_err(EventError::Database)?;
        if storage != storage_id || season != season_number || status != "reserved" {
            return Err(EventError::Conflict("write_claims_require_review"));
        }
        actual.push(episode);
    }
    let mut expected = episode_numbers.to_vec();
    expected.sort_unstable();
    expected.dedup();
    actual.sort_unstable();
    if actual != expected || actual.is_empty() {
        return Err(EventError::Conflict("write_claims_incomplete"));
    }
    Ok(())
}

pub async fn get_followup_for_recovery(
    pool: &AnyPool,
    id: i64,
) -> Result<FollowupRecoveryItem, EventError> {
    let row = sqlx::query("SELECT id, show_id, kind, status, payload_json, last_error, correlation_id FROM update_event WHERE id = ?")
        .bind(id).fetch_optional(pool).await.map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let status = text(&row, "status")?;
    let kind = text(&row, "kind")?;
    if kind != "new_ep" || !["processing", "failed"].contains(&status.as_str()) {
        return Err(EventError::Conflict("event_not_reviewable"));
    }
    let payload_text = text(&row, "payload_json")?;
    let payload = serde_json::from_str(&payload_text)
        .map_err(|_| EventError::Invalid("invalid_followup_payload"))?;
    Ok(FollowupRecoveryItem {
        id: row.try_get("id").map_err(EventError::Database)?,
        show_id: row.try_get("show_id").map_err(EventError::Database)?,
        status,
        last_error: optional_text(&row, "last_error")?,
        correlation_id: optional_text(&row, "correlation_id")?,
        payload,
    })
}

pub async fn verify_recovery_claims(
    pool: &AnyPool,
    event: &FollowupRecoveryItem,
    storage_id: i64,
    season_number: i64,
    episode_numbers: &[i64],
    action: &str,
    mysql: bool,
) -> Result<(), EventError> {
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let query = format!(
        "SELECT show_storage_id, season_number, episode_number, status FROM show_episode_write_claim WHERE event_id = ? ORDER BY show_storage_id, season_number, episode_number{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let claims = sqlx::query(&query)
        .bind(event.id)
        .fetch_all(&mut *tx)
        .await
        .map_err(EventError::Database)?;
    let reviewed_no_output = event
        .last_error
        .as_deref()
        .is_some_and(|value| value.starts_with("INTERRUPTED_REVIEW_CONFIRMED_NO_OUTPUT"));
    let mut actual = Vec::with_capacity(claims.len());
    for claim in &claims {
        let claim_storage: i64 = claim
            .try_get("show_storage_id")
            .map_err(EventError::Database)?;
        let claim_season: i64 = claim
            .try_get("season_number")
            .map_err(EventError::Database)?;
        let claim_episode: i64 = claim
            .try_get("episode_number")
            .map_err(EventError::Database)?;
        let status = text(claim, "status")?;
        let valid_status = match event.status.as_str() {
            "processing" => status == "reserved",
            "failed" => status == "uncertain" || (reviewed_no_output && status == "reserved"),
            _ => false,
        };
        if claim_storage != storage_id || claim_season != season_number || !valid_status {
            return Err(EventError::Conflict("write_claims_changed"));
        }
        actual.push(claim_episode);
    }
    actual.sort_unstable();
    let mut expected = episode_numbers.to_vec();
    expected.sort_unstable();
    expected.dedup();
    let valid =
        if action == "retry_after_review" && event.status == "processing" && claims.is_empty() {
            true
        } else {
            !claims.is_empty() && actual == expected && expected.len() == episode_numbers.len()
        };
    if !valid {
        return Err(EventError::Conflict("write_claims_changed"));
    }
    tx.commit().await.map_err(EventError::Database)
}

pub async fn recover_followup_no_output(
    pool: &AnyPool,
    event_id: i64,
    storage_id: i64,
    season_number: i64,
    episode_numbers: &[i64],
    mysql: bool,
) -> Result<Event, EventError> {
    let mut expected = episode_numbers.to_vec();
    expected.sort_unstable();
    expected.dedup();
    if expected.is_empty() || expected.len() != episode_numbers.len() {
        return Err(EventError::Invalid("invalid_followup_selection"));
    }
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let event_query = format!(
        "SELECT kind, status, last_error FROM update_event WHERE id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let event = sqlx::query(&event_query)
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let kind = text(&event, "kind")?;
    let status = text(&event, "status")?;
    let last_error = optional_text(&event, "last_error")?;
    if kind != "new_ep" || !["processing", "failed"].contains(&status.as_str()) {
        return Err(EventError::Conflict("event_not_reviewable"));
    }
    let claim_query = format!(
        "SELECT show_storage_id, season_number, episode_number, status FROM show_episode_write_claim WHERE event_id = ? ORDER BY show_storage_id, season_number, episode_number{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let claims = sqlx::query(&claim_query)
        .bind(event_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(EventError::Database)?;
    let mut actual = Vec::with_capacity(claims.len());
    let reviewed_no_output = last_error
        .as_deref()
        .is_some_and(|value| value.starts_with("INTERRUPTED_REVIEW_CONFIRMED_NO_OUTPUT"));
    for claim in &claims {
        let storage: i64 = claim
            .try_get("show_storage_id")
            .map_err(EventError::Database)?;
        let season: i64 = claim
            .try_get("season_number")
            .map_err(EventError::Database)?;
        let episode: i64 = claim
            .try_get("episode_number")
            .map_err(EventError::Database)?;
        let claim_status = text(claim, "status")?;
        let status_valid = match status.as_str() {
            "processing" => claim_status == "reserved",
            "failed" => {
                claim_status == "uncertain" || (reviewed_no_output && claim_status == "reserved")
            }
            _ => false,
        };
        if storage != storage_id || season != season_number || !status_valid {
            return Err(EventError::Conflict("write_claims_changed"));
        }
        actual.push(episode);
    }
    actual.sort_unstable();
    let claims_valid = if status == "processing" && claims.is_empty() {
        true
    } else {
        actual == expected
    };
    if !claims_valid || (status == "failed" && claims.is_empty()) {
        return Err(EventError::Conflict("write_claims_changed"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    if status == "failed" && !reviewed_no_output {
        sqlx::query(&format!("UPDATE show_episode_write_claim SET status='reserved', updated_at={timestamp} WHERE event_id=? AND status='uncertain'"))
            .bind(event_id).execute(&mut *tx).await.map_err(EventError::Database)?;
    }
    let new_error = match last_error.as_deref() {
        Some(value) if reviewed_no_output => value.to_owned(),
        Some(value) => format!(
            "INTERRUPTED_REVIEW_CONFIRMED_NO_OUTPUT: {}",
            value.chars().take(512).collect::<String>()
        ),
        None => "INTERRUPTED_REVIEW_CONFIRMED_NO_OUTPUT".to_owned(),
    };
    sqlx::query(&format!("UPDATE update_event SET status='failed', last_error=?, completed_at={timestamp}, updated_at={timestamp} WHERE id=? AND status=?"))
        .bind(new_error).bind(event_id).bind(status).execute(&mut *tx).await.map_err(EventError::Database)?;
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, event_id).await
}

pub async fn finalize_recovered_followup(
    pool: &AnyPool,
    event_id: i64,
    storage_id: i64,
    season_number: i64,
    episode_numbers: &[i64],
    mysql: bool,
) -> Result<Event, EventError> {
    let mut expected = episode_numbers.to_vec();
    expected.sort_unstable();
    expected.dedup();
    if expected.is_empty() || expected.len() != episode_numbers.len() {
        return Err(EventError::Invalid("invalid_followup_selection"));
    }
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let event_query = format!(
        "SELECT kind, status FROM update_event WHERE id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let event = sqlx::query(&event_query)
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let kind = text(&event, "kind")?;
    let status = text(&event, "status")?;
    if kind != "new_ep" || !["processing", "failed"].contains(&status.as_str()) {
        return Err(EventError::Conflict("event_not_reviewable"));
    }
    let claim_query = format!(
        "SELECT show_storage_id, season_number, episode_number, status FROM show_episode_write_claim WHERE event_id = ? ORDER BY show_storage_id, season_number, episode_number{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let claims = sqlx::query(&claim_query)
        .bind(event_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(EventError::Database)?;
    let mut actual = Vec::with_capacity(claims.len());
    for claim in &claims {
        let claim_storage: i64 = claim
            .try_get("show_storage_id")
            .map_err(EventError::Database)?;
        let claim_season: i64 = claim
            .try_get("season_number")
            .map_err(EventError::Database)?;
        let claim_episode: i64 = claim
            .try_get("episode_number")
            .map_err(EventError::Database)?;
        let claim_status = text(claim, "status")?;
        if claim_storage != storage_id
            || claim_season != season_number
            || !["reserved", "uncertain"].contains(&claim_status.as_str())
        {
            return Err(EventError::Conflict("write_claims_changed"));
        }
        actual.push(claim_episode);
    }
    actual.sort_unstable();
    if actual != expected {
        return Err(EventError::Conflict("write_claims_changed"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    let completed = sqlx::query(&format!("UPDATE show_episode_write_claim SET status='completed', updated_at={timestamp} WHERE event_id=? AND status IN ('reserved','uncertain')"))
        .bind(event_id).execute(&mut *tx).await.map_err(EventError::Database)?;
    if completed.rows_affected() as usize != expected.len() {
        return Err(EventError::Conflict("write_claims_changed"));
    }
    sqlx::query(&format!("UPDATE update_event SET status='processed', last_error=NULL, completed_at={timestamp}, updated_at={timestamp} WHERE id=? AND status=?"))
        .bind(event_id).bind(status).execute(&mut *tx).await.map_err(EventError::Database)?;
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, event_id).await
}

pub async fn finish_followup_processed(
    pool: &AnyPool,
    event_id: i64,
    storage_id: i64,
    season_number: i64,
    episode_numbers: &[i64],
    mysql: bool,
) -> Result<Event, EventError> {
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let query = format!(
        "SELECT kind, status FROM update_event WHERE id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&query)
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let kind: String = row.try_get("kind").map_err(EventError::Database)?;
    let status: String = row.try_get("status").map_err(EventError::Database)?;
    if kind != "new_ep" || status != "processing" {
        return Err(EventError::Conflict("event_not_processing"));
    }
    let claims = sqlx::query("SELECT show_storage_id, season_number, episode_number, status FROM show_episode_write_claim WHERE event_id = ?")
        .bind(event_id).fetch_all(&mut *tx).await.map_err(EventError::Database)?;
    let mut actual = Vec::with_capacity(claims.len());
    for claim in &claims {
        let claim_storage: i64 = claim
            .try_get("show_storage_id")
            .map_err(EventError::Database)?;
        let claim_season: i64 = claim
            .try_get("season_number")
            .map_err(EventError::Database)?;
        let claim_episode: i64 = claim
            .try_get("episode_number")
            .map_err(EventError::Database)?;
        let claim_status: String = claim
            .try_get("status")
            .or_else(|_| {
                claim.try_get::<Vec<u8>, _>("status").and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|error| sqlx::Error::Decode(Box::new(error)))
                })
            })
            .map_err(EventError::Database)?;
        if claim_storage != storage_id
            || claim_season != season_number
            || claim_status != "reserved"
        {
            return Err(EventError::Conflict("write_claims_require_review"));
        }
        actual.push(claim_episode);
    }
    let mut expected = episode_numbers.to_vec();
    expected.sort_unstable();
    expected.dedup();
    actual.sort_unstable();
    if actual != expected || actual.is_empty() {
        return Err(EventError::Conflict("write_claims_incomplete"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    sqlx::query(&format!("UPDATE show_episode_write_claim SET status='completed', updated_at={timestamp} WHERE event_id=? AND status='reserved'"))
        .bind(event_id).execute(&mut *tx).await.map_err(EventError::Database)?;
    sqlx::query(&format!("UPDATE update_event SET status='processed', completed_at={timestamp}, updated_at={timestamp}, last_error=NULL WHERE id=? AND status='processing'"))
        .bind(event_id).execute(&mut *tx).await.map_err(EventError::Database)?;
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, event_id).await
}

pub async fn finish_followup_failed(
    pool: &AnyPool,
    event_id: i64,
    error: &str,
    status: &str,
    write_attempted: bool,
    mysql: bool,
) -> Result<Event, EventError> {
    if !["failed", "skipped"].contains(&status) {
        return Err(EventError::Invalid("invalid_followup_terminal_status"));
    }
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let query = format!(
        "SELECT kind, status FROM update_event WHERE id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&query)
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EventError::Database)?
        .ok_or(EventError::NotFound)?;
    let kind: String = row.try_get("kind").map_err(EventError::Database)?;
    let current: String = row.try_get("status").map_err(EventError::Database)?;
    if kind != "new_ep" || current != "processing" {
        return Err(EventError::Conflict("event_not_processing"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    if write_attempted {
        sqlx::query(&format!("UPDATE show_episode_write_claim SET status='uncertain', updated_at={timestamp} WHERE event_id=? AND status='reserved'"))
            .bind(event_id).execute(&mut *tx).await.map_err(EventError::Database)?;
    } else {
        sqlx::query("DELETE FROM show_episode_write_claim WHERE event_id=? AND status='reserved'")
            .bind(event_id)
            .execute(&mut *tx)
            .await
            .map_err(EventError::Database)?;
    }
    sqlx::query(&format!("UPDATE update_event SET status=?, completed_at={timestamp}, updated_at={timestamp}, last_error=? WHERE id=? AND status='processing'"))
        .bind(status).bind(if status == "failed" { Some(error) } else { None }).bind(event_id)
        .execute(&mut *tx).await.map_err(EventError::Database)?;
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, event_id).await
}

pub async fn create(pool: &AnyPool, input: CreateEvent, mysql: bool) -> Result<Event, EventError> {
    let kind = input.kind.trim();
    let source = input.source.trim();
    if kind.is_empty() || kind.len() > 64 {
        return Err(EventError::Invalid("invalid_event_kind"));
    }
    if kind == "new_ep" {
        return Err(EventError::Conflict("followup_queue_required"));
    }
    if source.is_empty() || source.len() > 64 {
        return Err(EventError::Invalid("invalid_event_source"));
    }
    if !input.payload.is_object() {
        return Err(EventError::Invalid("invalid_event_payload"));
    }
    if input.show_id.is_some_and(|id| id <= 0) {
        return Err(EventError::Invalid("invalid_show_id"));
    }
    if input
        .correlation_id
        .as_deref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
    {
        return Err(EventError::Invalid("invalid_correlation_id"));
    }
    let payload = serde_json::to_string(&input.payload)
        .map_err(|_| EventError::Invalid("invalid_event_payload"))?;
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    sqlx::query("INSERT INTO update_event (show_id, kind, source, payload_json, correlation_id) VALUES (?, ?, ?, ?, ?)")
        .bind(input.show_id).bind(kind).bind(source).bind(payload).bind(input.correlation_id)
        .execute(&mut *tx).await.map_err(EventError::Database)?;
    let id: i64 = if mysql {
        sqlx::query_scalar("SELECT LAST_INSERT_ID()")
            .fetch_one(&mut *tx)
            .await
    } else {
        sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await
    }
    .map_err(EventError::Database)?;
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, id).await
}

pub async fn queue_followup(
    pool: &AnyPool,
    show_id: i64,
    storage_id: i64,
    provider_id: &str,
    share_url: &str,
    share_title: Option<&str>,
    season_number: i64,
    selected_files: &[Value],
    estimated_bytes: u64,
    source: &str,
    correlation_id: &str,
    mysql: bool,
) -> Result<(Event, Vec<Value>), EventError> {
    if show_id <= 0
        || storage_id <= 0
        || season_number <= 0
        || provider_id.is_empty()
        || share_url.trim().is_empty()
        || selected_files.is_empty()
        || !["manual", "automation"].contains(&source)
        || correlation_id.trim().is_empty()
        || correlation_id.len() > 128
    {
        return Err(EventError::Invalid("invalid_followup_queue_request"));
    }
    let mut episode_numbers = Vec::with_capacity(selected_files.len());
    for file in selected_files {
        let number = file
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .filter(|number| *number > 0)
            .ok_or(EventError::Invalid("invalid_followup_selection"))?;
        if episode_numbers.contains(&number) {
            return Err(EventError::Invalid("duplicate_followup_episode"));
        }
        episode_numbers.push(number);
    }
    let payload = serde_json::json!({
        "storageId":storage_id,
        "providerId":provider_id,
        "shareUrl":share_url,
        "shareTitle":share_title,
        "seasonNumber":season_number,
        "selectedFiles":selected_files,
        "estimatedBytes":estimated_bytes,
    });
    let payload = serde_json::to_string(&payload)
        .map_err(|_| EventError::Invalid("invalid_followup_selection"))?;
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    sqlx::query("INSERT INTO update_event (show_id, kind, source, payload_json, correlation_id) VALUES (?, 'new_ep', ?, ?, ?)")
        .bind(show_id).bind(source).bind(payload).bind(correlation_id)
        .execute(&mut *tx).await.map_err(EventError::Database)?;
    let id: i64 = if mysql {
        sqlx::query_scalar("SELECT LAST_INSERT_ID()")
            .fetch_one(&mut *tx)
            .await
    } else {
        sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *tx)
            .await
    }
    .map_err(EventError::Database)?;

    let claim_insert = if mysql {
        "INSERT INTO show_episode_write_claim (show_storage_id, season_number, episode_number, event_id, status) VALUES (?, ?, ?, ?, 'reserved') ON DUPLICATE KEY UPDATE id = id"
    } else {
        "INSERT INTO show_episode_write_claim (show_storage_id, season_number, episode_number, event_id, status) VALUES (?, ?, ?, ?, 'reserved') ON CONFLICT(show_storage_id, season_number, episode_number) DO NOTHING"
    };
    let mut blocked = Vec::new();
    for episode_number in episode_numbers {
        let result = sqlx::query(claim_insert)
            .bind(storage_id)
            .bind(season_number)
            .bind(episode_number)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(EventError::Database)?;
        if result.rows_affected() == 0 {
            let row = sqlx::query("SELECT event_id, status FROM show_episode_write_claim WHERE show_storage_id = ? AND season_number = ? AND episode_number = ?")
                .bind(storage_id).bind(season_number).bind(episode_number)
                .fetch_one(&mut *tx).await.map_err(EventError::Database)?;
            let event_id: Option<i64> = row.try_get("event_id").map_err(EventError::Database)?;
            let status: String = row
                .try_get("status")
                .or_else(|_| {
                    row.try_get::<Vec<u8>, _>("status").and_then(|bytes| {
                        String::from_utf8(bytes)
                            .map_err(|error| sqlx::Error::Decode(Box::new(error)))
                    })
                })
                .map_err(EventError::Database)?;
            blocked.push(serde_json::json!({"episodeNumber":episode_number,"eventId":event_id,"status":status}));
        }
    }
    if !blocked.is_empty() {
        sqlx::query(
            "DELETE FROM show_episode_write_claim WHERE event_id = ? AND status = 'reserved'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(EventError::Database)?;
        let timestamp = if mysql {
            "CURRENT_TIMESTAMP(3)"
        } else {
            "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
        };
        sqlx::query(&format!("UPDATE update_event SET status='skipped', completed_at={timestamp}, updated_at={timestamp} WHERE id=? AND status='pending'"))
            .bind(id).execute(&mut *tx).await.map_err(EventError::Database)?;
    }
    tx.commit().await.map_err(EventError::Database)?;
    Ok((get(pool, id).await?, blocked))
}

pub async fn transition(
    pool: &AnyPool,
    id: i64,
    input: TransitionEvent,
    mysql: bool,
) -> Result<Event, EventError> {
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let current_row = sqlx::query(&format!(
        "{SELECT_EVENT} WHERE e.id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(EventError::Database)?;
    let current = current_row
        .as_ref()
        .map(parse_event)
        .transpose()?
        .ok_or(EventError::NotFound)?;
    if current.kind == "new_ep" {
        return Err(EventError::Conflict("followup_state_machine_required"));
    }
    let allowed = match current.status.as_str() {
        "pending" => ["processing", "skipped", "failed"].contains(&input.status.as_str()),
        "processing" => ["processed", "skipped", "failed"].contains(&input.status.as_str()),
        _ => false,
    };
    if !allowed {
        return Err(EventError::Conflict("invalid_event_transition"));
    }
    if input.status == "failed" && input.error.as_deref().unwrap_or("").trim().is_empty() {
        return Err(EventError::Invalid("event_error_required"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    let statement = format!(
        "UPDATE update_event SET status=?, attempt_count=attempt_count+CASE WHEN ?='processing' THEN 1 ELSE 0 END, started_at=CASE WHEN ?='processing' THEN {timestamp} ELSE started_at END, completed_at=CASE WHEN ? IN ('processed','skipped','failed') THEN {timestamp} ELSE NULL END, last_error=?, updated_at={timestamp} WHERE id=? AND status=?"
    );
    let result = sqlx::query(&statement)
        .bind(&input.status)
        .bind(&input.status)
        .bind(&input.status)
        .bind(&input.status)
        .bind(if input.status == "failed" {
            input.error.as_deref()
        } else {
            None
        })
        .bind(id)
        .bind(&current.status)
        .execute(&mut *tx)
        .await
        .map_err(EventError::Database)?;
    if result.rows_affected() != 1 {
        return Err(EventError::Conflict("event_changed_concurrently"));
    }
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, id).await
}

pub async fn retry(pool: &AnyPool, id: i64, mysql: bool) -> Result<Event, EventError> {
    let mut tx = pool.begin().await.map_err(EventError::Database)?;
    let row = sqlx::query(&format!(
        "{SELECT_EVENT} WHERE e.id = ?{}",
        if mysql { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(EventError::Database)?;
    let current = row
        .as_ref()
        .map(parse_event)
        .transpose()?
        .ok_or(EventError::NotFound)?;
    if current.kind == "new_ep" {
        return Err(EventError::Conflict("followup_recovery_required"));
    }
    if current.status != "failed" || !current.retryable {
        return Err(EventError::Conflict("event_not_retryable"));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    let result = sqlx::query(&format!("UPDATE update_event SET status='pending', last_error=NULL, started_at=NULL, completed_at=NULL, updated_at={timestamp} WHERE id=? AND status='failed'")).bind(id).execute(&mut *tx).await.map_err(EventError::Database)?;
    if result.rows_affected() != 1 {
        return Err(EventError::Conflict("event_changed_concurrently"));
    }
    tx.commit().await.map_err(EventError::Database)?;
    get(pool, id).await
}
