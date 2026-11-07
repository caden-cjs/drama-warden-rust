use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::{AnyPool, Row};

#[derive(Debug)]
pub enum PluginCallError {
    Invalid(&'static str),
    Database(sqlx::Error),
}

impl IntoResponse for PluginCallError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid plugin call filter"),
            Self::Database(error) => {
                tracing::error!(error = %error, "plugin call query failed");
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
pub struct PluginCall {
    pub id: i64,
    pub plugin_id: String,
    pub capability: String,
    pub source: String,
    pub correlation_id: Option<String>,
    pub ok: bool,
    pub error_code: Option<String>,
    pub duration_ms: i64,
    pub called_at: String,
}

#[derive(Default)]
pub struct Filters<'a> {
    pub plugin_id: Option<&'a str>,
    pub capability: Option<&'a str>,
    pub source: Option<&'a str>,
    pub correlation_id: Option<&'a str>,
    pub ok: Option<bool>,
    pub limit: i64,
    pub offset: i64,
}

fn text(row: &sqlx::any::AnyRow, key: &str) -> Result<String, PluginCallError> {
    if let Ok(value) = row.try_get::<String, _>(key) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(key).map_err(PluginCallError::Database)?;
    String::from_utf8(bytes)
        .map_err(|error| PluginCallError::Database(sqlx::Error::Decode(Box::new(error))))
}

fn optional_text(row: &sqlx::any::AnyRow, key: &str) -> Result<Option<String>, PluginCallError> {
    if let Ok(value) = row.try_get::<Option<String>, _>(key) {
        return Ok(value);
    }
    match row.try_get::<Option<Vec<u8>>, _>(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|error| PluginCallError::Database(sqlx::Error::Decode(Box::new(error)))),
        Ok(None) => Ok(None),
        Err(error) => Err(PluginCallError::Database(error)),
    }
}

pub async fn list(
    pool: &AnyPool,
    filters: Filters<'_>,
) -> Result<Vec<PluginCall>, PluginCallError> {
    let limit = if filters.limit == 0 {
        50
    } else {
        filters.limit
    };
    if limit < 1 || limit > 100 {
        return Err(PluginCallError::Invalid("invalid_limit"));
    }
    if filters.offset < 0 {
        return Err(PluginCallError::Invalid("invalid_offset"));
    }
    if filters.offset > 1_000_000 {
        return Err(PluginCallError::Invalid("invalid_offset"));
    }
    for (value, max, code) in [
        (filters.plugin_id, 64, "invalid_plugin_id_filter"),
        (filters.capability, 64, "invalid_capability_filter"),
        (filters.source, 128, "invalid_source_filter"),
        (filters.correlation_id, 255, "invalid_correlation_id_filter"),
    ] {
        if value.is_some_and(|item| item.chars().count() > max) {
            return Err(PluginCallError::Invalid(code));
        }
    }
    let mut clauses = Vec::new();
    if filters.plugin_id.is_some() {
        clauses.push("plugin_id = ?");
    }
    if filters.capability.is_some() {
        clauses.push("capability = ?");
    }
    if filters.source.is_some() {
        clauses.push("source = ?");
    }
    if filters.correlation_id.is_some() {
        clauses.push("correlation_id = ?");
    }
    if filters.ok.is_some() {
        clauses.push("ok = ?");
    }
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let query = format!(
        "SELECT id, CAST(plugin_id AS CHAR) AS plugin_id, CAST(capability AS CHAR) AS capability, CAST(source AS CHAR) AS source, CAST(correlation_id AS CHAR) AS correlation_id, CAST(ok AS SIGNED) AS ok, CAST(error_code AS CHAR) AS error_code, CAST(duration_ms AS SIGNED) AS duration_ms, CAST(called_at AS CHAR) AS called_at FROM plugin_call_log{where_clause} ORDER BY called_at DESC, id DESC LIMIT ? OFFSET ?"
    );
    let mut request = sqlx::query(&query);
    if let Some(value) = filters.plugin_id {
        request = request.bind(value);
    }
    if let Some(value) = filters.capability {
        request = request.bind(value);
    }
    if let Some(value) = filters.source {
        request = request.bind(value);
    }
    if let Some(value) = filters.correlation_id {
        request = request.bind(value);
    }
    if let Some(value) = filters.ok {
        request = request.bind(i64::from(value));
    }
    let rows = request
        .bind(limit)
        .bind(filters.offset)
        .fetch_all(pool)
        .await
        .map_err(PluginCallError::Database)?;
    rows.iter()
        .map(|row| {
            Ok(PluginCall {
                id: row.try_get("id").map_err(PluginCallError::Database)?,
                plugin_id: text(row, "plugin_id")?,
                capability: text(row, "capability")?,
                source: text(row, "source")?,
                correlation_id: optional_text(row, "correlation_id")?,
                ok: row
                    .try_get::<i64, _>("ok")
                    .map_err(PluginCallError::Database)?
                    != 0,
                error_code: optional_text(row, "error_code")?,
                duration_ms: row
                    .try_get("duration_ms")
                    .map_err(PluginCallError::Database)?,
                called_at: text(row, "called_at")?,
            })
        })
        .collect()
}
