use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::{subtitle_content, subtitles};
use crate::plugins::{PluginHost, PluginSummary};
use sqlx::AnyPool;

const PROVIDER_LIST_LIMIT: usize = 20;
const SIDE_CAR_EXTENSIONS: &[&str] = &[".srt", ".ass", ".ssa", ".vtt"];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub enabled: bool,
    pub processing: bool,
    pub interval_ms: u64,
}

impl WorkerStatus {
    pub fn from_environment() -> Self {
        let enabled = std::env::var("SUBTITLE_WORKER_ENABLED")
            .ok()
            .map(|value| value.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        Self {
            enabled,
            processing: false,
            interval_ms: 30_000,
        }
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn safe_code(value: Option<&str>) -> String {
    value
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        })
        .unwrap_or("SUBTITLE_JOB_FAILED")
        .to_owned()
}

fn storage_plugin(plugins: &[PluginSummary], provider_id: &str) -> bool {
    plugins.iter().any(|plugin| {
        plugin.id == provider_id
            && plugin.enabled
            && plugin.kind == "storage-provider"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "writeSidecar")
    })
}

fn subtitle_plugins(plugins: &[PluginSummary]) -> Vec<&PluginSummary> {
    plugins
        .iter()
        .filter(|plugin| {
            plugin.enabled
                && plugin.kind == "subtitle-provider"
                && plugin
                    .capabilities
                    .iter()
                    .any(|capability| capability == "search")
                && plugin
                    .capabilities
                    .iter()
                    .any(|capability| capability == "download")
        })
        .collect()
}

fn path_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(
        ".",
        |(parent, _)| if parent.is_empty() { "/" } else { parent },
    )
}

fn file_extension(name: &str) -> &str {
    name.rfind('.').map_or("", |index| &name[index..])
}

fn same_base_subtitle(file: &Value, video_base: &str) -> bool {
    if file.get("isDir").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let Some(name) = string(file, "name").or_else(|| string(file, "fileName")) else {
        return false;
    };
    let extension = file_extension(name).to_ascii_lowercase();
    if !SIDE_CAR_EXTENSIONS.contains(&extension.as_str()) {
        return false;
    }
    let base = name
        .get(..name.len().saturating_sub(extension.len()))
        .unwrap_or_default();
    base.eq_ignore_ascii_case(video_base)
}

async fn list_parent(
    plugins: &PluginHost,
    provider_id: &str,
    relative_path: &str,
    correlation_id: &str,
) -> Result<Vec<Value>, String> {
    let listing = plugins
        .invoke(
            provider_id,
            "list",
            json!({"path":path_parent(relative_path),"options":{"recursive":false,"limit":200}}),
            "subtitle-worker",
            Some(correlation_id.to_owned()),
        )
        .await
        .map_err(|error| error.code)?;
    if listing.get("truncated").and_then(Value::as_bool) != Some(false) {
        return Err("INCOMPLETE_STORAGE_LISTING".to_owned());
    }
    listing
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "INVALID_STORAGE_LISTING".to_owned())
}

async fn process_job(
    pool: &AnyPool,
    plugins: &PluginHost,
    mysql: bool,
    job: &subtitles::SubtitleJob,
    token: &str,
) -> Result<(), String> {
    let context = subtitles::context(pool, job.id)
        .await
        .map_err(|_| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?
        .ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let show = context
        .get("show")
        .ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let storage = context
        .get("storage")
        .ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    if storage.get("enabled").and_then(Value::as_bool) != Some(true) {
        return Err("SUBTITLE_TARGET_UNAVAILABLE".to_owned());
    }
    let provider_id =
        string(storage, "providerId").ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let assets = context
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    if assets.is_empty() {
        subtitles::skip(pool, job.id, "VIDEO_ASSET_NOT_FOUND", token, mysql)
            .await
            .map_err(|_| "SUBTITLE_JOB_STATE_CHANGED".to_owned())?;
        return Ok(());
    }
    if assets.len() != 1 {
        subtitles::skip(pool, job.id, "VIDEO_ASSET_AMBIGUOUS", token, mysql)
            .await
            .map_err(|_| "SUBTITLE_JOB_STATE_CHANGED".to_owned())?;
        return Ok(());
    }
    let available_plugins = plugins.list().await;
    if !storage_plugin(&available_plugins, provider_id) {
        return Err("SIDECAR_WRITE_UNSUPPORTED".to_owned());
    }
    let asset = &assets[0];
    let relative_path =
        string(asset, "relativePath").ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let video_name =
        string(asset, "fileName").ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let base = video_name
        .rfind('.')
        .map(|index| &video_name[..index])
        .filter(|base| !base.is_empty())
        .ok_or_else(|| "INVALID_VIDEO_NAME".to_owned())?;
    let correlation_id = format!("subtitle-job-{}", job.id);
    let existing = list_parent(plugins, provider_id, relative_path, &correlation_id).await?;
    if existing.iter().any(|file| same_base_subtitle(file, base)) {
        subtitles::skip(pool, job.id, "SUBTITLE_ALREADY_EXISTS", token, mysql)
            .await
            .map_err(|_| "SUBTITLE_JOB_STATE_CHANGED".to_owned())?;
        return Ok(());
    }

    let providers = subtitle_plugins(&available_plugins);
    if providers.is_empty() {
        return Err("NO_SUBTITLE_PROVIDER".to_owned());
    }
    let name = string(show, "name").ok_or_else(|| "SUBTITLE_TARGET_UNAVAILABLE".to_owned())?;
    let season = job.season_number;
    let episode = job.episode_number;
    let query = format!("{name} S{season:02}E{episode:02}");
    let mut provider_errors = 0_usize;
    let mut successful_searches = 0_usize;
    let mut last_download_error = None;
    let mut selected: Option<(String, subtitle_content::SubtitleContent)> = None;
    for provider in providers.iter().copied() {
        let found = plugins.invoke(
            &provider.id,
            "search",
            json!({"query":query,"languages":&job.languages,"seasonNumber":season,"episodeNumber":episode,"limit":PROVIDER_LIST_LIMIT}),
            "subtitle-worker",
            Some(correlation_id.clone()),
        ).await;
        let found = match found {
            Ok(Value::Array(found)) => {
                successful_searches += 1;
                found
            }
            Ok(_) => {
                provider_errors += 1;
                continue;
            }
            Err(error) => {
                provider_errors += 1;
                tracing::warn!(provider_id=%provider.id,error_code=%error.code,"subtitle provider search failed");
                continue;
            }
        };
        for candidate in found.iter().filter(|candidate| {
            candidate.get("id").is_some_and(|id| !id.is_null())
                && subtitle_content::candidate_matches(candidate, season, episode)
        }) {
            let candidate_id = candidate
                .get("id")
                .map(|id| match id {
                    Value::String(value) => value.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let downloaded = plugins
                .invoke(
                    &provider.id,
                    "download",
                    json!({"candidateId":candidate_id}),
                    "subtitle-worker",
                    Some(correlation_id.clone()),
                )
                .await;
            let result = (|| {
                let response = downloaded.map_err(|error| error.code)?;
                let file_name = response
                    .get("fileName")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "INVALID_SUBTITLE_RESPONSE".to_owned())?;
                let encoded = response
                    .get("contentBase64")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "INVALID_SUBTITLE_RESPONSE".to_owned())?;
                let bytes = crate::portable_transfer::decode_base64(encoded)
                    .map_err(|_| "INVALID_SUBTITLE_RESPONSE".to_owned())?;
                let content =
                    subtitle_content::normalize_download(file_name, bytes, season, episode)
                        .map_err(|error| error.0.to_owned())?;
                Ok::<_, String>(content)
            })();
            match result {
                Ok(content) => {
                    selected = Some((provider.id.clone(), content));
                    break;
                }
                Err(code) => {
                    last_download_error = Some(code);
                }
            }
        }
        if selected.is_some() {
            break;
        }
    }
    let Some((subtitle_provider_id, downloaded)) = selected else {
        if let Some(code) = last_download_error {
            return Err(code);
        }
        if successful_searches == 0 && provider_errors == providers.len() {
            return Err("ALL_SUBTITLE_PROVIDERS_FAILED".to_owned());
        }
        subtitles::skip(pool, job.id, "NO_MATCHING_SUBTITLE", token, mysql)
            .await
            .map_err(|_| "SUBTITLE_JOB_STATE_CHANGED".to_owned())?;
        return Ok(());
    };
    let output_name = subtitle_content::sidecar_name(video_name, &downloaded.file_name)
        .map_err(|error| error.0.to_owned())?;
    let content_base64 = crate::portable_transfer::encode_base64(&downloaded.bytes);
    subtitles::mark_write_started(pool, job.id, &subtitle_provider_id, token, mysql)
        .await
        .map_err(|_| "SUBTITLE_JOB_LEASE_LOST".to_owned())?;
    let result = plugins.invoke(
        provider_id,
        "writeSidecar",
        json!({"videoPath":relative_path,"fileName":output_name,"contentBase64":content_base64}),
        "subtitle-worker",
        Some(correlation_id),
    ).await.map_err(|error| error.code)?;
    let output_path = result
        .get("path")
        .and_then(Value::as_str)
        .filter(|_| result.get("ok").and_then(Value::as_bool) == Some(true))
        .ok_or_else(|| "SIDECAR_WRITE_UNCONFIRMED".to_owned())?;
    subtitles::complete(
        pool,
        job.id,
        &subtitle_provider_id,
        output_path,
        token,
        mysql,
    )
    .await
    .map_err(|_| "SUBTITLE_JOB_STATE_CHANGED".to_owned())?;
    Ok(())
}

pub async fn tick(pool: &AnyPool, plugins: &PluginHost, mysql: bool) {
    let claimed = match subtitles::claim_next(pool, mysql).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::error!(error=?error,"subtitle queue claim failed");
            return;
        }
    };
    let Some((job, token)) = claimed else { return };
    if let Err(code) = process_job(pool, plugins, mysql, &job, &token).await {
        let code = safe_code(Some(&code));
        if subtitles::fail(pool, job.id, &code, &token, mysql)
            .await
            .is_err()
        {
            tracing::error!(
                job_id = job.id,
                error_code = "SUBTITLE_JOB_STATE_PERSIST_FAILED",
                "subtitle failure could not be recorded"
            );
        }
        tracing::warn!(job_id=job.id,error_code=%code,"subtitle job failed");
    }
}

pub async fn run_worker(
    pool: AnyPool,
    plugins: PluginHost,
    mysql: bool,
    status: Arc<Mutex<WorkerStatus>>,
) {
    if !status.lock().await.enabled {
        return;
    }
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        status.lock().await.processing = true;
        tick(&pool, &plugins, mysql).await;
        status.lock().await.processing = false;
    }
}
