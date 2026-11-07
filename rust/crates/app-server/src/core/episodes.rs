use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{AnyPool, Row};

#[derive(Debug)]
pub enum EpisodeError {
    Invalid(&'static str),
    NotFound(&'static str),
    Unavailable,
    Database(sqlx::Error),
}

impl IntoResponse for EpisodeError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid storage request"),
            Self::NotFound(code) => (StatusCode::NOT_FOUND, code, "storage record not found"),
            Self::Unavailable => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "storage_provider_unavailable",
                "storage provider is not enabled",
            ),
            Self::Database(error) => {
                tracing::error!(error = %error, "episode catalog operation failed");
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateStorage {
    pub provider_id: String,
    pub root_path: String,
    pub local_path: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    pub id: i64,
    pub show_id: i64,
    pub provider_id: String,
    pub root_path: String,
    pub local_path: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageEpisode {
    pub season_number: i64,
    pub episode_number: i64,
    pub status: String,
    pub asset_count: i64,
    pub scanned_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ScannedAsset {
    pub season_number: i64,
    pub episode_number: i64,
    pub asset_key: String,
    pub relative_path: String,
    pub file_name: String,
    pub file_size: Option<i64>,
    pub file_mtime: Option<i64>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetSyncResult {
    pub visible_asset_count: usize,
    pub marked_absent: u64,
    pub complete_snapshot: bool,
}

fn text(row: &sqlx::any::AnyRow, column: &str) -> Result<String, EpisodeError> {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(column).map_err(EpisodeError::Database)?;
    String::from_utf8(bytes)
        .map_err(|error| EpisodeError::Database(sqlx::Error::Decode(Box::new(error))))
}

fn optional_text(row: &sqlx::any::AnyRow, column: &str) -> Result<Option<String>, EpisodeError> {
    match row.try_get::<Option<String>, _>(column) {
        Ok(value) => Ok(value),
        Err(_) => match row.try_get::<Option<Vec<u8>>, _>(column) {
            Ok(Some(bytes)) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| EpisodeError::Database(sqlx::Error::Decode(Box::new(error)))),
            Ok(None) => Ok(None),
            Err(error) => Err(EpisodeError::Database(error)),
        },
    }
}

fn parse_storage(row: &sqlx::any::AnyRow) -> Result<Storage, EpisodeError> {
    Ok(Storage {
        id: row.try_get("id").map_err(EpisodeError::Database)?,
        show_id: row.try_get("show_id").map_err(EpisodeError::Database)?,
        provider_id: text(row, "provider_id")?,
        root_path: text(row, "root_path")?,
        local_path: optional_text(row, "local_path")?,
        enabled: row
            .try_get::<i64, _>("enabled")
            .map_err(EpisodeError::Database)?
            != 0,
        created_at: text(row, "created_at")?,
        updated_at: text(row, "updated_at")?,
    })
}

fn storage_select(mysql: bool) -> &'static str {
    if mysql {
        "SELECT id, show_id, CAST(provider_id AS BINARY) AS provider_id, CAST(root_path AS BINARY) AS root_path, CAST(local_path AS BINARY) AS local_path, CAST(enabled AS SIGNED) AS enabled, CAST(created_at AS CHAR) AS created_at, CAST(updated_at AS CHAR) AS updated_at FROM show_storage"
    } else {
        "SELECT id, show_id, CAST(provider_id AS BLOB) AS provider_id, CAST(root_path AS BLOB) AS root_path, CAST(local_path AS BLOB) AS local_path, CAST(enabled AS INTEGER) AS enabled, CAST(created_at AS BLOB) AS created_at, CAST(updated_at AS BLOB) AS updated_at FROM show_storage"
    }
}

pub async fn list_storages(
    pool: &AnyPool,
    show_id: i64,
    mysql: bool,
) -> Result<Vec<Storage>, EpisodeError> {
    let rows = sqlx::query(&format!(
        "{} WHERE show_id = ? ORDER BY id",
        storage_select(mysql)
    ))
    .bind(show_id)
    .fetch_all(pool)
    .await
    .map_err(EpisodeError::Database)?;
    rows.iter().map(parse_storage).collect()
}

pub async fn get_storage(
    pool: &AnyPool,
    show_id: i64,
    storage_id: i64,
    mysql: bool,
) -> Result<Storage, EpisodeError> {
    let row = sqlx::query(&format!(
        "{} WHERE show_id = ? AND id = ?",
        storage_select(mysql)
    ))
    .bind(show_id)
    .bind(storage_id)
    .fetch_optional(pool)
    .await
    .map_err(EpisodeError::Database)?;
    row.as_ref()
        .map(parse_storage)
        .transpose()?
        .ok_or(EpisodeError::NotFound("storage_not_found"))
}

pub async fn create_storage(
    pool: &AnyPool,
    show_id: i64,
    input: CreateStorage,
    mysql: bool,
) -> Result<Storage, EpisodeError> {
    let provider_id = input.provider_id.trim();
    let mut root_path = input.root_path.trim().to_owned();
    let local_path = input
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if show_id <= 0 {
        return Err(EpisodeError::Invalid("invalid_show_id"));
    }
    if provider_id.len() < 2
        || provider_id.len() > 64
        || !provider_id.as_bytes()[0].is_ascii_lowercase()
        || !provider_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(EpisodeError::Invalid("invalid_provider_id"));
    }
    if root_path.is_empty() || root_path.len() > 600 || root_path.contains('\0') {
        return Err(EpisodeError::Invalid("invalid_root_path"));
    }
    if provider_id == "local-storage" {
        let segments = root_path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if !root_path.starts_with('/')
            || root_path.contains('\\')
            || segments.is_empty()
            || segments.iter().any(|part| *part == "." || *part == "..")
        {
            return Err(EpisodeError::Invalid("invalid_root_path"));
        }
        root_path = format!("/{}", segments.join("/"));
    }
    if local_path.is_some_and(|path| path.len() > 1024 || path.contains('\0')) {
        return Err(EpisodeError::Invalid("invalid_local_path"));
    }
    let upsert = if mysql {
        "INSERT INTO show_storage (show_id, provider_id, root_path, local_path, enabled) VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE local_path = COALESCE(VALUES(local_path), show_storage.local_path), enabled = VALUES(enabled), updated_at = CURRENT_TIMESTAMP(3)"
    } else {
        "INSERT INTO show_storage (show_id, provider_id, root_path, local_path, enabled) VALUES (?, ?, ?, ?, ?) ON CONFLICT(show_id, provider_id, root_path) DO UPDATE SET local_path = COALESCE(excluded.local_path, show_storage.local_path), enabled = excluded.enabled, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
    sqlx::query(upsert)
        .bind(show_id)
        .bind(provider_id)
        .bind(&root_path)
        .bind(local_path)
        .bind(i64::from(input.enabled != Some(false)))
        .execute(pool)
        .await
        .map_err(EpisodeError::Database)?;
    let row = sqlx::query(&format!(
        "{} WHERE show_id = ? AND provider_id = ? AND root_path = ?",
        storage_select(mysql)
    ))
    .bind(show_id)
    .bind(provider_id)
    .bind(root_path)
    .fetch_one(pool)
    .await
    .map_err(EpisodeError::Database)?;
    parse_storage(&row)
}

pub async fn list_storage_episodes(
    pool: &AnyPool,
    storage_id: i64,
    mysql: bool,
) -> Result<Vec<StorageEpisode>, EpisodeError> {
    let scanned_expr = if mysql {
        "CAST(es.scanned_at AS CHAR)"
    } else {
        "CAST(es.scanned_at AS BLOB)"
    };
    let query = format!(
        "SELECT s.season_number, e.episode_number, CAST(es.status AS BINARY) AS status, es.asset_count, {scanned_expr} AS scanned_at FROM show_episode_storage es JOIN show_episode e ON e.id = es.episode_id JOIN show_season s ON s.id = e.season_id WHERE es.show_storage_id = ? ORDER BY s.season_number, e.episode_number"
    );
    let rows = sqlx::query(&query)
        .bind(storage_id)
        .fetch_all(pool)
        .await
        .map_err(EpisodeError::Database)?;
    rows.iter()
        .map(|row| {
            Ok(StorageEpisode {
                season_number: row
                    .try_get("season_number")
                    .map_err(EpisodeError::Database)?,
                episode_number: row
                    .try_get("episode_number")
                    .map_err(EpisodeError::Database)?,
                status: text(row, "status")?,
                asset_count: row.try_get("asset_count").map_err(EpisodeError::Database)?,
                scanned_at: optional_text(row, "scanned_at")?,
            })
        })
        .collect()
}

pub async fn sync_scanned_assets(
    pool: &AnyPool,
    show_id: i64,
    storage_id: i64,
    scan_id: &str,
    assets: &[ScannedAsset],
    complete_snapshot: bool,
    mysql: bool,
) -> Result<AssetSyncResult, EpisodeError> {
    if show_id <= 0 || storage_id <= 0 || scan_id.is_empty() || scan_id.len() > 64 {
        return Err(EpisodeError::Invalid("invalid_storage_scan"));
    }
    let mut tx = pool.begin().await.map_err(EpisodeError::Database)?;
    let storage = sqlx::query("SELECT show_id FROM show_storage WHERE id = ?")
        .bind(storage_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(EpisodeError::Database)?
        .ok_or(EpisodeError::NotFound("storage_not_found"))?;
    let actual_show_id: i64 = storage.try_get("show_id").map_err(EpisodeError::Database)?;
    if actual_show_id != show_id {
        return Err(EpisodeError::NotFound("storage_not_found"));
    }
    let season_upsert = if mysql {
        "INSERT INTO show_season (show_id, season_number) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id"
    } else {
        "INSERT INTO show_season (show_id, season_number) VALUES (?, ?) ON CONFLICT(show_id, season_number) DO NOTHING"
    };
    let episode_upsert = if mysql {
        "INSERT INTO show_episode (season_id, episode_number) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id"
    } else {
        "INSERT INTO show_episode (season_id, episode_number) VALUES (?, ?) ON CONFLICT(season_id, episode_number) DO NOTHING"
    };
    let asset_upsert = if mysql {
        "INSERT INTO show_episode_asset (episode_id, show_storage_id, asset_key, relative_path, file_name, file_size, file_mtime, quality, is_present, scan_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?) ON DUPLICATE KEY UPDATE episode_id = VALUES(episode_id), relative_path = VALUES(relative_path), file_name = VALUES(file_name), file_size = VALUES(file_size), file_mtime = VALUES(file_mtime), quality = VALUES(quality), is_present = 1, scan_id = VALUES(scan_id), last_seen_at = CURRENT_TIMESTAMP(3)"
    } else {
        "INSERT INTO show_episode_asset (episode_id, show_storage_id, asset_key, relative_path, file_name, file_size, file_mtime, quality, is_present, scan_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?) ON CONFLICT(show_storage_id, asset_key) DO UPDATE SET episode_id = excluded.episode_id, relative_path = excluded.relative_path, file_name = excluded.file_name, file_size = excluded.file_size, file_mtime = excluded.file_mtime, quality = excluded.quality, is_present = 1, scan_id = excluded.scan_id, last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
    let mut seen = std::collections::HashSet::new();
    for asset in assets {
        if asset.season_number < 0
            || asset.episode_number <= 0
            || asset.asset_key.is_empty()
            || asset.asset_key.len() > 700
            || asset.relative_path.is_empty()
            || asset.relative_path.len() > 2048
            || asset.file_name.is_empty()
            || asset.file_name.len() > 1024
            || asset
                .quality
                .as_ref()
                .is_some_and(|quality| quality.len() > 64)
            || asset.file_size.is_some_and(|size| size < 0)
            || asset.file_mtime.is_some_and(|mtime| mtime < 0)
            || !seen.insert(asset.asset_key.as_str())
        {
            return Err(EpisodeError::Invalid("invalid_storage_asset"));
        }
        sqlx::query(season_upsert)
            .bind(show_id)
            .bind(asset.season_number)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
        let season: i64 = sqlx::query_scalar(
            "SELECT id FROM show_season WHERE show_id = ? AND season_number = ?",
        )
        .bind(show_id)
        .bind(asset.season_number)
        .fetch_one(&mut *tx)
        .await
        .map_err(EpisodeError::Database)?;
        sqlx::query(episode_upsert)
            .bind(season)
            .bind(asset.episode_number)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
        let episode: i64 = sqlx::query_scalar(
            "SELECT id FROM show_episode WHERE season_id = ? AND episode_number = ?",
        )
        .bind(season)
        .bind(asset.episode_number)
        .fetch_one(&mut *tx)
        .await
        .map_err(EpisodeError::Database)?;
        sqlx::query(asset_upsert)
            .bind(episode)
            .bind(storage_id)
            .bind(&asset.asset_key)
            .bind(&asset.relative_path)
            .bind(&asset.file_name)
            .bind(asset.file_size)
            .bind(asset.file_mtime)
            .bind(&asset.quality)
            .bind(scan_id)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
    }
    let marked_absent = if complete_snapshot {
        sqlx::query("UPDATE show_episode_asset SET is_present = 0 WHERE show_storage_id = ? AND is_present = 1 AND scan_id <> ?")
            .bind(storage_id)
            .bind(scan_id)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?
            .rows_affected()
    } else {
        0
    };
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
    let state_upsert = if mysql {
        format!(
            "INSERT INTO show_episode_storage (episode_id, show_storage_id, status, asset_count, scanned_at) SELECT e.id, ?, CASE WHEN COUNT(a.id) = 0 THEN 'missing' WHEN COUNT(a.id) = 1 THEN 'present' ELSE 'conflict' END, COUNT(a.id), {timestamp} FROM show_episode e JOIN show_season s ON s.id = e.season_id LEFT JOIN show_episode_asset a ON a.episode_id = e.id AND a.show_storage_id = ? AND a.is_present = 1 WHERE s.show_id = ? GROUP BY e.id ON DUPLICATE KEY UPDATE status = VALUES(status), asset_count = VALUES(asset_count), scanned_at = VALUES(scanned_at)"
        )
    } else {
        format!(
            "INSERT INTO show_episode_storage (episode_id, show_storage_id, status, asset_count, scanned_at) SELECT e.id, ?, CASE WHEN COUNT(a.id) = 0 THEN 'missing' WHEN COUNT(a.id) = 1 THEN 'present' ELSE 'conflict' END, COUNT(a.id), {timestamp} FROM show_episode e JOIN show_season s ON s.id = e.season_id LEFT JOIN show_episode_asset a ON a.episode_id = e.id AND a.show_storage_id = ? AND a.is_present = 1 WHERE s.show_id = ? GROUP BY e.id ON CONFLICT(episode_id, show_storage_id) DO UPDATE SET status = excluded.status, asset_count = excluded.asset_count, scanned_at = excluded.scanned_at"
        )
    };
    sqlx::query(&state_upsert)
        .bind(storage_id)
        .bind(storage_id)
        .bind(show_id)
        .execute(&mut *tx)
        .await
        .map_err(EpisodeError::Database)?;
    tx.commit().await.map_err(EpisodeError::Database)?;
    Ok(AssetSyncResult {
        visible_asset_count: assets.len(),
        marked_absent,
        complete_snapshot,
    })
}

fn optional_field(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn episode_date(item: &Value, key: &str) -> Option<String> {
    let value = optional_field(item, key)?;
    let date = value.get(..10)?;
    let bytes = date.as_bytes();
    (bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit()))
    .then(|| date.to_owned())
}

/// Persist one provider's normalized show manifest as a single catalog update.
pub async fn sync_metadata_manifest(
    pool: &AnyPool,
    show_id: i64,
    provider_id: &str,
    external_id: &str,
    detail: &Value,
    season_number: i64,
    episodes: &[Value],
    mysql: bool,
) -> Result<usize, EpisodeError> {
    if show_id <= 0 || season_number <= 0 || provider_id.is_empty() || external_id.is_empty() {
        return Err(EpisodeError::Invalid("invalid_metadata_manifest"));
    }
    let mut seasons = detail
        .get("seasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !seasons
        .iter()
        .any(|season| season.get("seasonNumber").and_then(Value::as_i64) == Some(season_number))
    {
        seasons.push(serde_json::json!({"seasonNumber": season_number}));
    }
    let timestamp = if mysql {
        "CURRENT_TIMESTAMP(3)"
    } else {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
    let season_upsert = if mysql {
        format!(
            "INSERT INTO show_season (show_id, season_number, name, episode_count, first_air_date, metadata_provider_id, metadata_synced_at) VALUES (?, ?, ?, ?, ?, ?, {timestamp}) ON DUPLICATE KEY UPDATE name = COALESCE(VALUES(name), show_season.name), episode_count = COALESCE(VALUES(episode_count), show_season.episode_count), first_air_date = COALESCE(VALUES(first_air_date), show_season.first_air_date), metadata_provider_id = VALUES(metadata_provider_id), metadata_synced_at = {timestamp}, updated_at = {timestamp}"
        )
    } else {
        format!(
            "INSERT INTO show_season (show_id, season_number, name, episode_count, first_air_date, metadata_provider_id, metadata_synced_at) VALUES (?, ?, ?, ?, ?, ?, {timestamp}) ON CONFLICT(show_id, season_number) DO UPDATE SET name = COALESCE(excluded.name, show_season.name), episode_count = COALESCE(excluded.episode_count, show_season.episode_count), first_air_date = COALESCE(excluded.first_air_date, show_season.first_air_date), metadata_provider_id = excluded.metadata_provider_id, metadata_synced_at = {timestamp}, updated_at = {timestamp}"
        )
    };
    let mut tx = pool.begin().await.map_err(EpisodeError::Database)?;
    for season in &seasons {
        let Some(number) = season
            .get("seasonNumber")
            .and_then(Value::as_i64)
            .filter(|number| *number >= 0)
        else {
            continue;
        };
        let name = optional_field(season, "name");
        let count = season
            .get("episodeCount")
            .and_then(Value::as_i64)
            .filter(|count| *count >= 0);
        let air_date = episode_date(season, "airDate");
        sqlx::query(&season_upsert)
            .bind(show_id)
            .bind(number)
            .bind(name)
            .bind(count)
            .bind(air_date)
            .bind(provider_id)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
    }

    let mut persisted = 0;
    for episode in episodes {
        let Some(number) = episode
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .filter(|number| *number > 0)
        else {
            continue;
        };
        let item_season = episode
            .get("seasonNumber")
            .and_then(Value::as_i64)
            .filter(|number| *number >= 0)
            .unwrap_or(season_number);
        if !seasons
            .iter()
            .any(|season| season.get("seasonNumber").and_then(Value::as_i64) == Some(item_season))
        {
            let fallback = serde_json::json!({"seasonNumber": item_season});
            sqlx::query(&season_upsert)
                .bind(show_id)
                .bind(item_season)
                .bind(None::<String>)
                .bind(None::<i64>)
                .bind(None::<String>)
                .bind(provider_id)
                .execute(&mut *tx)
                .await
                .map_err(EpisodeError::Database)?;
            seasons.push(fallback);
        }
        let season_row =
            sqlx::query("SELECT id FROM show_season WHERE show_id = ? AND season_number = ?")
                .bind(show_id)
                .bind(item_season)
                .fetch_one(&mut *tx)
                .await
                .map_err(EpisodeError::Database)?;
        let season_id: i64 = season_row.try_get("id").map_err(EpisodeError::Database)?;
        let episode_insert = if mysql {
            "INSERT INTO show_episode (season_id, episode_number) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id"
        } else {
            "INSERT INTO show_episode (season_id, episode_number) VALUES (?, ?) ON CONFLICT(season_id, episode_number) DO NOTHING"
        };
        sqlx::query(episode_insert)
            .bind(season_id)
            .bind(number)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
        let episode_row =
            sqlx::query("SELECT id FROM show_episode WHERE season_id = ? AND episode_number = ?")
                .bind(season_id)
                .bind(number)
                .fetch_one(&mut *tx)
                .await
                .map_err(EpisodeError::Database)?;
        let episode_id: i64 = episode_row.try_get("id").map_err(EpisodeError::Database)?;
        let provider_episode_id =
            episode
                .get("externalId")
                .or_else(|| episode.get("id"))
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                });
        let metadata_json = serde_json::to_string(episode)
            .map_err(|_| EpisodeError::Invalid("invalid_metadata_manifest"))?;
        let metadata_upsert = if mysql {
            format!(
                "INSERT INTO show_episode_metadata (episode_id, provider_id, provider_external_id, provider_episode_id, name, overview, air_date, air_time, runtime, still_path, metadata_json, synced_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {timestamp}) ON DUPLICATE KEY UPDATE provider_external_id = VALUES(provider_external_id), provider_episode_id = VALUES(provider_episode_id), name = VALUES(name), overview = VALUES(overview), air_date = VALUES(air_date), air_time = VALUES(air_time), runtime = VALUES(runtime), still_path = VALUES(still_path), metadata_json = VALUES(metadata_json), synced_at = {timestamp}"
            )
        } else {
            format!(
                "INSERT INTO show_episode_metadata (episode_id, provider_id, provider_external_id, provider_episode_id, name, overview, air_date, air_time, runtime, still_path, metadata_json, synced_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {timestamp}) ON CONFLICT(episode_id, provider_id) DO UPDATE SET provider_external_id = excluded.provider_external_id, provider_episode_id = excluded.provider_episode_id, name = excluded.name, overview = excluded.overview, air_date = excluded.air_date, air_time = excluded.air_time, runtime = excluded.runtime, still_path = excluded.still_path, metadata_json = excluded.metadata_json, synced_at = {timestamp}"
            )
        };
        sqlx::query(&metadata_upsert)
            .bind(episode_id)
            .bind(provider_id)
            .bind(external_id)
            .bind(provider_episode_id)
            .bind(optional_field(episode, "name"))
            .bind(optional_field(episode, "overview"))
            .bind(episode_date(episode, "airDate"))
            .bind(optional_field(episode, "airTime"))
            .bind(
                episode
                    .get("runtime")
                    .and_then(Value::as_i64)
                    .filter(|runtime| *runtime > 0),
            )
            .bind(optional_field(episode, "stillPath"))
            .bind(metadata_json)
            .execute(&mut *tx)
            .await
            .map_err(EpisodeError::Database)?;
        persisted += 1;
    }
    tx.commit().await.map_err(EpisodeError::Database)?;
    Ok(persisted)
}
