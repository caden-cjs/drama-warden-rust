use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::{AnyPool, Row};

fn show_columns(mysql: bool) -> &'static str {
    if mysql {
        "SELECT id, CAST(name AS BINARY) AS name, CAST(media_type AS BINARY) AS media_type, CAST(category AS BINARY) AS category, \
         CAST(finished AS SIGNED) AS finished, CAST(aliases_json AS BINARY) AS aliases_json, \
         CAST(created_at AS BINARY) AS created_at, CAST(updated_at AS BINARY) AS updated_at FROM shows"
    } else {
        "SELECT id, CAST(name AS BLOB) AS name, CAST(media_type AS BLOB) AS media_type, CAST(category AS BLOB) AS category, \
         CAST(finished AS INTEGER) AS finished, CAST(aliases_json AS BLOB) AS aliases_json, \
         CAST(created_at AS BLOB) AS created_at, CAST(updated_at AS BLOB) AS updated_at FROM shows"
    }
}

#[derive(Debug)]
pub enum ShowError {
    Invalid(&'static str),
    NotFound,
    Conflict,
    Database(sqlx::Error),
}

impl IntoResponse for ShowError {
    fn into_response(self) -> Response {
        let (status, code, error) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid show request"),
            Self::NotFound => (StatusCode::NOT_FOUND, "show_not_found", "show not found"),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "metadata_identity_conflict",
                "metadata identity is already linked",
            ),
            Self::Database(error) => {
                tracing::error!(error = %error, "show database operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "request failed",
                )
            }
        };
        (
            status,
            Json(serde_json::json!({ "error": error, "code": code })),
        )
            .into_response()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateShow {
    pub name: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn default_media_type() -> String {
    "tv".to_owned()
}

fn default_category() -> String {
    "tv".to_owned()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub id: i64,
    pub name: String,
    pub media_type: String,
    pub category: String,
    pub finished: bool,
    pub aliases: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataIdentity {
    pub id: i64,
    pub show_id: i64,
    pub provider_id: String,
    pub external_id: String,
    pub media_type: String,
    pub linked_by: String,
    pub linked_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkIdentity {
    pub provider_id: String,
    pub external_id: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    #[serde(default = "default_linked_by")]
    pub linked_by: String,
}

fn default_linked_by() -> String {
    "manual".to_owned()
}

fn text_column(row: &sqlx::any::AnyRow, column: &str) -> Result<String, ShowError> {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(column).map_err(ShowError::Database)?;
    String::from_utf8(bytes)
        .map_err(|error| ShowError::Database(sqlx::Error::Decode(Box::new(error))))
}

fn parse_show(row: &sqlx::any::AnyRow) -> Result<Show, ShowError> {
    let aliases_json = text_column(row, "aliases_json")?;
    let aliases = serde_json::from_str::<Vec<String>>(&aliases_json).unwrap_or_default();
    Ok(Show {
        id: row.try_get("id").map_err(ShowError::Database)?,
        name: text_column(row, "name")?,
        media_type: text_column(row, "media_type")?,
        category: text_column(row, "category")?,
        finished: row
            .try_get::<i64, _>("finished")
            .map_err(ShowError::Database)?
            != 0,
        aliases,
        created_at: text_column(row, "created_at")?,
        updated_at: text_column(row, "updated_at")?,
    })
}

fn parse_identity(row: &sqlx::any::AnyRow) -> Result<MetadataIdentity, ShowError> {
    Ok(MetadataIdentity {
        id: row.try_get("id").map_err(ShowError::Database)?,
        show_id: row.try_get("show_id").map_err(ShowError::Database)?,
        provider_id: text_column(row, "provider_id")?,
        external_id: text_column(row, "external_id")?,
        media_type: text_column(row, "media_type")?,
        linked_by: text_column(row, "linked_by")?,
        linked_at: text_column(row, "linked_at")?,
    })
}

pub async fn list(pool: &AnyPool, mysql: bool) -> Result<Vec<Show>, ShowError> {
    let rows = sqlx::query(&format!("{} ORDER BY LOWER(name), id", show_columns(mysql)))
        .fetch_all(pool)
        .await
        .map_err(ShowError::Database)?;
    rows.iter().map(parse_show).collect()
}

pub async fn get(pool: &AnyPool, id: i64, mysql: bool) -> Result<Option<Show>, ShowError> {
    let row = sqlx::query(&format!("{} WHERE id = ?", show_columns(mysql)))
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(ShowError::Database)?;
    row.as_ref().map(parse_show).transpose()
}

pub async fn create(pool: &AnyPool, input: CreateShow, mysql: bool) -> Result<Show, ShowError> {
    let name = input.name.trim();
    let media_type = input.media_type.trim();
    let category = input.category.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(ShowError::Invalid("invalid_show_name"));
    }
    if media_type.is_empty() || media_type.chars().count() > 16 {
        return Err(ShowError::Invalid("invalid_media_type"));
    }
    if category.is_empty() || category.chars().count() > 16 {
        return Err(ShowError::Invalid("invalid_show_category"));
    }
    let aliases = input
        .aliases
        .into_iter()
        .map(|alias| alias.trim().to_owned())
        .filter(|alias| !alias.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    let aliases =
        serde_json::to_string(&aliases).map_err(|_| ShowError::Invalid("invalid_aliases"))?;
    let mut transaction = pool.begin().await.map_err(ShowError::Database)?;
    sqlx::query("INSERT INTO shows (name, media_type, category, finished, aliases_json) VALUES (?, ?, ?, ?, ?)")
        .bind(name)
        .bind(media_type)
        .bind(category)
        .bind(i64::from(input.finished))
        .bind(aliases)
        .execute(&mut *transaction)
        .await
        .map_err(ShowError::Database)?;
    let id: i64 = if mysql {
        sqlx::query_scalar("SELECT LAST_INSERT_ID()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(ShowError::Database)?
    } else {
        sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(ShowError::Database)?
    };
    let row = sqlx::query(&format!("{} WHERE id = ?", show_columns(mysql)))
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(ShowError::Database)?;
    transaction.commit().await.map_err(ShowError::Database)?;
    parse_show(&row)
}

pub async fn list_identities(
    pool: &AnyPool,
    show_id: i64,
    mysql: bool,
) -> Result<Vec<MetadataIdentity>, ShowError> {
    if get(pool, show_id, mysql).await?.is_none() {
        return Err(ShowError::NotFound);
    }
    let text_type = if mysql { "BINARY" } else { "BLOB" };
    let rows = sqlx::query(
        &format!("SELECT id, show_id, CAST(provider_id AS {text_type}) AS provider_id, CAST(external_id AS {text_type}) AS external_id, \
         CAST(media_type AS {text_type}) AS media_type, CAST(linked_by AS {text_type}) AS linked_by, CAST(linked_at AS {text_type}) AS linked_at \
         FROM show_metadata_identity WHERE show_id = ? ORDER BY provider_id"),
    ).bind(show_id).fetch_all(pool).await.map_err(ShowError::Database)?;
    rows.iter().map(parse_identity).collect()
}

pub async fn link_identity(
    pool: &AnyPool,
    show_id: i64,
    input: LinkIdentity,
    mysql: bool,
) -> Result<MetadataIdentity, ShowError> {
    let provider_id = input.provider_id.trim();
    let external_id = input.external_id.trim();
    let media_type = input.media_type.trim();
    let linked_by = input.linked_by.trim();
    if provider_id.is_empty() || provider_id.chars().count() > 64 {
        return Err(ShowError::Invalid("invalid_provider_id"));
    }
    if external_id.is_empty() || external_id.chars().count() > 255 {
        return Err(ShowError::Invalid("invalid_external_id"));
    }
    if media_type.is_empty() || media_type.chars().count() > 16 {
        return Err(ShowError::Invalid("invalid_media_type"));
    }
    if linked_by.is_empty() || linked_by.chars().count() > 64 {
        return Err(ShowError::Invalid("invalid_linked_by"));
    }
    let mut tx = pool.begin().await.map_err(ShowError::Database)?;
    let show_exists: Option<i64> = sqlx::query_scalar("SELECT id FROM shows WHERE id = ?")
        .bind(show_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ShowError::Database)?;
    if show_exists.is_none() {
        return Err(ShowError::NotFound);
    }
    let current = sqlx::query(
        "SELECT external_id FROM show_metadata_identity WHERE show_id = ? AND provider_id = ?",
    )
    .bind(show_id)
    .bind(provider_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(ShowError::Database)?;
    if let Some(row) = current {
        let linked_external_id = text_column(&row, "external_id")?;
        if linked_external_id != external_id {
            return Err(ShowError::Conflict);
        }
    } else {
        let owner: Option<i64> = sqlx::query_scalar(
            "SELECT show_id FROM show_metadata_identity WHERE provider_id = ? AND external_id = ?",
        )
        .bind(provider_id)
        .bind(external_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ShowError::Database)?;
        if owner.is_some_and(|owner_id| owner_id != show_id) {
            return Err(ShowError::Conflict);
        }
        sqlx::query(
            "INSERT INTO show_metadata_identity (show_id, provider_id, external_id, media_type, linked_by) \
             VALUES (?, ?, ?, ?, ?)",
        ).bind(show_id).bind(provider_id).bind(external_id).bind(media_type).bind(linked_by)
            .execute(&mut *tx).await.map_err(ShowError::Database)?;
    }
    let text_type = if mysql { "BINARY" } else { "BLOB" };
    let row = sqlx::query(&format!(
        "SELECT id, show_id, CAST(provider_id AS {text_type}) AS provider_id, CAST(external_id AS {text_type}) AS external_id, \
         CAST(media_type AS {text_type}) AS media_type, CAST(linked_by AS {text_type}) AS linked_by, CAST(linked_at AS {text_type}) AS linked_at \
         FROM show_metadata_identity WHERE show_id = ? AND provider_id = ?"
    )).bind(show_id).bind(provider_id).fetch_one(&mut *tx).await.map_err(ShowError::Database)?;
    tx.commit().await.map_err(ShowError::Database)?;
    parse_identity(&row)
}
