use std::{collections::HashSet, env, net::SocketAddr, time::Duration};

use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tower_http::services::ServeDir;
use tracing::{error, info};

mod config;
mod core;
mod db;
mod metadata_plugins;
mod plugins;
mod portable_transfer;

#[derive(Clone)]
struct AppState {
    database: db::Database,
    api_token: String,
    plugins: plugins::PluginHost,
    runtime_updates: watch::Sender<core::runtime_settings::RuntimeSettings>,
    automation_status: std::sync::Arc<Mutex<AutomationStatus>>,
    subtitle_worker_status: std::sync::Arc<Mutex<core::subtitle_worker::WorkerStatus>>,
}

#[derive(Default)]
struct AutomationStatus {
    running: bool,
    last_run: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    database: &'static str,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
    code: &'static str,
}

async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.database.pool)
        .await
    {
        Ok(_) => axum::Json(HealthResponse {
            ok: true,
            database: state.database.driver,
        })
        .into_response(),
        Err(error) => {
            error!(error = %error, "database health query failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(HealthResponse {
                    ok: false,
                    database: "unavailable",
                }),
            )
                .into_response()
        }
    }
}

async fn auth(
    headers: HeaderMap,
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(value) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return unauthorized();
    };
    let Some(candidate) = value.strip_prefix("Bearer ") else {
        return unauthorized();
    };
    let matches = bool::from(candidate.as_bytes().ct_eq(state.api_token.as_bytes()));
    if !matches {
        return unauthorized();
    }
    next.run(request).await
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(ApiError {
            error: "authentication required",
            code: "unauthorized",
        }),
    )
        .into_response()
}

async fn list_shows(State(state): State<AppState>) -> Result<Response, core::shows::ShowError> {
    let items = core::shows::list(&state.database.pool, state.database.driver == "mysql").await?;
    Ok(axum::Json(serde_json::json!({ "items": items })).into_response())
}

async fn create_show(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<core::shows::CreateShow>,
) -> Result<Response, core::shows::ShowError> {
    let show = core::shows::create(
        &state.database.pool,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(serde_json::json!({ "show": show })),
    )
        .into_response())
}

async fn get_show(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
) -> Result<Response, core::shows::ShowError> {
    if show_id <= 0 {
        return Err(core::shows::ShowError::Invalid("invalid_show_id"));
    }
    let Some(show) = core::shows::get(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await?
    else {
        return Err(core::shows::ShowError::NotFound);
    };
    let metadata_identities = core::shows::list_identities(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(
        axum::Json(serde_json::json!({ "show": show, "metadataIdentities": metadata_identities }))
            .into_response(),
    )
}

async fn list_metadata_identities(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
) -> Result<Response, core::shows::ShowError> {
    if show_id <= 0 {
        return Err(core::shows::ShowError::Invalid("invalid_show_id"));
    }
    let items = core::shows::list_identities(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(axum::Json(serde_json::json!({ "items": items })).into_response())
}

async fn link_metadata_identity(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<core::shows::LinkIdentity>,
) -> Result<Response, core::shows::ShowError> {
    if show_id <= 0 {
        return Err(core::shows::ShowError::Invalid("invalid_show_id"));
    }
    let identity = core::shows::link_identity(
        &state.database.pool,
        show_id,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(serde_json::json!({ "identity": identity })),
    )
        .into_response())
}

async fn list_show_storages(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
) -> Result<Response, core::episodes::EpisodeError> {
    if show_id <= 0 {
        return Err(core::episodes::EpisodeError::Invalid("invalid_show_id"));
    }
    if core::shows::get(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await
    .map_err(|error| match error {
        core::shows::ShowError::Database(db_error) => {
            core::episodes::EpisodeError::Database(db_error)
        }
        _ => core::episodes::EpisodeError::NotFound("show_not_found"),
    })?
    .is_none()
    {
        return Err(core::episodes::EpisodeError::NotFound("show_not_found"));
    }
    let items = core::episodes::list_storages(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(axum::Json(serde_json::json!({"items": items})).into_response())
}

async fn create_show_storage(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<core::episodes::CreateStorage>,
) -> Result<Response, core::episodes::EpisodeError> {
    if show_id <= 0 {
        return Err(core::episodes::EpisodeError::Invalid("invalid_show_id"));
    }
    if core::shows::get(
        &state.database.pool,
        show_id,
        state.database.driver == "mysql",
    )
    .await
    .map_err(|error| match error {
        core::shows::ShowError::Database(db_error) => {
            core::episodes::EpisodeError::Database(db_error)
        }
        _ => core::episodes::EpisodeError::NotFound("show_not_found"),
    })?
    .is_none()
    {
        return Err(core::episodes::EpisodeError::NotFound("show_not_found"));
    }
    let provider_id = input.provider_id.trim();
    let available = state.plugins.list().await.into_iter().any(|plugin| {
        plugin.id == provider_id
            && plugin.enabled
            && plugin.kind == "storage-provider"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "listSharedFiles")
    });
    if !available {
        return Err(core::episodes::EpisodeError::Unavailable);
    }
    let storage = core::episodes::create_storage(
        &state.database.pool,
        show_id,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(serde_json::json!({"storage": storage})),
    )
        .into_response())
}

async fn list_storage_episodes(
    State(state): State<AppState>,
    axum::extract::Path((show_id, storage_id)): axum::extract::Path<(i64, i64)>,
) -> Result<Response, core::episodes::EpisodeError> {
    if show_id <= 0 {
        return Err(core::episodes::EpisodeError::Invalid("invalid_show_id"));
    }
    if storage_id <= 0 {
        return Err(core::episodes::EpisodeError::Invalid("invalid_storage_id"));
    }
    let _storage = core::episodes::get_storage(
        &state.database.pool,
        show_id,
        storage_id,
        state.database.driver == "mysql",
    )
    .await?;
    let items = core::episodes::list_storage_episodes(
        &state.database.pool,
        storage_id,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(axum::Json(serde_json::json!({"items": items})).into_response())
}

fn normalized_storage_path(value: &str) -> Option<String> {
    if value.contains('\0') || value.contains('\\') {
        return None;
    }
    let absolute = value.starts_with('/');
    let mut segments = Vec::new();
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return None;
        }
        segments.push(segment);
    }
    Some(format!(
        "{}{}",
        if absolute { "/" } else { "" },
        segments.join("/")
    ))
}

fn relative_storage_path(root: &str, file_path: &str) -> Option<String> {
    let root = normalized_storage_path(root)?;
    let file_path = normalized_storage_path(file_path)?;
    if file_path.is_empty() || file_path == root {
        return None;
    }
    if root.is_empty() {
        return Some(file_path.trim_start_matches('/').to_owned());
    }
    let prefix = if root == "/" {
        "/".to_owned()
    } else {
        format!("{root}/")
    };
    file_path
        .strip_prefix(&prefix)
        .filter(|relative| !relative.is_empty())
        .map(str::to_owned)
}

fn season_from_parent_path(relative_path: &str) -> Option<i64> {
    relative_path.split('/').rev().skip(1).find_map(|segment| {
        let lower = segment.to_ascii_lowercase();
        let suffix = lower.strip_prefix("season")?;
        let digits = suffix.trim_start_matches([' ', '.', '_', '-']);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    })
}

fn scan_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("scan-{}-{timestamp}-{sequence}", std::process::id())
}

async fn scan_storage(
    State(state): State<AppState>,
    axum::extract::Path((show_id, storage_id)): axum::extract::Path<(i64, i64)>,
) -> Response {
    if show_id <= 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_show_id",
            "invalid show id",
        );
    }
    if storage_id <= 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_storage_id",
            "invalid storage id",
        );
    }
    let mysql = state.database.driver == "mysql";
    let storage =
        match core::episodes::get_storage(&state.database.pool, show_id, storage_id, mysql).await {
            Ok(storage) => storage,
            Err(error) => return error.into_response(),
        };
    if !storage.enabled {
        return api_error(
            StatusCode::CONFLICT,
            "storage_disabled",
            "storage is disabled",
        );
    }
    let available = state.plugins.list().await.into_iter().any(|plugin| {
        plugin.id == storage.provider_id
            && plugin.enabled
            && plugin.kind == "storage-provider"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "list")
    });
    if !available {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "storage_provider_unavailable",
            "storage provider is not enabled",
        );
    }
    let scan_id = scan_id();
    let listing = match state.plugins.invoke(
        &storage.provider_id,
        "list",
        serde_json::json!({"path":storage.root_path,"options":{"recursive":true}}),
        "storage-catalog-scan",
        Some(scan_id.clone()),
    ).await {
        Ok(listing) => listing,
        Err(error) => return (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error":"storage listing failed","code":error.code,"providerId":storage.provider_id}))).into_response(),
    };
    let Some(files) = listing.get("files").and_then(serde_json::Value::as_array) else {
        return api_error(
            StatusCode::BAD_GATEWAY,
            "invalid_storage_listing",
            "storage provider returned an invalid listing",
        );
    };
    let Some(truncated) = listing
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
    else {
        return api_error(
            StatusCode::BAD_GATEWAY,
            "invalid_storage_listing",
            "storage provider omitted listing completeness",
        );
    };
    let mut assets = Vec::new();
    let mut ignored_file_count = 0_usize;
    for file in files {
        if file.get("isDir").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        let file_path = file
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let file_name = file
            .get("name")
            .or_else(|| file.get("fileName"))
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .or_else(|| file_path.rsplit('/').next().filter(|name| !name.is_empty()));
        let Some(file_name) = file_name else {
            ignored_file_count += 1;
            continue;
        };
        let Some((parsed_season, episode_number, quality)) =
            core::followup_plan::parse_video_episode(file_name)
        else {
            ignored_file_count += 1;
            continue;
        };
        let Some(relative_path) = relative_storage_path(&storage.root_path, file_path) else {
            return api_error(
                StatusCode::BAD_GATEWAY,
                "invalid_storage_path",
                "storage provider returned a file outside the configured root",
            );
        };
        let season_number = core::followup_plan::explicit_video_season_number(file_name)
            .or_else(|| season_from_parent_path(&relative_path))
            .unwrap_or(parsed_season);
        if season_number < 0 || episode_number <= 0 {
            ignored_file_count += 1;
            continue;
        }
        let file_size = file
            .get("size")
            .and_then(serde_json::Value::as_i64)
            .or_else(|| {
                file.get("size")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| i64::try_from(value).ok())
            })
            .filter(|value| *value >= 0);
        let file_mtime = file
            .get("mtime")
            .and_then(serde_json::Value::as_i64)
            .or_else(|| {
                file.get("mtime")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| i64::try_from(value).ok())
            })
            .or_else(|| {
                file.get("mtime")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<i64>().ok())
            })
            .filter(|value| *value >= 0);
        assets.push(core::episodes::ScannedAsset {
            season_number,
            episode_number,
            asset_key: relative_path.clone(),
            relative_path,
            file_name: file_name.to_owned(),
            file_size,
            file_mtime,
            quality,
        });
    }
    match core::episodes::sync_scanned_assets(
        &state.database.pool,
        show_id,
        storage_id,
        &scan_id,
        &assets,
        !truncated,
        mysql,
    )
    .await
    {
        Ok(result) => axum::Json(serde_json::json!({
            "showId":show_id,
            "storageId":storage_id,
            "scanId":scan_id,
            "listedEntryCount":files.len(),
            "ignoredFileCount":ignored_file_count,
            "truncated":truncated,
            "completeSnapshot":!truncated,
            "visibleAssetCount":result.visible_asset_count,
            "markedAbsent":result.marked_absent,
        }))
        .into_response(),
        Err(error) => error.into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventListQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginCallQuery {
    plugin_id: Option<String>,
    capability: Option<String>,
    source: Option<String>,
    correlation_id: Option<String>,
    ok: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataSearchQuery {
    q: Option<String>,
    media_type: Option<String>,
    limit: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FollowupPreviewRequest {
    storage_id: i64,
    share_url: String,
    share_title: Option<String>,
    source: Option<String>,
    correlation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceSearchRequest {
    storage_id: i64,
    query: Option<String>,
    limit: Option<i64>,
}

fn china_today() -> String {
    let days = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + 8 * 60 * 60)
        .div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

async fn preview_followup(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<FollowupPreviewRequest>,
) -> Response {
    if show_id <= 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_show_id",
            "invalid show id",
        );
    }
    if input.storage_id <= 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_storage_id",
            "invalid storage id",
        );
    }
    let share_url = input.share_url.trim();
    if share_url.is_empty() || share_url.len() > 4096 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_share_url",
            "share URL is required",
        );
    }
    let mysql = state.database.driver == "mysql";
    let show = match core::shows::get(&state.database.pool, show_id, mysql).await {
        Ok(Some(show)) => show,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "show_not_found", "show not found"),
        Err(_) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "request failed",
            );
        }
    };
    let storage =
        match core::episodes::get_storage(&state.database.pool, show_id, input.storage_id, mysql)
            .await
        {
            Ok(storage) => storage,
            Err(error) => return error.into_response(),
        };
    if !storage.enabled {
        return api_error(
            StatusCode::CONFLICT,
            "storage_disabled",
            "storage is disabled",
        );
    }
    let storage_plugin = state.plugins.list().await.into_iter().find(|plugin| {
        plugin.id == storage.provider_id
            && plugin.enabled
            && plugin.kind == "storage-provider"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "listSharedFiles")
    });
    if storage_plugin.is_none() {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "storage_provider_unavailable",
            "storage provider is not enabled",
        );
    }
    let stored = match core::episodes::list_storage_episodes(
        &state.database.pool,
        storage.id,
        mysql,
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return error.into_response(),
    };
    let latest = stored
        .iter()
        .filter(|item| {
            item.status == "present" || item.status == "conflict" || item.asset_count > 0
        })
        .max_by_key(|item| (item.season_number, item.episode_number));
    let listing = match state.plugins.invoke(&storage.provider_id, "listSharedFiles", serde_json::json!({"shareUrl":share_url}), "followup-preview", None).await {
        Ok(listing) => listing,
        Err(error) => return (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error":"share listing failed","code":error.code,"providerId":storage.provider_id}))).into_response(),
    };
    let files = listing
        .get("files")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let base = serde_json::json!({
        "show":{"id":show.id,"name":show.name},
        "storage":{"id":storage.id,"providerId":storage.provider_id,"rootPath":storage.root_path},
        "shareFileCount":files.len(),
    });
    if listing
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
    {
        let mut response = base;
        response["eligible"] = serde_json::json!(false);
        response["reason"] = serde_json::json!("share_listing_incomplete");
        response["metadata"] = serde_json::Value::Null;
        response["plan"] = serde_json::Value::Null;
        return axum::Json(response).into_response();
    }
    let manifest = core::metadata::resolve_episode_manifest(
        &state.database.pool,
        &state.plugins,
        &show,
        latest.map(|item| item.season_number).unwrap_or(0),
        latest.map(|item| item.episode_number).unwrap_or(0),
        &files,
        mysql,
    )
    .await;
    let metadata_summary = serde_json::json!({
        "providerId":manifest.provider_id,
        "seasonNumber":manifest.season_number,
        "maxEpisode":manifest.max_episode,
        "sufficient":manifest.sufficient,
        "requiredEpisode":manifest.required_episode,
        "coverageGapEpisode":manifest.coverage_gap_episode,
        "failures":manifest.failures,
    });
    let Some(season_number) = manifest.season_number else {
        let mut response = base;
        response["eligible"] = serde_json::json!(false);
        response["reason"] = serde_json::json!("metadata_unavailable");
        response["metadata"] = metadata_summary;
        response["plan"] = serde_json::Value::Null;
        return axum::Json(response).into_response();
    };
    if !manifest.sufficient {
        let mut response = base;
        response["eligible"] = serde_json::json!(false);
        response["reason"] = serde_json::json!("metadata_coverage_insufficient");
        response["metadata"] = metadata_summary;
        response["plan"] = serde_json::Value::Null;
        return axum::Json(response).into_response();
    }
    let stored_json = stored
        .iter()
        .map(|episode| {
            serde_json::json!({
                "seasonNumber":episode.season_number,"episodeNumber":episode.episode_number,
                "status":episode.status,"assetCount":episode.asset_count,
            })
        })
        .collect::<Vec<_>>();
    let Some(plan) = core::followup_plan::create_plan(
        season_number,
        &manifest.episodes,
        &stored_json,
        &files,
        &china_today(),
        3,
        8 * 1024 * 1024 * 1024,
    ) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "followup_plan_failed",
            "could not create follow-up plan",
        );
    };
    let eligible = !plan.save_batch.is_empty();
    let mut response = base;
    response["eligible"] = serde_json::json!(eligible);
    response["reason"] = if eligible {
        serde_json::Value::Null
    } else {
        serde_json::json!("no_confirmed_missing_episodes")
    };
    response["metadata"] = metadata_summary;
    response["plan"] = serde_json::to_value(plan).unwrap_or(serde_json::Value::Null);
    axum::Json(response).into_response()
}

async fn queue_followup(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<FollowupPreviewRequest>,
) -> Response {
    let source = if input.source.as_deref() == Some("automation") {
        "automation"
    } else {
        "manual"
    };
    let correlation_id = input
        .correlation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("followup-{}-{timestamp}-{sequence}", std::process::id())
        });
    if correlation_id.len() > 128 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_correlation_id",
            "correlation id is too long",
        );
    }
    let preview_response = preview_followup(
        State(state.clone()),
        axum::extract::Path(show_id),
        axum::Json(input.clone()),
    )
    .await;
    let preview_status = preview_response.status();
    if !preview_status.is_success() {
        return preview_response;
    }
    let preview_bytes =
        match axum::body::to_bytes(preview_response.into_body(), 2 * 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "preview_read_failed",
                    "could not read preview result",
                );
            }
        };
    let preview: serde_json::Value = match serde_json::from_slice(&preview_bytes) {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "preview_parse_failed",
                "could not parse preview result",
            );
        }
    };
    if preview.get("eligible").and_then(serde_json::Value::as_bool) != Some(true) {
        return axum::Json(serde_json::json!({
            "queued":false,
            "reason":preview.get("reason").cloned().unwrap_or(serde_json::Value::Null),
            "preview":preview,
        }))
        .into_response();
    }
    let storage_id = preview
        .pointer("/storage/id")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let provider_id = preview
        .pointer("/storage/providerId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let season_number = preview
        .pointer("/plan/seasonNumber")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let estimated_bytes = preview
        .pointer("/plan/estimatedBytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let selected_files = preview
        .pointer("/plan/saveBatch")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let queued = core::events::queue_followup(
        &state.database.pool,
        show_id,
        storage_id,
        provider_id,
        input.share_url.trim(),
        input
            .share_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        season_number,
        &selected_files,
        estimated_bytes,
        source,
        &correlation_id,
        state.database.driver == "mysql",
    )
    .await;
    match queued {
        Ok((event, blocked)) if blocked.is_empty() => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({"queued":true,"event":event,"preview":preview})),
        )
            .into_response(),
        Ok((event, blocked)) => axum::Json(serde_json::json!({
            "queued":false,
            "reason":"episode_write_claimed",
            "blocked":blocked,
            "event":event,
            "preview":preview,
        }))
        .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn search_resources(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<ResourceSearchRequest>,
) -> Response {
    if show_id <= 0 || input.storage_id <= 0 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_show_or_storage_id",
            "invalid show or storage id",
        );
    }
    let mysql = state.database.driver == "mysql";
    let show = match core::shows::get(&state.database.pool, show_id, mysql).await {
        Ok(Some(show)) => show,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "show_not_found", "show not found"),
        Err(_) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "request failed",
            );
        }
    };
    if show.media_type != "tv" {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_media_type",
            "resource discovery supports TV shows only",
        );
    }
    let storage =
        match core::episodes::get_storage(&state.database.pool, show_id, input.storage_id, mysql)
            .await
        {
            Ok(storage) => storage,
            Err(error) => return error.into_response(),
        };
    if !storage.enabled {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "storage_disabled",
            "storage is disabled",
        );
    }
    let limit = input.limit.unwrap_or(10).clamp(1, 20) as usize;
    if input.limit.is_some_and(|value| value < 1) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_limit",
            "limit must be positive",
        );
    }
    let query = input
        .query
        .unwrap_or_else(|| show.name.clone())
        .trim()
        .to_owned();
    if query.is_empty() || query.len() > 200 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "query must contain 1 to 200 characters",
        );
    }
    let stored = match core::episodes::list_storage_episodes(
        &state.database.pool,
        storage.id,
        mysql,
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return error.into_response(),
    };
    let latest = stored
        .iter()
        .filter(|item| {
            item.status == "present" || item.status == "conflict" || item.asset_count > 0
        })
        .max_by_key(|item| (item.season_number, item.episode_number));
    let correlation_id = format!(
        "resource-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let providers = state
        .plugins
        .list()
        .await
        .into_iter()
        .filter(|plugin| {
            plugin.enabled
                && plugin.kind == "resource-provider"
                && plugin.capabilities.iter().any(|cap| cap == "search")
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    let mut provider_results = Vec::new();
    let mut seen = HashSet::new();
    for provider in &providers {
        let remaining = limit.saturating_sub(results.len());
        if remaining == 0 {
            break;
        }
        let response = state
            .plugins
            .invoke(
                &provider.id,
                "search",
                serde_json::json!({
                    "query":query,
                    "aliases":show.aliases,
                    "mediaType":"tv",
                    "storageProvider":storage.provider_id,
                    "seasonNumber":latest.map(|item| item.season_number),
                    "episodeNumber":latest.map(|item| item.episode_number).unwrap_or(0),
                    "limit":remaining,
                }),
                "resource-discovery",
                Some(correlation_id.clone()),
            )
            .await;
        let response = match response {
            Ok(value) => value,
            Err(error) => {
                provider_results.push(serde_json::json!({"providerId":provider.id,"ok":false,"resultCount":0,"errorCode":error.code}));
                continue;
            }
        };
        let items = response
            .as_array()
            .or_else(|| response.get("items").and_then(serde_json::Value::as_array));
        let Some(items) = items else {
            provider_results.push(serde_json::json!({"providerId":provider.id,"ok":false,"resultCount":0,"errorCode":"RESOURCE_RESPONSE_INVALID"}));
            continue;
        };
        let truncated = !response.is_array()
            && response
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
        let mut accepted = 0;
        for item in items {
            if results.len() >= limit {
                break;
            }
            if !item.is_object() {
                continue;
            }
            let share_url = item
                .get("shareUrl")
                .or_else(|| item.get("url"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if share_url.is_empty()
                || share_url.len() > 4096
                || share_url.chars().any(char::is_control)
                || !seen.insert(share_url.to_owned())
            {
                continue;
            }
            let title = item
                .get("title")
                .or_else(|| item.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(|value| value.trim().chars().take(500).collect::<String>())
                .filter(|value| !value.is_empty());
            let source_id = item
                .get("sourceId")
                .or_else(|| item.get("id"))
                .and_then(|value| match value {
                    serde_json::Value::String(value) => Some(value.clone()),
                    serde_json::Value::Number(value) => Some(value.to_string()),
                    serde_json::Value::Bool(value) => Some(value.to_string()),
                    _ => None,
                })
                .map(|value| value.chars().take(255).collect::<String>());
            let preview_response = preview_followup(
                State(state.clone()),
                axum::extract::Path(show_id),
                axum::Json(FollowupPreviewRequest {
                    storage_id: storage.id,
                    share_url: share_url.to_owned(),
                    share_title: title.clone(),
                    source: Some("resource-discovery".to_owned()),
                    correlation_id: Some(correlation_id.clone()),
                }),
            )
            .await;
            let preview_status = preview_response.status();
            let preview_value = match axum::body::to_bytes(preview_response.into_body(), 2 * 1024 * 1024).await {
                Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_else(|_| serde_json::json!({"eligible":false,"reason":"preview_failed","errorCode":"PREVIEW_RESPONSE_INVALID"})),
                Err(_) => serde_json::json!({"eligible":false,"reason":"preview_failed","errorCode":"PREVIEW_RESPONSE_INVALID"}),
            };
            let preview = if preview_status.is_success() {
                preview_value
            } else {
                serde_json::json!({"eligible":false,"reason":"preview_failed","errorCode":preview_value.get("code").cloned().unwrap_or(serde_json::json!("PREVIEW_FAILED"))})
            };
            results.push(serde_json::json!({
                "providerId":provider.id,
                "shareUrl":share_url,
                "title":title,
                "sourceId":source_id,
                "preview":preview,
                "searchComplete":!truncated,
            }));
            accepted += 1;
        }
        provider_results.push(serde_json::json!({"providerId":provider.id,"ok":true,"resultCount":accepted,"truncated":truncated}));
    }
    if !providers.is_empty()
        && !provider_results.is_empty()
        && provider_results
            .iter()
            .all(|item| item.get("ok").and_then(serde_json::Value::as_bool) == Some(false))
    {
        return (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error":"all resource providers failed","code":"ALL_RESOURCE_PROVIDERS_FAILED","providers":provider_results}))).into_response();
    }
    axum::Json(serde_json::json!({
        "show":{"id":show.id,"name":show.name},
        "storage":{"id":storage.id,"providerId":storage.provider_id},
        "query":query,
        "providers":provider_results,
        "results":results,
    }))
    .into_response()
}

fn timestamp_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let second_of_day = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        (second_of_day / 60) % 60,
        second_of_day % 60
    )
}

fn event_error_code(error: &core::events::EventError) -> &'static str {
    match error {
        core::events::EventError::Invalid(code) | core::events::EventError::Conflict(code) => code,
        core::events::EventError::NotFound => "event_not_found",
        core::events::EventError::Database(_) => "database_error",
    }
}

fn show_error_code(error: &core::shows::ShowError) -> &'static str {
    match error {
        core::shows::ShowError::Invalid(code) => code,
        core::shows::ShowError::NotFound => "show_not_found",
        core::shows::ShowError::Conflict => "metadata_identity_conflict",
        core::shows::ShowError::Database(_) => "database_error",
    }
}

fn episode_error_code(error: &core::episodes::EpisodeError) -> &'static str {
    match error {
        core::episodes::EpisodeError::Invalid(code)
        | core::episodes::EpisodeError::NotFound(code) => code,
        core::episodes::EpisodeError::Unavailable => "storage_provider_unavailable",
        core::episodes::EpisodeError::Database(_) => "database_error",
    }
}

async fn response_json(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let value = match axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    };
    (status, value)
}

async fn run_automation_scan(
    state: &AppState,
    queue_candidates: bool,
    process_queued: bool,
) -> serde_json::Value {
    {
        let mut status = state.automation_status.lock().await;
        if status.running {
            return serde_json::json!({"skipped":true,"reason":"scan_in_progress"});
        }
        status.running = true;
    }
    let started_at = timestamp_utc();
    let mut shows_checked = 0usize;
    let mut storages_checked = 0usize;
    let mut candidates_found = 0usize;
    let mut eligible_candidates = 0usize;
    let mut events_queued = 0usize;
    let mut events_processed = 0usize;
    let mut events_failed = 0usize;
    let mut storage_limit_reached = false;
    let mut errors = Vec::new();
    let mut candidates = Vec::new();
    let mysql = state.database.driver == "mysql";

    if process_queued {
        match core::events::list(&state.database.pool, Some("pending"), 1000, 0).await {
            Ok(events) => {
                for event in events
                    .into_iter()
                    .filter(|event| event.kind == "new_ep" && event.source == "automation")
                {
                    match core::followup::process_event(
                        &state.database.pool,
                        &state.plugins,
                        event.id,
                        mysql,
                    )
                    .await
                    {
                        Ok(result)
                            if result.get("ok").and_then(serde_json::Value::as_bool)
                                == Some(true) =>
                        {
                            events_processed += 1
                        }
                        Ok(_) => events_failed += 1,
                        Err(error) => {
                            events_failed += 1;
                            if errors.len() < 100 {
                                errors.push(serde_json::json!({"eventId":event.id,"errorCode":event_error_code(&error)}));
                            }
                        }
                    }
                }
            }
            Err(error) => errors.push(serde_json::json!({"errorCode":event_error_code(&error)})),
        }
    }

    match core::shows::list(&state.database.pool, mysql).await {
        Ok(shows) => {
            for show in shows
                .into_iter()
                .filter(|show| show.media_type == "tv" && !show.finished)
            {
                shows_checked += 1;
                let storages = match core::episodes::list_storages(
                    &state.database.pool,
                    show.id,
                    mysql,
                )
                .await
                {
                    Ok(items) => items
                        .into_iter()
                        .filter(|storage| storage.enabled)
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        if errors.len() < 100 {
                            errors.push(serde_json::json!({"showId":show.id,"errorCode":episode_error_code(&error)}));
                        }
                        continue;
                    }
                };
                for storage in storages {
                    if storages_checked >= 200 {
                        storage_limit_reached = true;
                        break;
                    }
                    storages_checked += 1;
                    let response = search_resources(
                        State(state.clone()),
                        axum::extract::Path(show.id),
                        axum::Json(ResourceSearchRequest {
                            storage_id: storage.id,
                            query: None,
                            limit: Some(5),
                        }),
                    )
                    .await;
                    let (http_status, discovery) = response_json(response).await;
                    if !http_status.is_success() {
                        if errors.len() < 100 {
                            errors.push(serde_json::json!({"showId":show.id,"storageId":storage.id,"errorCode":discovery.get("code").cloned().unwrap_or(serde_json::json!("RESOURCE_SEARCH_FAILED"))}));
                        }
                        continue;
                    }
                    let items = discovery
                        .get("results")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    candidates_found += items.len();
                    let eligible = items
                        .iter()
                        .filter(|candidate| {
                            candidate
                                .get("searchComplete")
                                .and_then(serde_json::Value::as_bool)
                                != Some(false)
                                && candidate
                                    .pointer("/preview/eligible")
                                    .and_then(serde_json::Value::as_bool)
                                    == Some(true)
                        })
                        .collect::<Vec<_>>();
                    eligible_candidates += eligible.len();
                    for candidate in &items {
                        if candidates.len() >= 100 {
                            break;
                        }
                        candidates.push(serde_json::json!({
                        "showId":show.id,"showName":show.name,"storageId":storage.id,
                        "providerId":candidate.get("providerId"),"searchComplete":candidate.get("searchComplete"),
                        "shareUrl":candidate.get("shareUrl"),"title":candidate.get("title"),"sourceId":candidate.get("sourceId"),"preview":candidate.get("preview"),
                    }));
                    }
                    if queue_candidates {
                        if let Some(candidate) = eligible.first() {
                            let queue_response = queue_followup(
                                State(state.clone()),
                                axum::extract::Path(show.id),
                                axum::Json(FollowupPreviewRequest {
                                    storage_id: storage.id,
                                    share_url: candidate
                                        .get("shareUrl")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("")
                                        .to_owned(),
                                    share_title: candidate
                                        .get("title")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned),
                                    source: Some("automation".to_owned()),
                                    correlation_id: None,
                                }),
                            )
                            .await;
                            let (status, result) = response_json(queue_response).await;
                            if status.is_success()
                                && result.get("queued").and_then(serde_json::Value::as_bool)
                                    == Some(true)
                            {
                                events_queued += 1;
                            }
                        }
                    }
                }
                if storage_limit_reached {
                    break;
                }
            }
        }
        Err(error) => errors.push(serde_json::json!({"errorCode":show_error_code(&error)})),
    }

    if process_queued {
        if let Ok(events) = core::events::list(&state.database.pool, Some("pending"), 1000, 0).await
        {
            for event in events
                .into_iter()
                .filter(|event| event.kind == "new_ep" && event.source == "automation")
            {
                match core::followup::process_event(
                    &state.database.pool,
                    &state.plugins,
                    event.id,
                    mysql,
                )
                .await
                {
                    Ok(result)
                        if result.get("ok").and_then(serde_json::Value::as_bool) == Some(true) =>
                    {
                        events_processed += 1
                    }
                    Ok(_) => events_failed += 1,
                    Err(_) => events_failed += 1,
                }
            }
        }
    }
    let completed_at = timestamp_utc();
    let result = serde_json::json!({
        "startedAt":started_at,"completedAt":completed_at,"showsChecked":shows_checked,
        "storagesChecked":storages_checked,"candidatesFound":candidates_found,
        "eligibleCandidates":eligible_candidates,"eventsQueued":events_queued,
        "eventsProcessed":events_processed,"eventsFailed":events_failed,
        "storageLimitReached":storage_limit_reached,"errors":errors,"candidates":candidates,
    });
    let summary = serde_json::json!({
        "startedAt":started_at,"completedAt":completed_at,"showsChecked":shows_checked,
        "storagesChecked":storages_checked,"candidatesFound":candidates_found,
        "eligibleCandidates":eligible_candidates,"eventsQueued":events_queued,
        "eventsProcessed":events_processed,"eventsFailed":events_failed,
        "errorCount":errors.len(),"storageLimitReached":storage_limit_reached,
    });
    let mut status = state.automation_status.lock().await;
    status.running = false;
    status.last_run = Some(summary);
    result
}

async fn run_scheduler(
    state: AppState,
    mut updates: watch::Receiver<core::runtime_settings::RuntimeSettings>,
) {
    let mut settings = updates.borrow().clone();
    if settings.followup_automation_enabled {
        run_automation_scan(&state, true, settings.followup_auto_process).await;
    }
    loop {
        if !settings.followup_automation_enabled {
            if updates.changed().await.is_err() {
                break;
            }
        } else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs((settings.followup_scan_interval_minutes as u64) * 60)) => {
                    run_automation_scan(&state, true, settings.followup_auto_process).await;
                }
                changed = updates.changed() => if changed.is_err() { break; },
            }
        }
        let next = updates.borrow().clone();
        let enabling = !settings.followup_automation_enabled && next.followup_automation_enabled;
        settings = next;
        if enabling {
            run_automation_scan(&state, true, settings.followup_auto_process).await;
        }
    }
}

async fn search_metadata(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<MetadataSearchQuery>,
) -> Response {
    let query_text = query.q.unwrap_or_default().trim().to_owned();
    if query_text.is_empty() || query_text.chars().count() > 200 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "q is required and must be 200 characters or fewer",
        );
    }
    let media_type = query.media_type.as_deref().unwrap_or("tv");
    if !["tv", "movie"].contains(&media_type) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_media_type",
            "mediaType must be tv or movie",
        );
    }
    let requested_limit = match query.limit.as_deref() {
        None => 10,
        Some(value) => match value.parse::<i64>() {
            Ok(value) if value > 0 => value.min(50),
            _ => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_limit",
                    "limit must be a positive integer",
                );
            }
        },
    };
    let plugins = state.plugins.list().await;
    let providers: Vec<_> = plugins
        .into_iter()
        .filter(|plugin| {
            plugin.enabled
                && plugin.kind == "metadata-provider"
                && plugin
                    .capabilities
                    .iter()
                    .any(|capability| capability == "search")
        })
        .collect();
    let mut results = Vec::new();
    let mut provider_results = Vec::new();
    let mut errors = Vec::new();
    for provider in providers {
        match state.plugins.invoke(
            &provider.id,
            "search",
            serde_json::json!({ "query": query_text, "mediaType": media_type, "limit": requested_limit }),
            "http-api",
            None,
        ).await {
            Ok(serde_json::Value::Array(items)) => {
                let items: Vec<_> = items.into_iter().filter_map(|mut item| {
                    let object = item.as_object_mut()?;
                    object.insert("providerId".to_owned(), serde_json::json!(provider.id));
                    object.insert("providerLabel".to_owned(), serde_json::json!(provider.label));
                    Some(item)
                }).take(requested_limit as usize).collect();
                provider_results.push(serde_json::json!({"providerId": provider.id, "providerLabel": provider.label, "ok": true, "count": items.len()}));
                results.extend(items);
            }
            Ok(_) => {
                let code = "PLUGIN_RESPONSE_INVALID";
                provider_results.push(serde_json::json!({"providerId": provider.id, "providerLabel": provider.label, "ok": false, "code": code}));
                errors.push(serde_json::json!({"providerId": provider.id, "code": code, "message": "Metadata provider request failed"}));
            }
            Err(failure) => {
                let code = failure.code;
                provider_results.push(serde_json::json!({"providerId": provider.id, "providerLabel": provider.label, "ok": false, "code": code}));
                errors.push(serde_json::json!({"providerId": provider.id, "code": code, "message": "Metadata provider request failed"}));
            }
        }
    }
    if !provider_results.is_empty()
        && provider_results
            .iter()
            .all(|provider| provider["ok"] == false)
    {
        return (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({
                "error": "all metadata providers failed",
                "code": "metadata_providers_failed",
                "providers": errors.iter().map(|failure| serde_json::json!({
                    "providerId": failure["providerId"],
                    "errorCode": failure["code"],
                })).collect::<Vec<_>>(),
            })),
        )
            .into_response();
    }
    axum::Json(serde_json::json!({
        "query": query_text,
        "mediaType": media_type,
        "providers": provider_results,
        "results": results,
        "errors": errors,
    }))
    .into_response()
}

fn api_error(status: StatusCode, code: &'static str, error: &'static str) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "error": error, "code": code })),
    )
        .into_response()
}

async fn list_plugin_calls(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<PluginCallQuery>,
) -> Result<Response, core::plugin_calls::PluginCallError> {
    let ok = match query.ok.as_deref() {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => {
            return Err(core::plugin_calls::PluginCallError::Invalid(
                "invalid_ok_filter",
            ));
        }
    };
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    let items = core::plugin_calls::list(
        &state.database.pool,
        core::plugin_calls::Filters {
            plugin_id: query.plugin_id.as_deref(),
            capability: query.capability.as_deref(),
            source: query.source.as_deref(),
            correlation_id: query.correlation_id.as_deref(),
            ok,
            limit,
            offset,
        },
    )
    .await?;
    Ok(
        axum::Json(serde_json::json!({ "items": items, "limit": limit, "offset": offset }))
            .into_response(),
    )
}

async fn list_events(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<EventListQuery>,
) -> Result<Response, core::events::EventError> {
    let items = core::events::list(
        &state.database.pool,
        query.status.as_deref(),
        query.limit.unwrap_or(100),
        query.offset.unwrap_or(0),
    )
    .await?;
    Ok(axum::Json(serde_json::json!({ "items": items })).into_response())
}

async fn create_event(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<core::events::CreateEvent>,
) -> Result<Response, core::events::EventError> {
    let event = core::events::create(
        &state.database.pool,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(serde_json::json!({ "event": event })),
    )
        .into_response())
}

async fn transition_event(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<core::events::TransitionEvent>,
) -> Result<Response, core::events::EventError> {
    let event = core::events::transition(
        &state.database.pool,
        id,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    if matches!(event.status.as_str(), "processed" | "skipped" | "failed") {
        let notification = core::events::notification_event(&event);
        let _ = state.plugins.publish_event(notification).await;
    }
    Ok(axum::Json(serde_json::json!({ "event": event })).into_response())
}

async fn retry_event(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Response, core::events::EventError> {
    let event =
        core::events::retry(&state.database.pool, id, state.database.driver == "mysql").await?;
    Ok(axum::Json(serde_json::json!({ "event": event })).into_response())
}

async fn process_followup_event(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Response, core::events::EventError> {
    let result = core::followup::process_event(
        &state.database.pool,
        &state.plugins,
        id,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(axum::Json(result).into_response())
}

async fn recover_followup_event(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<core::followup::RecoverRequest>,
) -> Result<Response, core::events::EventError> {
    let result = core::followup::recover_event(
        &state.database.pool,
        &state.plugins,
        id,
        input,
        state.database.driver == "mysql",
    )
    .await?;
    Ok(axum::Json(result).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleJobsQuery {
    status: Option<String>,
    show_id: Option<i64>,
    limit: Option<i64>,
}

async fn subtitle_status(State(state): State<AppState>) -> Response {
    let worker = state.subtitle_worker_status.lock().await.clone();
    let providers = state
        .plugins
        .list()
        .await
        .into_iter()
        .filter(|plugin| plugin.enabled && plugin.kind == "subtitle-provider")
        .map(|plugin| serde_json::json!({"id":plugin.id,"label":plugin.label}))
        .collect::<Vec<_>>();
    axum::Json(serde_json::json!({"worker":worker,"providers":providers})).into_response()
}

async fn list_subtitle_jobs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<SubtitleJobsQuery>,
) -> Result<Response, core::subtitles::SubtitleError> {
    let status = query.status.filter(|status| !status.is_empty());
    let items = core::subtitles::list(
        &state.database.pool,
        status.as_deref(),
        query.show_id,
        query.limit.unwrap_or(100),
    )
    .await?;
    Ok(axum::Json(serde_json::json!({"items":items})).into_response())
}

async fn queue_subtitle_job(
    State(state): State<AppState>,
    axum::extract::Path(show_id): axum::extract::Path<i64>,
    axum::Json(input): axum::Json<core::subtitles::QueueRequest>,
) -> Result<Response, core::subtitles::SubtitleError> {
    let (job, queued) = core::subtitles::queue(
        &state.database.pool,
        state.database.driver == "mysql",
        show_id,
        input,
        &state.plugins,
    )
    .await?;
    Ok((
        if queued {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        axum::Json(serde_json::json!({"queued":queued,"job":job})),
    )
        .into_response())
}

async fn get_subtitle_job(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Response, core::subtitles::SubtitleError> {
    let Some(job) = core::subtitles::get(&state.database.pool, id).await? else {
        return Err(core::subtitles::SubtitleError::NotFound(
            "subtitle_job_not_found",
        ));
    };
    Ok(axum::Json(serde_json::json!({"job":job})).into_response())
}

async fn retry_subtitle_job(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Response, core::subtitles::SubtitleError> {
    let job =
        core::subtitles::retry(&state.database.pool, id, state.database.driver == "mysql").await?;
    Ok(axum::Json(serde_json::json!({"job":job})).into_response())
}

async fn get_runtime_settings(State(state): State<AppState>) -> Response {
    match core::runtime_settings::get(&state.database.pool, state.database.driver == "mysql").await
    {
        Ok(response) => axum::Json(response).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn update_runtime_settings(
    State(state): State<AppState>,
    axum::Json(input): axum::Json<serde_json::Value>,
) -> Response {
    match core::runtime_settings::update(
        &state.database.pool,
        input,
        state.database.driver == "mysql",
    )
    .await
    {
        Ok(response) => {
            state
                .runtime_updates
                .send_replace(response.settings.clone());
            axum::Json(response).into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn reset_runtime_settings(State(state): State<AppState>) -> Response {
    match core::runtime_settings::reset(&state.database.pool).await {
        Ok(response) => {
            state
                .runtime_updates
                .send_replace(response.settings.clone());
            axum::Json(response).into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn automation_status(State(state): State<AppState>) -> Response {
    let settings =
        match core::runtime_settings::get(&state.database.pool, state.database.driver == "mysql")
            .await
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let status = state.automation_status.lock().await;
    axum::Json(serde_json::json!({
        "settings":settings.settings,
        "defaults":settings.defaults,
        "source":settings.source,
        "running":status.running,
        "lastRun":status.last_run,
    }))
    .into_response()
}

async fn automation_scan(State(state): State<AppState>) -> Response {
    let result = run_automation_scan(&state, false, false).await;
    axum::Json(result).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginEnableRequest {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginMoveRequest {
    direction: String,
}

async fn list_plugins(State(state): State<AppState>) -> Response {
    axum::Json(serde_json::json!({ "items": state.plugins.list().await })).into_response()
}

async fn reset_plugin(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Response, plugins::PluginFailure> {
    state.plugins.reset(&id).await?;
    Ok(axum::Json(serde_json::json!({ "pluginId": id, "ok": true })).into_response())
}

async fn monitor_plugin(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Response, plugins::PluginFailure> {
    let result = state.plugins.monitor(&id).await?;
    Ok(axum::Json(serde_json::json!({ "result": result })).into_response())
}

async fn set_plugin_enabled(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(input): axum::Json<PluginEnableRequest>,
) -> Result<Response, plugins::PluginFailure> {
    state.plugins.set_enabled(&id, input.enabled).await?;
    Ok(axum::Json(serde_json::json!({ "items": state.plugins.list().await })).into_response())
}

async fn move_plugin(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(input): axum::Json<PluginMoveRequest>,
) -> Result<Response, plugins::PluginFailure> {
    let items = state.plugins.move_plugin(&id, &input.direction).await?;
    Ok(axum::Json(serde_json::json!({ "items": items })).into_response())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--plugin")
    {
        let provider_id = arguments.get(1).ok_or("--plugin requires a provider id")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(metadata_plugins::serve(provider_id));
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "--import-database")
    {
        if arguments.len() != 2 {
            return Err("--import-database requires exactly one .backup path".into());
        }
        if std::path::Path::new(&arguments[1])
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("backup")
        {
            return Err("database exports must use the .backup suffix".into());
        }
        config::load()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(async {
            let database = db::connect().await?;
            let imported = portable_transfer::import_file(
                &database.pool,
                database.driver,
                std::path::Path::new(&arguments[1]),
            )
            .await
            .map_err(std::io::Error::other)?;
            let row_total = imported.values().sum::<usize>();
            let table_counts = imported
                .iter()
                .map(|(table, count)| format!("{table}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Database import verified ({row_total} rows; {table_counts})");
            database.pool.close().await;
            Ok(())
        });
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "--export-database")
    {
        if arguments.len() != 2 {
            return Err("--export-database requires exactly one .backup path".into());
        }
        if std::path::Path::new(&arguments[1])
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("backup")
        {
            return Err("database exports must use the .backup suffix".into());
        }
        config::load()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(async {
            let database = db::connect().await?;
            let counts = portable_transfer::export_file(
                &database.pool,
                database.driver,
                std::path::Path::new(&arguments[1]),
            )
            .await
            .map_err(std::io::Error::other)?;
            let row_total = counts.values().sum::<usize>();
            let table_counts = counts
                .iter()
                .map(|(table, count)| format!("{table}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Database export written ({row_total} rows; {table_counts})");
            database.pool.close().await;
            Ok(())
        });
    }
    config::load()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_server())
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let api_token =
        env::var("API_ACCESS_TOKEN").map_err(|_| "API_ACCESS_TOKEN must be configured")?;
    if api_token.trim().is_empty() {
        return Err("API_ACCESS_TOKEN must not be empty".into());
    }
    let database = db::connect().await?;
    let plugins = plugins::PluginHost::new(database.pool.clone(), database.driver == "mysql");
    plugins
        .load_configured()
        .await
        .map_err(|failure| format!("plugin loading failed: {}", failure.code))?;
    plugins
        .initialize_all()
        .await
        .map_err(|failure| format!("plugin initialization failed: {}", failure.code))?;
    let configured_runtime =
        core::runtime_settings::get(&database.pool, database.driver == "mysql")
            .await
            .map_err(|error| format!("runtime settings loading failed: {error:?}"))?;
    let (runtime_updates, runtime_receiver) = watch::channel(configured_runtime.settings);
    let subtitle_worker_status = std::sync::Arc::new(Mutex::new(
        core::subtitle_worker::WorkerStatus::from_environment(),
    ));
    let state = AppState {
        database,
        api_token,
        plugins,
        runtime_updates,
        automation_status: std::sync::Arc::new(Mutex::new(AutomationStatus::default())),
        subtitle_worker_status: subtitle_worker_status.clone(),
    };
    let protected = Router::new()
        .route(
            "/api/settings/runtime",
            get(get_runtime_settings)
                .put(update_runtime_settings)
                .delete(reset_runtime_settings),
        )
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/{id}", axum::routing::put(set_plugin_enabled))
        .route(
            "/api/plugins/{id}/monitor",
            axum::routing::post(monitor_plugin),
        )
        .route("/api/plugins/{id}/reset", axum::routing::post(reset_plugin))
        .route("/api/plugins/{id}/move", axum::routing::post(move_plugin))
        .route("/api/shows", get(list_shows).post(create_show))
        .route("/api/shows/{show_id}", get(get_show))
        .route(
            "/api/shows/{show_id}/storages",
            get(list_show_storages).post(create_show_storage),
        )
        .route(
            "/api/shows/{show_id}/storages/{storage_id}/episodes",
            get(list_storage_episodes),
        )
        .route(
            "/api/shows/{show_id}/storages/{storage_id}/scan",
            axum::routing::post(scan_storage),
        )
        .route(
            "/api/shows/{show_id}/followup/preview",
            axum::routing::post(preview_followup),
        )
        .route(
            "/api/shows/{show_id}/followup/queue",
            axum::routing::post(queue_followup),
        )
        .route(
            "/api/shows/{show_id}/resources/search",
            axum::routing::post(search_resources),
        )
        .route(
            "/api/shows/{show_id}/metadata-identities",
            get(list_metadata_identities).post(link_metadata_identity),
        )
        .route("/api/events", get(list_events).post(create_event))
        .route(
            "/api/events/{id}/transition",
            axum::routing::post(transition_event),
        )
        .route("/api/events/{id}/retry", axum::routing::post(retry_event))
        .route(
            "/api/events/{id}/process",
            axum::routing::post(process_followup_event),
        )
        .route(
            "/api/events/{id}/recover",
            axum::routing::post(recover_followup_event),
        )
        .route("/api/plugin-calls", get(list_plugin_calls))
        .route("/api/subtitles/status", get(subtitle_status))
        .route("/api/subtitle-jobs", get(list_subtitle_jobs))
        .route(
            "/api/shows/{show_id}/subtitle-jobs",
            axum::routing::post(queue_subtitle_job),
        )
        .route("/api/subtitle-jobs/{id}", get(get_subtitle_job))
        .route(
            "/api/subtitle-jobs/{id}/retry",
            axum::routing::post(retry_subtitle_job),
        )
        .route("/api/metadata/search", get(search_metadata))
        .route("/api/automation/status", get(automation_status))
        .route("/api/automation/scan", axum::routing::post(automation_scan))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    let app = Router::new()
        .route("/api/health", get(health))
        .merge(protected)
        .fallback_service(ServeDir::new("frontend/dist").append_index_html_on_directories(true))
        .with_state(state.clone());
    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_owned())
        .parse::<u16>()?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(address).await?;
    info!(address = %address, "drama-warden Rust backend listening");
    tokio::spawn(run_scheduler(state.clone(), runtime_receiver));
    tokio::spawn(core::subtitle_worker::run_worker(
        state.database.pool.clone(),
        state.plugins.clone(),
        state.database.driver == "mysql",
        subtitle_worker_status,
    ));
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
