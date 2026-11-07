use crate::{core::shows, plugins::PluginHost};
use serde_json::{Value, json};
use sqlx::AnyPool;
use std::collections::{HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug)]
pub struct Manifest {
    pub provider_id: Option<String>,
    pub season_number: Option<i64>,
    pub episodes: Vec<Value>,
    pub max_episode: i64,
    pub sufficient: bool,
    pub required_episode: i64,
    pub coverage_gap_episode: Option<i64>,
    pub failures: Vec<Value>,
}

fn normalized_title(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn manifest_coverage(episodes: &[Value]) -> (HashSet<i64>, i64, Option<i64>) {
    let numbers: HashSet<i64> = episodes
        .iter()
        .filter_map(|episode| episode.get("episodeNumber").and_then(Value::as_i64))
        .filter(|number| *number > 0)
        .collect();
    let maximum = numbers.iter().copied().max().unwrap_or(0);
    let mut next = 1;
    for number in 1..=maximum {
        if !numbers.contains(&number) {
            next = number;
            break;
        }
        next = number + 1;
    }
    let gap = (next <= maximum).then_some(next);
    (numbers, maximum, gap)
}

fn episode_key(file: &Value) -> Option<(i64, i64)> {
    if file.get("isDir").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let name = file
        .get("name")
        .or_else(|| file.get("fileName"))
        .and_then(Value::as_str)?;
    let season = super::followup_plan::video_season_number(name)?;
    let episode = super::followup_plan::video_episode_number(name)?;
    Some((season, episode))
}

pub async fn resolve_episode_manifest(
    pool: &AnyPool,
    plugins: &PluginHost,
    show: &shows::Show,
    current_season: i64,
    current_episode: i64,
    observed_files: &[Value],
    mysql: bool,
) -> Manifest {
    let identities = match shows::list_identities(pool, show.id, mysql).await {
        Ok(items) => items,
        Err(_error) => {
            return Manifest {
                provider_id: None,
                season_number: None,
                episodes: vec![],
                max_episode: 0,
                sufficient: false,
                required_episode: 0,
                coverage_gap_episode: None,
                failures: vec![json!({"code":"METADATA_IDENTITY_READ_FAILED"})],
            };
        }
    };
    let identity_map: HashMap<String, shows::MetadataIdentity> = identities
        .into_iter()
        .map(|identity| (identity.provider_id.clone(), identity))
        .collect();
    let providers = plugins
        .list()
        .await
        .into_iter()
        .filter(|plugin| {
            plugin.enabled
                && plugin.kind == "metadata-provider"
                && plugin
                    .capabilities
                    .iter()
                    .any(|capability| capability == "detail")
                && plugin
                    .capabilities
                    .iter()
                    .any(|capability| capability == "episodes")
        })
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    let mut best_insufficient: Option<Manifest> = None;

    for provider in providers {
        let identity = if let Some(identity) = identity_map.get(&provider.id) {
            Some((identity.external_id.clone(), identity.media_type.clone()))
        } else if provider
            .capabilities
            .iter()
            .any(|capability| capability == "search")
        {
            let mut candidates = HashSet::new();
            for title in
                std::iter::once(show.name.as_str()).chain(show.aliases.iter().map(String::as_str))
            {
                if title.trim().is_empty() {
                    continue;
                }
                match plugins.invoke(&provider.id, "search", json!({"query":title,"mediaType":"tv","limit":25}), "metadata-auto-link", None).await {
                    Ok(results) => {
                        if let Some(results) = results.as_array() {
                            for result in results {
                                let normalized = normalized_title(title);
                                let matches_title = result.get("name").and_then(Value::as_str).is_some_and(|name| normalized_title(name) == normalized)
                                    || result.get("originalName").and_then(Value::as_str).is_some_and(|name| normalized_title(name) == normalized);
                                if matches_title {
                                    if let Some(id) = result.get("externalId").and_then(Value::as_str).or_else(|| result.get("id").and_then(Value::as_str)) {
                                        candidates.insert(id.to_owned());
                                    } else if let Some(id) = result.get("externalId").and_then(Value::as_i64).or_else(|| result.get("id").and_then(Value::as_i64)) {
                                        candidates.insert(id.to_string());
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => failures.push(json!({"providerId":provider.id,"code":error.code,"message":"Metadata provider request failed"})),
                }
            }
            if candidates.len() == 1 {
                let external_id = candidates.into_iter().next().unwrap_or_default();
                match shows::link_identity(
                    pool,
                    show.id,
                    shows::LinkIdentity {
                        provider_id: provider.id.clone(),
                        external_id: external_id.clone(),
                        media_type: "tv".to_owned(),
                        linked_by: "exact-match".to_owned(),
                    },
                    mysql,
                )
                .await
                {
                    Ok(_) => Some((external_id, "tv".to_owned())),
                    Err(_) => {
                        failures.push(json!({"providerId":provider.id,"code":"METADATA_LINK_FAILED","message":"Metadata link could not be saved"}));
                        continue;
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let Some((external_id, media_type)) = identity else {
            continue;
        };

        let detail = match plugins
            .invoke(
                &provider.id,
                "detail",
                json!({"externalId":external_id,"mediaType":media_type}),
                "metadata-resolution",
                None,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => {
                failures.push(json!({"providerId":provider.id,"code":error.code,"message":"Metadata provider request failed"}));
                continue;
            }
        };
        let season_number = detail
            .get("seasons")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|season| season.get("seasonNumber").and_then(Value::as_i64))
            .filter(|number| *number > 0)
            .max();
        let Some(season_number) = season_number else {
            continue;
        };
        let episodes = match plugins
            .invoke(
                &provider.id,
                "episodes",
                json!({"externalId":external_id,"seasonNumber":season_number}),
                "metadata-resolution",
                None,
            )
            .await
        {
            Ok(value) => value.as_array().cloned().unwrap_or_default(),
            Err(error) => {
                failures.push(json!({"providerId":provider.id,"code":error.code,"message":"Metadata provider request failed"}));
                continue;
            }
        };
        let episodes: Vec<Value> = episodes
            .into_iter()
            .filter(|episode| {
                episode
                    .get("episodeNumber")
                    .and_then(Value::as_i64)
                    .is_some_and(|number| number > 0)
                    && episode
                        .get("seasonNumber")
                        .and_then(Value::as_i64)
                        .is_none_or(|number| number == season_number)
            })
            .map(|mut episode| {
                episode["seasonNumber"] = json!(season_number);
                episode
            })
            .collect();
        if episodes.is_empty() {
            continue;
        }
        if super::episodes::sync_metadata_manifest(
            pool,
            show.id,
            &provider.id,
            &external_id,
            &detail,
            season_number,
            &episodes,
            mysql,
        )
        .await
        .is_err()
        {
            failures.push(json!({"providerId":provider.id,"code":"METADATA_PERSIST_FAILED","message":"Metadata catalog update failed"}));
            continue;
        }
        let (numbers, max_episode, coverage_gap_episode) = manifest_coverage(&episodes);
        let observed = observed_files
            .iter()
            .filter_map(episode_key)
            .filter(|(season, _)| *season == season_number)
            .map(|(_, episode)| episode)
            .max()
            .unwrap_or(0);
        let next_required = if current_season > 0 && current_season < season_number {
            1
        } else {
            current_episode.max(0) + 1
        };
        let required_episode = observed.max(next_required);
        let sufficient = max_episode >= required_episode
            && coverage_gap_episode.is_none()
            && numbers.contains(&required_episode);
        let manifest = Manifest {
            provider_id: Some(provider.id.clone()),
            season_number: Some(season_number),
            episodes,
            max_episode,
            sufficient,
            required_episode,
            coverage_gap_episode,
            failures: failures.clone(),
        };
        if sufficient {
            return manifest;
        }
        if best_insufficient.is_none() {
            best_insufficient = Some(manifest);
        }
    }
    best_insufficient.unwrap_or(Manifest {
        provider_id: None,
        season_number: None,
        episodes: vec![],
        max_episode: 0,
        sufficient: false,
        required_episode: 0,
        coverage_gap_episode: None,
        failures,
    })
}
