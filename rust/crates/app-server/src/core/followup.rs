use crate::{
    core::{episodes, events, shows},
    plugins::PluginHost,
};
use serde_json::{Value, json};
use sqlx::AnyPool;
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

static ACTIVE_EVENTS: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

struct ActiveEvent(i64);

impl ActiveEvent {
    fn acquire(id: i64) -> Result<Self, events::EventError> {
        let mut active = ACTIVE_EVENTS.lock().expect("active event mutex poisoned");
        if !active.insert(id) {
            return Err(events::EventError::Conflict("event_still_active"));
        }
        Ok(Self(id))
    }
}

impl Drop for ActiveEvent {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_EVENTS.lock() {
            active.remove(&self.0);
        }
    }
}

struct ProcessError {
    code: String,
    stage: String,
    write_attempted: bool,
}

impl ProcessError {
    fn new(code: impl Into<String>, stage: &str, write_attempted: bool) -> Self {
        Self {
            code: code.into(),
            stage: stage.to_owned(),
            write_attempted,
        }
    }
}

struct ProcessContext {
    stage: String,
    write_attempted: bool,
}

fn as_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn size(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .filter(|value| *value >= 0)
            .map(|value| value as u64)
    })
}

fn indexer_task_name(provider_id: &str, category: &str) -> Option<String> {
    let provider = match provider_id {
        "pan115" => "115",
        "quark" => "quark",
        _ => return None,
    };
    let category = match category {
        "movie" => "电影",
        "tv" => "电视剧",
        "anime" => "动漫",
        "variety" => "综艺",
        _ => return None,
    };
    Some(if provider == "quark" && category == "电视剧" {
        "quark_strm_电视剧".to_owned()
    } else if provider == "quark" {
        format!("quark_{category}")
    } else {
        format!("115_{category}")
    })
}

fn basename(file: &Value) -> Option<String> {
    as_text(file.get("name").or_else(|| file.get("fileName"))).or_else(|| {
        let path = file.get("path")?.as_str()?;
        path.rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    })
}

fn normalize_path(path: &str) -> Result<String, &'static str> {
    if path.contains('\0') || path.contains('\\') {
        return Err("INVALID_STORAGE_PATH");
    }
    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return Err("INVALID_STORAGE_PATH");
        }
        segments.push(segment);
    }
    Ok(format!(
        "{}{}",
        if absolute { "/" } else { "" },
        segments.join("/")
    ))
}

fn join_path(base: &str, name: &str) -> Result<String, &'static str> {
    let base = normalize_path(base)?;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err("INVALID_STORAGE_PATH");
    }
    Ok(if base.is_empty() {
        name.to_owned()
    } else if base == "/" {
        format!("/{name}")
    } else {
        format!("{base}/{name}")
    })
}

fn relative_path(root: &str, path: &str) -> Result<String, &'static str> {
    let root = normalize_path(root)?;
    let path = normalize_path(path)?;
    if path.is_empty() || path == root {
        return Err("INVALID_STORAGE_PATH");
    }
    if root.is_empty() {
        return Ok(path.trim_start_matches('/').to_owned());
    }
    let prefix = if root == "/" {
        "/".to_owned()
    } else {
        format!("{root}/")
    };
    path.strip_prefix(&prefix)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or("PATH_OUTSIDE_ROOT")
}

fn complete_files(listing: &Value, label: &str) -> Result<Vec<Value>, &'static str> {
    if listing.get("truncated").and_then(Value::as_bool) != Some(false) {
        return Err("INCOMPLETE_LISTING");
    }
    listing
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            let _ = label;
            "INVALID_STORAGE_LISTING"
        })
}

fn canonical_name(
    show_name: &str,
    season: i64,
    episode: i64,
    quality: Option<&str>,
    source_name: &str,
) -> Result<String, &'static str> {
    let title: String = show_name
        .chars()
        .map(|character| {
            if character == '\0' || character.is_control() || "\\/:*?\"<>|".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.trim().is_empty() || season <= 0 || episode <= 0 {
        return Err("INVALID_EVENT_SELECTION");
    }
    let extension = source_name
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| {
            !ext.is_empty()
                && ext.len() <= 8
                && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or("INVALID_EVENT_SELECTION")?;
    let extension = extension.to_ascii_lowercase();
    if ![
        "mp4", "mkv", "m4v", "avi", "mov", "ts", "m2ts", "wmv", "flv", "webm", "mpg", "mpeg",
        "rmvb", "rm",
    ]
    .contains(&extension.as_str())
    {
        return Err("INVALID_EVENT_SELECTION");
    }
    let quality = quality.filter(|value| !value.trim().is_empty());
    Ok(format!(
        "{} S{season:02}E{episode:02}{}.{extension}",
        title.trim(),
        quality.map(|value| format!(" {value}")).unwrap_or_default()
    ))
}

fn file_key(file: &Value, root: &str) -> Result<String, &'static str> {
    if file.get("isDir").and_then(Value::as_bool) == Some(true) {
        return Err("INVALID_STORAGE_LISTING");
    }
    let path = file
        .get("path")
        .and_then(Value::as_str)
        .ok_or("INVALID_STORAGE_LISTING")?;
    let relative = relative_path(root, path)?;
    let size = size(file.get("size"))
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(format!("{relative}\0{size}"))
}

fn plugin_error(error: crate::plugins::PluginFailure, context: &ProcessContext) -> ProcessError {
    ProcessError::new(error.code, &context.stage, context.write_attempted)
}

async fn invoke(
    plugins: &PluginHost,
    provider_id: &str,
    capability: &str,
    input: Value,
    event: &events::FollowupWorkItem,
    context: &ProcessContext,
) -> Result<Value, ProcessError> {
    plugins
        .invoke(
            provider_id,
            capability,
            input,
            "followup-process",
            event.correlation_id.clone(),
        )
        .await
        .map_err(|error| plugin_error(error, context))
}

async fn process_inner(
    pool: &AnyPool,
    plugins: &PluginHost,
    event: &events::FollowupWorkItem,
    mysql: bool,
    context: &mut ProcessContext,
) -> Result<Value, ProcessError> {
    let fail = |code: &str, context: &ProcessContext| {
        ProcessError::new(code, &context.stage, context.write_attempted)
    };
    let storage_id = event
        .payload
        .get("storageId")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
    let season_number = event
        .payload
        .get("seasonNumber")
        .and_then(Value::as_i64)
        .filter(|number| *number > 0)
        .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
    let provider_id = as_text(event.payload.get("providerId"))
        .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
    let share_url = as_text(event.payload.get("shareUrl"))
        .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
    let selected = event
        .payload
        .get("selectedFiles")
        .and_then(Value::as_array)
        .cloned()
        .filter(|files| !files.is_empty())
        .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
    let mut episode_numbers = Vec::with_capacity(selected.len());
    for file in &selected {
        let number = file
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .filter(|number| *number > 0)
            .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
        if episode_numbers.contains(&number) {
            return Err(fail("INVALID_EVENT_SELECTION", context));
        }
        episode_numbers.push(number);
    }
    let show = shows::get(pool, event.show_id, mysql)
        .await
        .map_err(|_| fail("DATABASE_ERROR", context))?
        .ok_or_else(|| fail("SHOW_NOT_FOUND", context))?;
    let storage = episodes::get_storage(pool, event.show_id, storage_id, mysql)
        .await
        .map_err(|_| fail("STORAGE_NOT_FOUND", context))?;
    let metadata_episodes = episodes::list_episode_metadata(pool, event.show_id, season_number)
        .await
        .map_err(|_| fail("DATABASE_ERROR", context))?;
    if !storage.enabled || storage.provider_id != provider_id {
        return Err(fail("STORAGE_CONFIGURATION_CHANGED", context));
    }
    events::verify_followup_claims(pool, event.id, storage_id, season_number, &episode_numbers)
        .await
        .map_err(|error| {
            fail(
                match error {
                    events::EventError::Conflict(code) => code,
                    _ => "WRITE_CLAIMS_REQUIRE_REVIEW",
                },
                context,
            )
        })?;
    let summaries = plugins.list().await;
    let storage_plugin = summaries.iter().find(|plugin| {
        plugin.id == provider_id && plugin.enabled && plugin.kind == "storage-provider"
    });
    if storage_plugin.is_none() {
        return Err(fail("STORAGE_PROVIDER_UNAVAILABLE", context));
    }
    let list_capability = |capability: &str| {
        storage_plugin
            .is_some_and(|plugin| plugin.capabilities.iter().any(|name| name == capability))
    };
    if !list_capability("listSharedFiles") || !list_capability("list") {
        return Err(fail("STORAGE_CAPABILITY_UNAVAILABLE", context));
    }

    context.stage = "share-recheck".to_owned();
    let source_listing = invoke(
        plugins,
        &provider_id,
        "listSharedFiles",
        json!({"shareUrl":share_url}),
        event,
        context,
    )
    .await?;
    let source_files =
        complete_files(&source_listing, "share").map_err(|code| fail(code, context))?;
    let mut fresh_selection = Vec::with_capacity(selected.len());
    for expected in &selected {
        let expected_id =
            as_text(expected.get("id")).ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
        let expected_name = expected
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
        let expected_size =
            size(expected.get("size")).ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
        let episode_number = expected
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let matches = source_files
            .iter()
            .filter(|file| {
                file.get("isDir").and_then(Value::as_bool) != Some(true)
                    && as_text(file.get("id")).as_deref() == Some(expected_id.as_str())
                    && basename(file).as_deref() == Some(expected_name)
                    && size(file.get("size")) == Some(expected_size)
                    && file
                        .get("path")
                        .and_then(Value::as_str)
                        .is_some_and(|path| {
                            let parsed = basename(file)
                                .and_then(|name| {
                                    let season = super::followup_plan::video_season_number(&name)?;
                                    let episode = super::followup_plan::selected_episode_number(
                                        &name,
                                        &metadata_episodes,
                                    )?;
                                    Some((season, episode))
                                });
                            parsed.is_some_and(|(season, episode)| {
                                season == season_number && episode == episode_number
                            }) && !path.is_empty()
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(fail("SHARE_SELECTION_CHANGED", context));
        }
        fresh_selection.push(matches[0].clone());
    }

    let destination_path = join_path(&storage.root_path, &format!("Season {season_number:02}"))
        .map_err(|code| fail(code, context))?;
    let mut canonical = Vec::with_capacity(selected.len());
    for file in &selected {
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("INVALID_EVENT_SELECTION", context))?;
        let episode = file
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let quality = file.get("quality").and_then(Value::as_str);
        let canonical_name = canonical_name(&show.name, season_number, episode, quality, name)
            .map_err(|code| fail(code, context))?;
        canonical.push((file.clone(), canonical_name));
    }
    context.stage = "destination-prepare".to_owned();
    invoke(
        plugins,
        &provider_id,
        "mkdir",
        json!({"path":destination_path}),
        event,
        context,
    )
    .await?;
    let before_listing = invoke(
        plugins,
        &provider_id,
        "list",
        json!({"path":destination_path,"options":{"recursive":true}}),
        event,
        context,
    )
    .await?;
    let before_files =
        complete_files(&before_listing, "destination").map_err(|code| fail(code, context))?;
    let before_keys = before_files
        .iter()
        .filter(|file| file.get("isDir").and_then(Value::as_bool) != Some(true))
        .map(|file| file_key(file, &destination_path))
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|code| fail(code, context))?;
    let occupied: HashSet<String> = before_files
        .iter()
        .filter(|file| file.get("isDir").and_then(Value::as_bool) != Some(true))
        .filter_map(basename)
        .collect();
    if canonical.iter().any(|(file, canonical)| {
        file.get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| occupied.contains(name))
            || occupied.contains(canonical)
    }) {
        return Err(fail("DESTINATION_CONFLICT", context));
    }

    context.stage = "storage-save".to_owned();
    context.write_attempted = true;
    invoke(plugins, &provider_id, "save", json!({"shareUrl":share_url,"destinationPath":destination_path,"selection":fresh_selection}), event, context).await?;

    context.stage = "save-verification".to_owned();
    let after_listing = invoke(
        plugins,
        &provider_id,
        "list",
        json!({"path":destination_path,"options":{"recursive":true}}),
        event,
        context,
    )
    .await?;
    let after_files =
        complete_files(&after_listing, "saved-file").map_err(|code| fail(code, context))?;
    let new_files = after_files
        .iter()
        .filter(|file| file.get("isDir").and_then(Value::as_bool) != Some(true))
        .filter(|file| {
            file_key(file, &destination_path).is_ok_and(|key| !before_keys.contains(&key))
        })
        .cloned()
        .collect::<Vec<_>>();
    if new_files.len() != canonical.len() {
        return Err(fail("SAVE_SELECTION_MISMATCH", context));
    }
    let mut moved = Vec::new();
    for (expected, canonical_name) in &canonical {
        let expected_name = expected.get("name").and_then(Value::as_str).unwrap_or("");
        let expected_size = size(expected.get("size"));
        let matches = new_files
            .iter()
            .filter(|file| {
                basename(file).as_deref() == Some(expected_name)
                    && size(file.get("size")) == expected_size
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(fail("SAVE_VERIFICATION_FAILED", context));
        }
        let file = matches[0];
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("SAVE_VERIFICATION_FAILED", context))?;
        let target_path =
            join_path(&destination_path, expected_name).map_err(|code| fail(code, context))?;
        if path != target_path {
            context.stage = "storage-move".to_owned();
            invoke(
                plugins,
                &provider_id,
                "move",
                json!({"sourcePath":path,"destinationPath":target_path}),
                event,
                context,
            )
            .await?;
        }
        moved.push((
            expected.clone(),
            canonical_name.clone(),
            target_path,
            expected_size.unwrap_or(0),
        ));
    }
    let rename_items = moved
        .iter()
        .filter(|(expected, canonical_name, _, _)| {
            expected.get("name").and_then(Value::as_str) != Some(canonical_name.as_str())
        })
        .map(|(_, canonical_name, path, _)| json!({"path":path,"newName":canonical_name}))
        .collect::<Vec<_>>();
    if !rename_items.is_empty() {
        context.stage = "storage-rename".to_owned();
        invoke(
            plugins,
            &provider_id,
            "renameMany",
            json!({"items":rename_items}),
            event,
            context,
        )
        .await?;
    }

    context.stage = "final-verification".to_owned();
    let final_listing = invoke(
        plugins,
        &provider_id,
        "list",
        json!({"path":destination_path,"options":{"recursive":true}}),
        event,
        context,
    )
    .await?;
    let final_files =
        complete_files(&final_listing, "final destination").map_err(|code| fail(code, context))?;
    let mut final_assets = Vec::new();
    for (expected, canonical_name) in &canonical {
        let expected_size = size(expected.get("size")).unwrap_or(0);
        let matches = final_files
            .iter()
            .filter(|file| {
                file.get("isDir").and_then(Value::as_bool) != Some(true)
                    && basename(file).as_deref() == Some(canonical_name.as_str())
                    && size(file.get("size")) == Some(expected_size)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(fail("FINAL_VERIFICATION_FAILED", context));
        }
        let file = matches[0];
        let file_path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("FINAL_VERIFICATION_FAILED", context))?;
        let relative =
            relative_path(&storage.root_path, file_path).map_err(|code| fail(code, context))?;
        let number = expected
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        final_assets.push(episodes::ScannedAsset {
            season_number,
            episode_number: number,
            asset_key: relative.clone(),
            relative_path: relative,
            file_name: canonical_name.clone(),
            file_size: i64::try_from(expected_size).ok(),
            file_mtime: file.get("mtime").and_then(Value::as_i64),
            quality: expected
                .get("quality")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    context.stage = "catalog-sync".to_owned();
    episodes::sync_scanned_assets(
        pool,
        event.show_id,
        storage_id,
        &format!("followup-{}", event.id),
        &final_assets,
        false,
        mysql,
    )
    .await
    .map_err(|_| fail("CATALOG_SYNC_FAILED", context))?;

    context.stage = "media-index".to_owned();
    let indexer = summaries.iter().find(|plugin| {
        plugin.enabled
            && plugin.kind == "indexer"
            && plugin
                .capabilities
                .iter()
                .any(|capability| capability == "reindex")
    });
    let mut index_result = json!({"skipped":true,"reason":"no_enabled_indexer"});
    if let Some(indexer) = indexer {
        let task_name = indexer_task_name(&storage.provider_id, &show.category);
        index_result = invoke(plugins, &indexer.id, "reindex", json!({"showId":event.show_id,"storageId":storage_id,"path":storage.root_path,"seasonNumber":season_number,"taskName":task_name}), event, context).await?;
        if index_result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(fail("INDEXER_UNCONFIRMED", context));
        }
    }
    let finished = events::finish_followup_processed(
        pool,
        event.id,
        storage_id,
        season_number,
        &episode_numbers,
        mysql,
    )
    .await
    .map_err(|_| fail("EVENT_FINALIZE_FAILED", context))?;
    let notification = events::notification_event(&finished);
    let _ = plugins.publish_event(notification).await;
    Ok(
        json!({"ok":true,"event":finished,"episodeNumbers":episode_numbers,"indexResult":index_result}),
    )
}

pub async fn process_event(
    pool: &AnyPool,
    plugins: &PluginHost,
    event_id: i64,
    mysql: bool,
) -> Result<Value, events::EventError> {
    let _active = ActiveEvent::acquire(event_id)?;
    let event = events::begin_followup_process(pool, event_id, mysql).await?;
    let mut context = ProcessContext {
        stage: "preflight".to_owned(),
        write_attempted: false,
    };
    match process_inner(pool, plugins, &event, mysql, &mut context).await {
        Ok(result) => Ok(result),
        Err(error) => {
            let terminal_status = if !error.write_attempted
                && ["DESTINATION_CONFLICT", "EPISODE_WRITE_CLAIMED"].contains(&error.code.as_str())
            {
                "skipped"
            } else {
                "failed"
            };
            let event = events::finish_followup_failed(
                pool,
                event.id,
                &format!("{} failed ({})", error.stage, error.code),
                terminal_status,
                error.write_attempted,
                mysql,
            )
            .await?;
            let notification = events::notification_event(&event);
            let _ = plugins.publish_event(notification).await;
            Ok(json!({"ok":false,"event":event,"stage":error.stage,"errorCode":error.code}))
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoverRequest {
    pub action: String,
    pub confirm_no_active_worker: bool,
    pub confirm_outputs_match: Option<bool>,
    pub confirm_no_output: Option<bool>,
}

fn recovery_error(code: &'static str) -> events::EventError {
    events::EventError::Conflict(code)
}

pub async fn recover_event(
    pool: &AnyPool,
    plugins: &PluginHost,
    event_id: i64,
    input: RecoverRequest,
    mysql: bool,
) -> Result<Value, events::EventError> {
    let _active = ActiveEvent::acquire(event_id)?;
    if !input.confirm_no_active_worker {
        return Err(events::EventError::Invalid(
            "confirm_no_active_worker_required",
        ));
    }
    if !["complete", "retry_after_review"].contains(&input.action.as_str()) {
        return Err(events::EventError::Invalid("invalid_recovery_action"));
    }
    if input.action == "complete" && input.confirm_outputs_match != Some(true) {
        return Err(events::EventError::Invalid(
            "confirm_outputs_match_required",
        ));
    }
    if input.action == "retry_after_review" && input.confirm_no_output != Some(true) {
        return Err(events::EventError::Invalid("confirm_no_output_required"));
    }

    let event = events::get_followup_for_recovery(pool, event_id).await?;
    let storage_id = event
        .payload
        .get("storageId")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| recovery_error("invalid_event_selection"))?;
    let season_number = event
        .payload
        .get("seasonNumber")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| recovery_error("invalid_event_selection"))?;
    let provider_id = as_text(event.payload.get("providerId"))
        .ok_or_else(|| recovery_error("invalid_event_selection"))?;
    let selected = event
        .payload
        .get("selectedFiles")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| recovery_error("invalid_event_selection"))?;
    let mut episode_numbers = Vec::with_capacity(selected.len());
    for file in selected {
        let episode = file
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or_else(|| recovery_error("invalid_event_selection"))?;
        if episode_numbers.contains(&episode) {
            return Err(recovery_error("invalid_event_selection"));
        }
        episode_numbers.push(episode);
    }
    events::verify_recovery_claims(
        pool,
        &event,
        storage_id,
        season_number,
        &episode_numbers,
        &input.action,
        mysql,
    )
    .await?;
    let show = shows::get(pool, event.show_id, mysql)
        .await
        .map_err(|_| recovery_error("show_not_found"))?
        .ok_or_else(|| recovery_error("show_not_found"))?;
    let storage = episodes::get_storage(pool, event.show_id, storage_id, mysql)
        .await
        .map_err(|_| recovery_error("storage_not_found"))?;
    if !storage.enabled || storage.provider_id != provider_id {
        return Err(recovery_error("recovery_configuration_changed"));
    }
    let summaries = plugins.list().await;
    let plugin = summaries
        .iter()
        .find(|item| item.id == provider_id && item.enabled && item.kind == "storage-provider")
        .ok_or_else(|| recovery_error("recovery_storage_unavailable"))?;
    for capability in ["list", "mkdir", "move", "renameMany"] {
        if input.action == "complete" && !plugin.capabilities.iter().any(|name| name == capability)
        {
            return Err(recovery_error("recovery_capability_unavailable"));
        }
    }
    if !plugin.capabilities.iter().any(|name| name == "list") {
        return Err(recovery_error("recovery_capability_unavailable"));
    }
    let work = events::FollowupWorkItem {
        id: event.id,
        show_id: event.show_id,
        correlation_id: event.correlation_id.clone(),
        payload: event.payload.clone(),
    };
    let context = ProcessContext {
        stage: "recovery".to_owned(),
        write_attempted: true,
    };
    let destination = join_path(&storage.root_path, &format!("Season {season_number:02}"))
        .map_err(recovery_error)?;
    let mut files;
    if input.action == "complete"
        && plugin.capabilities.iter().any(|name| name == "mkdir")
    {
        invoke(
            plugins,
            &provider_id,
            "mkdir",
            json!({"path":destination}),
            &work,
            &context,
        )
        .await
        .map_err(|_| recovery_error("recovery_plugin_failed"))?;
    }
    let listing = invoke(
        plugins,
        &provider_id,
        "list",
        json!({"path":destination,"options":{"recursive":true}}),
        &work,
        &context,
    )
    .await
    .map_err(|_| recovery_error("recovery_plugin_failed"))?;
    files = complete_files(&listing, "recovery").map_err(recovery_error)?;

    let mut expected = Vec::with_capacity(selected.len());
    for file in selected {
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| recovery_error("invalid_event_selection"))?;
        let file_size =
            size(file.get("size")).ok_or_else(|| recovery_error("invalid_event_selection"))?;
        let episode = file
            .get("episodeNumber")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let canonical = canonical_name(
            &show.name,
            season_number,
            episode,
            file.get("quality").and_then(Value::as_str),
            name,
        )
        .map_err(recovery_error)?;
        expected.push((file.clone(), name.to_owned(), canonical, file_size, episode));
    }
    if input.action == "retry_after_review" {
        let possible: HashSet<String> = expected
            .iter()
            .flat_map(|(_, name, canonical, _, _)| [name.clone(), canonical.clone()])
            .collect();
        if files.iter().any(|file| {
            file.get("isDir").and_then(Value::as_bool) != Some(true)
                && basename(file).is_some_and(|name| possible.contains(&name))
        }) {
            return Err(recovery_error("recovery_output_present"));
        }
        let event = events::recover_followup_no_output(
            pool,
            event_id,
            storage_id,
            season_number,
            &episode_numbers,
            mysql,
        )
        .await?;
        return Ok(json!({"ok":true,"event":event,"resolution":"retryable"}));
    }

    let mut matches = Vec::with_capacity(expected.len());
    for expected_file in &expected {
        let candidates = files
            .iter()
            .filter(|file| {
                file.get("isDir").and_then(Value::as_bool) != Some(true)
                    && file_key(file, &destination).is_ok()
                    && size(file.get("size")) == Some(expected_file.3)
                    && [expected_file.1.as_str(), expected_file.2.as_str()]
                        .contains(&basename(file).as_deref().unwrap_or(""))
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 || candidates[0].get("path").and_then(Value::as_str).is_none() {
            return Err(recovery_error("recovery_incomplete"));
        }
        let matched_path = candidates[0]
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| recovery_error("recovery_incomplete"))?;
        relative_path(&storage.root_path, matched_path).map_err(recovery_error)?;
        matches.push((expected_file, candidates[0].clone()));
    }
    if matches
        .iter()
        .filter_map(|(_, file)| file.get("path").and_then(Value::as_str))
        .collect::<HashSet<_>>()
        .len()
        != matches.len()
    {
        return Err(recovery_error("recovery_ambiguous"));
    }
    let canonical_names: HashSet<&str> = expected.iter().map(|item| item.2.as_str()).collect();
    let selected_paths: HashSet<&str> = matches
        .iter()
        .filter_map(|(_, file)| file.get("path").and_then(Value::as_str))
        .collect();
    if files.iter().any(|file| {
        file.get("isDir").and_then(Value::as_bool) != Some(true)
            && basename(file).is_some_and(|name| canonical_names.contains(name.as_str()))
            && file
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| !selected_paths.contains(path))
    }) {
        return Err(recovery_error("recovery_destination_conflict"));
    }
    let mut rename = Vec::new();
    for (expected_file, file) in &matches {
        let file_path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| recovery_error("recovery_incomplete"))?;
        let current_name = basename(file).ok_or_else(|| recovery_error("recovery_incomplete"))?;
        let target_path = join_path(&destination, &current_name).map_err(recovery_error)?;
        if files.iter().any(|other| {
            other.get("isDir").and_then(Value::as_bool) != Some(true)
                && other.get("path").and_then(Value::as_str) == Some(target_path.as_str())
                && other.get("path").and_then(Value::as_str) != Some(file_path)
        }) {
            return Err(recovery_error("recovery_destination_conflict"));
        }
        if file_path != target_path {
            invoke(
                plugins,
                &provider_id,
                "move",
                json!({"sourcePath":file_path,"destinationPath":target_path}),
                &work,
                &context,
            )
            .await
            .map_err(|_| recovery_error("recovery_plugin_failed"))?;
        }
        if current_name != expected_file.2 {
            rename.push(json!({"path":target_path,"newName":expected_file.2}));
        }
    }
    if !rename.is_empty() {
        invoke(
            plugins,
            &provider_id,
            "renameMany",
            json!({"items":rename}),
            &work,
            &context,
        )
        .await
        .map_err(|_| recovery_error("recovery_plugin_failed"))?;
    }
    let final_listing = invoke(
        plugins,
        &provider_id,
        "list",
        json!({"path":destination,"options":{"recursive":true}}),
        &work,
        &context,
    )
    .await
    .map_err(|_| recovery_error("recovery_plugin_failed"))?;
    files = complete_files(&final_listing, "recovered destination").map_err(recovery_error)?;
    let mut assets = Vec::new();
    for (_, _, canonical, file_size, episode) in &expected {
        let candidates = files
            .iter()
            .filter(|file| {
                file.get("isDir").and_then(Value::as_bool) != Some(true)
                    && file_key(file, &destination).is_ok()
                    && basename(file).as_deref() == Some(canonical)
                    && size(file.get("size")) == Some(*file_size)
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err(recovery_error("recovery_verification_failed"));
        }
        let path = candidates[0]
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| recovery_error("recovery_verification_failed"))?;
        let relative = relative_path(&storage.root_path, path).map_err(recovery_error)?;
        assets.push(episodes::ScannedAsset {
            season_number,
            episode_number: *episode,
            asset_key: relative.clone(),
            relative_path: relative,
            file_name: canonical.clone(),
            file_size: i64::try_from(*file_size).ok(),
            file_mtime: candidates[0].get("mtime").and_then(Value::as_i64),
            quality: expected
                .iter()
                .find(|item| item.4 == *episode)
                .and_then(|item| item.0.get("quality"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    episodes::sync_scanned_assets(
        pool,
        event.show_id,
        storage_id,
        &format!("recovery-{}", event.id),
        &assets,
        false,
        mysql,
    )
    .await
    .map_err(|_| recovery_error("catalog_sync_failed"))?;
    let indexer = summaries.iter().find(|item| {
        item.enabled
            && item.kind == "indexer"
            && item.capabilities.iter().any(|cap| cap == "reindex")
    });
    let mut index_result = json!({"skipped":true,"reason":"no_enabled_indexer"});
    if let Some(indexer) = indexer {
        let task_name = indexer_task_name(&storage.provider_id, &show.category);
        index_result = invoke(plugins, &indexer.id, "reindex", json!({"showId":event.show_id,"storageId":storage_id,"path":storage.root_path,"seasonNumber":season_number,"taskName":task_name}), &work, &context).await
            .map_err(|_| recovery_error("recovery_plugin_failed"))?;
        if index_result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(recovery_error("indexer_unconfirmed"));
        }
    }
    let event = events::finalize_recovered_followup(
        pool,
        event_id,
        storage_id,
        season_number,
        &episode_numbers,
        mysql,
    )
    .await?;
    let notification = events::notification_event(&event);
    let _ = plugins.publish_event(notification).await;
    Ok(
        json!({"ok":true,"event":event,"resolution":"processed","episodeNumbers":episode_numbers,"indexResult":index_result}),
    )
}

#[cfg(test)]
mod tests {
    use super::indexer_task_name;

    #[test]
    fn maps_legacy_storage_and_category_to_indexer_tasks() {
        assert_eq!(
            indexer_task_name("quark", "tv").as_deref(),
            Some("quark_strm_电视剧")
        );
        assert_eq!(
            indexer_task_name("quark", "anime").as_deref(),
            Some("quark_动漫")
        );
        assert_eq!(
            indexer_task_name("pan115", "variety").as_deref(),
            Some("115_综艺")
        );
        assert_eq!(indexer_task_name("unknown", "tv"), None);
    }
}
