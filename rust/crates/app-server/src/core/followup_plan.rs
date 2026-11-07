use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedFile {
    pub id: String,
    pub name: String,
    pub season_number: i64,
    pub episode_number: i64,
    pub quality: Option<String>,
    pub size: u64,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowupPlan {
    pub season_number: i64,
    pub conflicts: Vec<i64>,
    pub missing_aired_episode_numbers: Vec<i64>,
    pub missing_future_episode_numbers: Vec<i64>,
    pub future_available_episode_numbers: Vec<i64>,
    pub requested_episode_numbers: Vec<i64>,
    pub selected_files: Vec<SelectedFile>,
    pub save_batch: Vec<SelectedFile>,
    pub estimated_bytes: u64,
    pub deferred_count: usize,
}

fn positive_integer(value: &Value) -> Option<i64> {
    let number = value.as_i64()?;
    (number > 0).then_some(number)
}

fn episode_number(name: &str) -> Option<i64> {
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b's' {
            let mut cursor = index + 1;
            let season_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor > season_start && cursor < bytes.len() && bytes[cursor] == b'e' {
                if let Some(number) = digits_at(bytes, cursor + 1) {
                    return Some(number);
                }
            }
        }
    }
    for index in 0..bytes.len() {
        if bytes[index] != b'e' {
            continue;
        }
        if index + 1 < bytes.len() && bytes[index + 1] == b'p' {
            if let Some(number) = digits_at(bytes, index + 2) {
                return Some(number);
            }
        }
        if let Some(number) = digits_at(bytes, index + 1) {
            return Some(number);
        }
    }
    if let Some(index) = name.find('第') {
        let tail = &name[index + '第'.len_utf8()..];
        let digits: String = tail.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        let rest = &tail[digits.len()..];
        if !digits.is_empty() && (rest.starts_with('集') || rest.starts_with('期')) {
            if let Ok(number) = digits.parse() {
                return Some(number);
            }
        }
    }
    let leading: String = name.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if !leading.is_empty() {
        let rest = &name[leading.len()..];
        if rest.starts_with([' ', '.', '-', '~']) || rest.starts_with('(') || rest.is_empty() {
            return leading.parse().ok();
        }
    }
    None
}

fn digits_at(bytes: &[u8], mut index: usize) -> Option<i64> {
    let start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    (index > start)
        .then(|| std::str::from_utf8(&bytes[start..index]).ok()?.parse().ok())
        .flatten()
}

fn named_season(name: &str) -> Option<i64> {
    let bytes = name.to_ascii_lowercase();
    let bytes = bytes.as_bytes();
    (0..bytes.len()).find_map(|index| {
        if bytes[index] != b's' {
            return None;
        }
        let mut cursor = index + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == index + 1 || cursor >= bytes.len() || bytes[cursor] != b'e' {
            return None;
        }
        std::str::from_utf8(&bytes[index + 1..cursor])
            .ok()?
            .parse()
            .ok()
    })
}

fn video_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".mp4", ".mkv", ".m4v", ".avi", ".mov", ".ts", ".m2ts", ".wmv", ".flv", ".webm", ".mpg",
        ".mpeg", ".rmvb", ".rm",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

pub fn video_season_number(name: &str) -> Option<i64> {
    video_file(name).then(|| named_season(name).unwrap_or(1))
}

pub fn explicit_video_season_number(name: &str) -> Option<i64> {
    video_file(name).then(|| named_season(name)).flatten()
}

pub fn video_episode_number(name: &str) -> Option<i64> {
    video_file(name).then(|| episode_number(name)).flatten()
}

fn quality(name: &str) -> Option<(String, u8)> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("2160p") || lower.contains("uhd") || lower.contains("4k") {
        Some(("4K".into(), 4))
    } else if lower.contains("1080p") || lower.contains("fhd") || lower.contains("bluray") {
        Some(("1080p".into(), 3))
    } else if lower.contains("720p") || lower.contains("hd") {
        Some(("720p".into(), 2))
    } else if lower.contains(" sd") || lower.contains("sd.") {
        Some(("SD".into(), 1))
    } else {
        None
    }
}

pub fn parse_video_episode(name: &str) -> Option<(i64, i64, Option<String>)> {
    if !video_file(name) {
        return None;
    }
    let season = named_season(name).unwrap_or(1);
    let episode = episode_number(name)?;
    (episode > 0).then(|| (season, episode, quality(name).map(|(label, _)| label)))
}

fn select_files(files: &[Value], wanted: &[i64], season: i64) -> Vec<SelectedFile> {
    let wanted: HashSet<i64> = wanted.iter().copied().collect();
    let mut best: BTreeMap<i64, (SelectedFile, u8)> = BTreeMap::new();
    for file in files {
        if file.get("isDir").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let name = file
            .get("name")
            .or_else(|| file.get("fileName"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let id = match file.get("id") {
            Some(Value::String(value)) if !value.is_empty() => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => continue,
        };
        if !video_file(name)
            || named_season(name).is_some_and(|value| value != season)
            || (named_season(name).is_none() && season != 1)
        {
            continue;
        }
        let Some(number) = episode_number(name).filter(|number| wanted.contains(number)) else {
            continue;
        };
        let raw_size = file.get("size").and_then(Value::as_u64).unwrap_or(0);
        let (quality_label, rank) =
            quality(name).map_or((None, 0), |(label, rank)| (Some(label), rank));
        let selected = SelectedFile {
            id,
            name: name.to_owned(),
            season_number: season,
            episode_number: number,
            quality: quality_label,
            size: raw_size,
            path: file.get("path").and_then(Value::as_str).map(str::to_owned),
        };
        let replace = best.get(&number).is_none_or(|(prior, prior_rank)| {
            rank > *prior_rank || (rank == *prior_rank && raw_size > prior.size)
        });
        if replace {
            best.insert(number, (selected, rank));
        }
    }
    best.into_values().map(|(file, _)| file).collect()
}

pub fn create_plan(
    season: i64,
    metadata_episodes: &[Value],
    stored_episodes: &[Value],
    shared_files: &[Value],
    today: &str,
    max_episodes: usize,
    max_bytes: u64,
) -> Option<FollowupPlan> {
    if season <= 0 || !valid_date(today) || max_episodes == 0 || max_bytes == 0 {
        return None;
    }
    let mut statuses = HashMap::new();
    for item in stored_episodes {
        let current_season = item.get("seasonNumber").and_then(Value::as_i64);
        let Some(number) = item.get("episodeNumber").and_then(positive_integer) else {
            continue;
        };
        if current_season != Some(season) {
            continue;
        }
        let status = if item.get("status").and_then(Value::as_str) == Some("conflict")
            || item.get("assetCount").and_then(Value::as_i64).unwrap_or(0) > 1
        {
            "conflict"
        } else if item.get("status").and_then(Value::as_str) == Some("present")
            || item.get("assetCount").and_then(Value::as_i64).unwrap_or(0) == 1
        {
            "present"
        } else {
            "missing"
        };
        statuses.insert(number, status);
    }
    let mut expected = BTreeMap::new();
    for item in metadata_episodes {
        let Some(number) = item.get("episodeNumber").and_then(positive_integer) else {
            continue;
        };
        let episode_season = item
            .get("seasonNumber")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .unwrap_or(season);
        if episode_season != season {
            continue;
        }
        let air_date = item
            .get("airDate")
            .and_then(Value::as_str)
            .filter(|value| value.is_ascii() && value.len() >= 10 && valid_date(&value[..10]))
            .map(|value| &value[..10]);
        let future = air_date.is_some_and(|date| date > today);
        expected.insert(
            number,
            (statuses.get(&number).copied().unwrap_or("missing"), future),
        );
    }
    let conflicts = expected
        .iter()
        .filter_map(|(number, (status, _))| (*status == "conflict").then_some(*number))
        .collect::<Vec<_>>();
    let aired = expected
        .iter()
        .filter_map(|(number, (status, future))| {
            (*status == "missing" && !future).then_some(*number)
        })
        .collect::<Vec<_>>();
    let future_missing = expected
        .iter()
        .filter_map(|(number, (status, future))| {
            (*status == "missing" && *future).then_some(*number)
        })
        .collect::<Vec<_>>();
    let available_future = select_files(shared_files, &future_missing, season)
        .into_iter()
        .map(|file| file.episode_number)
        .collect::<Vec<_>>();
    let requested: Vec<i64> = aired
        .iter()
        .chain(&available_future)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let selected = select_files(shared_files, &requested, season);
    let mut save_batch = Vec::new();
    let mut estimated_bytes = 0_u64;
    for file in &selected {
        if file.size == 0 {
            continue;
        }
        if save_batch.len() >= max_episodes
            || (!save_batch.is_empty() && estimated_bytes.saturating_add(file.size) > max_bytes)
        {
            break;
        }
        estimated_bytes = estimated_bytes.saturating_add(file.size);
        save_batch.push(file.clone());
    }
    Some(FollowupPlan {
        season_number: season,
        conflicts,
        missing_aired_episode_numbers: aired,
        missing_future_episode_numbers: future_missing,
        future_available_episode_numbers: available_future,
        requested_episode_numbers: requested,
        selected_files: selected.clone(),
        deferred_count: selected.len().saturating_sub(save_batch.len()),
        save_batch,
        estimated_bytes,
    })
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}
