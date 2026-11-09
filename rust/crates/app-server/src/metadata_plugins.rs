use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use drama_warden_plugin_protocol::{
    Capability, ConfigField, ConfigSchema, PROTOCOL_VERSION, PluginError, PluginManifest,
    PluginRequest, PluginResponse,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use url::Url;

const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
const TVMAZE_WINDOW: Duration = Duration::from_secs(10);
const TVMAZE_MAX_REQUESTS: usize = 19;
const SEASON_CACHE_TTL: Duration = Duration::from_secs(300);
const SEASON_CACHE_LIMIT: usize = 100;

enum ProviderKind {
    Tvmaze,
    Tmdb,
}

struct SeasonCacheEntry {
    inserted_at: Instant,
    seasons: Vec<Value>,
}

struct MetadataProvider {
    kind: ProviderKind,
    client: reqwest::Client,
    tmdb_api_key: Option<String>,
    season_cache: HashMap<String, SeasonCacheEntry>,
    tvmaze_request_times: VecDeque<Instant>,
}

impl MetadataProvider {
    fn new(kind: ProviderKind) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent("drama-warden/0.1.0 metadata-provider")
            .build()
            .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
        Ok(Self {
            kind,
            client,
            tmdb_api_key: None,
            season_cache: HashMap::new(),
            tvmaze_request_times: VecDeque::new(),
        })
    }

    fn manifest(&self) -> PluginManifest {
        let (id, name, label) = match self.kind {
            ProviderKind::Tvmaze => ("tvmaze", "TVmaze", "TVmaze"),
            ProviderKind::Tmdb => ("tmdb", "The Movie Database", "TMDB"),
        };
        let config_schema = match self.kind {
            ProviderKind::Tvmaze => ConfigSchema::default(),
            ProviderKind::Tmdb => ConfigSchema {
                version: 1,
                fields: vec![ConfigField {
                    key: "apiKey".to_owned(),
                    field_type: "password".to_owned(),
                    required: true,
                    label: Some("TMDB API 密钥".to_owned()),
                    description: Some("用于访问 TMDB API。".to_owned()),
                    secret: Some(true),
                    scope: "runtime".to_owned(),
                    default: None,
                    options: Vec::new(),
                    min: None,
                    max: None,
                    placeholder: None,
                    group: None,
                }],
            },
        };
        PluginManifest {
            protocol_version: PROTOCOL_VERSION,
            api_version: 2,
            id: id.to_owned(),
            name: name.to_owned(),
            label: Some(label.to_owned()),
            kind: "metadata-provider".to_owned(),
            version: "1.0.0".to_owned(),
            capabilities: ["search", "detail", "episodes"]
                .into_iter()
                .map(|name| Capability {
                    name: name.to_owned(),
                    domain: "metadata".to_owned(),
                    description: String::new(),
                })
                .collect(),
            subscriptions: Vec::new(),
            config_schema,
            storage_contract: None,
        }
    }

    async fn handle(&mut self, request: PluginRequest) -> PluginResponse {
        let request_id = request.request_id;
        let result = match request.method.as_str() {
            "manifest" => serde_json::to_value(self.manifest())
                .map_err(|_| "PLUGIN_PROTOCOL_ERROR".to_owned()),
            "initialize" => self.initialize(request.input.get("config").unwrap_or(&Value::Null)),
            "reset" => {
                self.season_cache.clear();
                Ok(json!({"ok": true}))
            }
            "monitor" => {
                let result = self.health().await;
                Ok(match result {
                    Ok(()) => json!({"ok": true, "configured": true}),
                    Err(code) => json!({"ok": false, "configured": true, "errorCode": code}),
                })
            }
            "invoke" => self.invoke(&request.input).await,
            "event" => Ok(json!({"ok": true})),
            _ => Err("METHOD_UNSUPPORTED".to_owned()),
        };
        match result {
            Ok(result) => PluginResponse {
                request_id,
                result: Some(result),
                error: None,
            },
            Err(code) => PluginResponse {
                request_id,
                result: None,
                error: Some(PluginError {
                    code,
                    side_effect_may_have_completed: false,
                }),
            },
        }
    }

    fn initialize(&mut self, config: &Value) -> Result<Value, String> {
        match self.kind {
            ProviderKind::Tvmaze => Ok(json!({"ok": true})),
            ProviderKind::Tmdb => {
                let key = config
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "PLUGIN_CONFIG_REQUIRED".to_owned())?;
                self.tmdb_api_key = Some(key.to_owned());
                Ok(json!({"ok": true}))
            }
        }
    }

    async fn invoke(&mut self, request: &Value) -> Result<Value, String> {
        let capability = request
            .get("capability")
            .and_then(Value::as_str)
            .ok_or_else(|| "PLUGIN_CAPABILITY_UNSUPPORTED".to_owned())?;
        let input = request.get("input").unwrap_or(&Value::Null);
        match capability {
            "search" => self.search(input).await,
            "detail" => self.detail(input).await,
            "episodes" => self.episodes(input).await,
            _ => Err("PLUGIN_CAPABILITY_UNSUPPORTED".to_owned()),
        }
    }

    async fn search(&mut self, input: &Value) -> Result<Value, String> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if query.is_empty() || query.chars().count() > 200 {
            return Err("INVALID_QUERY".to_owned());
        }
        let media_type = input
            .get("mediaType")
            .and_then(Value::as_str)
            .unwrap_or("tv");
        if !["tv", "movie"].contains(&media_type) {
            return Err("UNSUPPORTED_MEDIA_TYPE".to_owned());
        }
        let limit = requested_limit(input)?;
        let url = match self.kind {
            ProviderKind::Tvmaze => {
                if media_type != "tv" {
                    return Ok(json!([]));
                }
                let mut url = Url::parse("https://api.tvmaze.com/search/shows")
                    .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                url.query_pairs_mut().append_pair("q", query);
                url
            }
            ProviderKind::Tmdb => {
                let endpoint = if media_type == "tv" { "tv" } else { "movie" };
                let mut url =
                    Url::parse(&format!("https://api.themoviedb.org/3/search/{endpoint}"))
                        .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                url.query_pairs_mut()
                    .append_pair("query", query)
                    .append_pair("include_adult", "false")
                    .append_pair("page", "1");
                self.add_tmdb_key(&mut url)?;
                url
            }
        };
        let response = self.get_json(url).await?;
        let items = match self.kind {
            ProviderKind::Tvmaze => response
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let show = entry.get("show")?;
                    let mut normalized = tvmaze_show(show, "tv");
                    if let Some(score) = entry.get("score").and_then(Value::as_f64) {
                        normalized["score"] = json!(score);
                    }
                    Some(normalized)
                })
                .collect::<Vec<_>>(),
            ProviderKind::Tmdb => response
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|item| tmdb_show(item, media_type))
                .collect::<Vec<_>>(),
        };
        Ok(Value::Array(items.into_iter().take(limit).collect()))
    }

    async fn detail(&mut self, input: &Value) -> Result<Value, String> {
        let external_id = required_numeric_id(input.get("externalId"))?;
        let media_type = input
            .get("mediaType")
            .and_then(Value::as_str)
            .unwrap_or("tv");
        if media_type != "tv" {
            return Err("UNSUPPORTED_MEDIA_TYPE".to_owned());
        }
        match self.kind {
            ProviderKind::Tvmaze => {
                let show_url = Url::parse(&format!("https://api.tvmaze.com/shows/{external_id}"))
                    .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                let show = self.get_json(show_url).await?;
                let seasons = self.tvmaze_seasons(&external_id, true).await?;
                let mut normalized = tvmaze_show(&show, "tv");
                normalized["seasons"] = Value::Array(seasons);
                Ok(normalized)
            }
            ProviderKind::Tmdb => {
                let mut url = Url::parse(&format!("https://api.themoviedb.org/3/tv/{external_id}"))
                    .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                self.add_tmdb_key(&mut url)?;
                let show = self.get_json(url).await?;
                let mut normalized = tmdb_show(&show, "tv");
                normalized["seasons"] = Value::Array(
                    show.get("seasons")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(tmdb_season)
                        .collect(),
                );
                Ok(normalized)
            }
        }
    }

    async fn episodes(&mut self, input: &Value) -> Result<Value, String> {
        let external_id = required_numeric_id(input.get("externalId"))?;
        let season = input
            .get("seasonNumber")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .ok_or_else(|| "INVALID_SEASON_NUMBER".to_owned())?;
        let response = match self.kind {
            ProviderKind::Tvmaze => {
                let seasons = self.tvmaze_seasons(&external_id, false).await?;
                let Some(season_id) = seasons
                    .iter()
                    .find(|item| item.get("seasonNumber").and_then(Value::as_i64) == Some(season))
                    .and_then(|item| item.get("externalId"))
                    .and_then(Value::as_str)
                else {
                    return Ok(json!([]));
                };
                let url = Url::parse(&format!(
                    "https://api.tvmaze.com/seasons/{season_id}/episodes"
                ))
                .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                self.get_json(url).await?
            }
            ProviderKind::Tmdb => {
                let mut url = Url::parse(&format!(
                    "https://api.themoviedb.org/3/tv/{external_id}/season/{season}"
                ))
                .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                self.add_tmdb_key(&mut url)?;
                self.get_json(url).await?
            }
        };
        let items = match self.kind {
            ProviderKind::Tvmaze => response
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|episode| tvmaze_episode(episode, season))
                .collect(),
            ProviderKind::Tmdb => response
                .get("episodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|episode| tmdb_episode(episode, season))
                .collect(),
        };
        Ok(Value::Array(items))
    }

    async fn health(&mut self) -> Result<(), String> {
        let url = match self.kind {
            ProviderKind::Tvmaze => Url::parse("https://api.tvmaze.com/shows/1")
                .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?,
            ProviderKind::Tmdb => {
                let mut url = Url::parse("https://api.themoviedb.org/3/configuration")
                    .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
                self.add_tmdb_key(&mut url)?;
                url
            }
        };
        self.get_json(url).await.map(|_| ())
    }

    async fn tvmaze_seasons(
        &mut self,
        external_id: &str,
        refresh: bool,
    ) -> Result<Vec<Value>, String> {
        if !refresh {
            if let Some(cached) = self
                .season_cache
                .get(external_id)
                .filter(|entry| entry.inserted_at.elapsed() < SEASON_CACHE_TTL)
            {
                return Ok(cached.seasons.clone());
            }
        }
        let url = Url::parse(&format!(
            "https://api.tvmaze.com/shows/{external_id}/seasons"
        ))
        .map_err(|_| "METADATA_CLIENT_ERROR".to_owned())?;
        let response = self.get_json(url).await?;
        let seasons = response
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(tvmaze_season)
            .collect::<Vec<_>>();
        self.season_cache.remove(external_id);
        self.season_cache.insert(
            external_id.to_owned(),
            SeasonCacheEntry {
                inserted_at: Instant::now(),
                seasons: seasons.clone(),
            },
        );
        while self.season_cache.len() > SEASON_CACHE_LIMIT {
            let oldest = self
                .season_cache
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.season_cache.remove(&oldest);
            } else {
                break;
            }
        }
        Ok(seasons)
    }

    fn add_tmdb_key(&self, url: &mut Url) -> Result<(), String> {
        let key = self
            .tmdb_api_key
            .as_deref()
            .ok_or_else(|| "PLUGIN_CONFIG_REQUIRED".to_owned())?;
        url.query_pairs_mut().append_pair("api_key", key);
        Ok(())
    }

    async fn get_json(&mut self, url: Url) -> Result<Value, String> {
        if matches!(self.kind, ProviderKind::Tvmaze) {
            self.wait_for_tvmaze_capacity().await;
        }
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "METADATA_TIMEOUT".to_owned()
                } else {
                    "METADATA_NETWORK_ERROR".to_owned()
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(if status.as_u16() == 404 {
                "METADATA_NOT_FOUND".to_owned()
            } else if status.as_u16() == 429 {
                "METADATA_RATE_LIMITED".to_owned()
            } else if status.is_server_error() {
                "METADATA_UPSTREAM_ERROR".to_owned()
            } else {
                "METADATA_UPSTREAM_REJECTED".to_owned()
            });
        }
        response.json::<Value>().await.map_err(|error| {
            if error.is_timeout() {
                "METADATA_TIMEOUT".to_owned()
            } else if error.is_decode() {
                "METADATA_INVALID_RESPONSE".to_owned()
            } else {
                "METADATA_NETWORK_ERROR".to_owned()
            }
        })
    }

    async fn wait_for_tvmaze_capacity(&mut self) {
        loop {
            let now = Instant::now();
            while self
                .tvmaze_request_times
                .front()
                .is_some_and(|time| now.duration_since(*time) >= TVMAZE_WINDOW)
            {
                self.tvmaze_request_times.pop_front();
            }
            if self.tvmaze_request_times.len() < TVMAZE_MAX_REQUESTS {
                self.tvmaze_request_times.push_back(now);
                return;
            }
            let delay =
                TVMAZE_WINDOW.saturating_sub(now.duration_since(self.tvmaze_request_times[0]));
            tokio::time::sleep(delay.max(Duration::from_millis(1))).await;
        }
    }
}

fn requested_limit(input: &Value) -> Result<usize, String> {
    let limit = input.get("limit").and_then(Value::as_i64).unwrap_or(10);
    if limit < 1 {
        return Err("INVALID_LIMIT".to_owned());
    }
    Ok(limit.min(50) as usize)
}

fn required_numeric_id(value: Option<&Value>) -> Result<String, String> {
    let id = match value {
        Some(Value::String(id)) => id.trim().to_owned(),
        Some(Value::Number(id)) => id.to_string(),
        _ => return Err("INVALID_EXTERNAL_ID".to_owned()),
    };
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("INVALID_EXTERNAL_ID".to_owned());
    }
    Ok(id)
}

fn nullable_text(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(|text| json!(text))
        .unwrap_or(Value::Null)
}
fn clean_text(value: Option<&Value>) -> Value {
    let Some(text) = value.and_then(Value::as_str) else {
        return Value::Null;
    };
    let mut result = String::new();
    let mut in_tag = false;
    let mut whitespace = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            character if character.is_whitespace() => whitespace = !result.is_empty(),
            character => {
                if whitespace {
                    result.push(' ');
                    whitespace = false;
                }
                result.push(character);
            }
        }
    }
    json!(result.trim())
}
fn year(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .and_then(|date| date.get(0..4))
        .and_then(|year| year.parse::<i64>().ok())
        .map(|year| json!(year))
        .unwrap_or(Value::Null)
}
fn finite_number(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .map(|number| json!(number))
        .unwrap_or(Value::Null)
}
fn nullable_integer(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_i64)
        .map(|number| json!(number))
        .unwrap_or(Value::Null)
}

fn tvmaze_show(item: &Value, media_type: &str) -> Value {
    let id = item
        .get("id")
        .map(|id| id.to_string().trim_matches('"').to_owned())
        .unwrap_or_default();
    let first_air_date = item.get("premiered").cloned().unwrap_or(Value::Null);
    let image = item.get("image");
    json!({
        "id": id, "externalId": id, "providerId": "tvmaze", "mediaType": media_type,
        "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
        "originalName": item.get("name").and_then(Value::as_str).unwrap_or(""),
        "overview": clean_text(item.get("summary")), "firstAirDate": first_air_date,
        "year": year(item.get("premiered")), "status": nullable_text(item.get("status")),
        "language": nullable_text(item.get("language")),
        "genres": item.get("genres").filter(|value| value.is_array()).cloned().unwrap_or_else(|| json!([])),
        "posterPath": image.and_then(|value| value.get("medium")).or_else(|| image.and_then(|value| value.get("original"))).cloned().unwrap_or(Value::Null),
        "externalUrl": nullable_text(item.get("url")),
    })
}

fn tvmaze_season(item: &Value) -> Option<Value> {
    let number = item.get("number")?.as_i64()?;
    if number < 0 {
        return None;
    }
    Some(
        json!({ "seasonNumber": number, "name": nullable_text(item.get("name")), "episodeCount": nullable_integer(item.get("episodeOrder")), "airDate": nullable_text(item.get("premiereDate")), "externalId": item.get("id")?.to_string().trim_matches('"') }),
    )
}

fn tvmaze_episode(item: &Value, season: i64) -> Option<Value> {
    let number = item.get("number")?.as_i64()?;
    if number <= 0 {
        return None;
    }
    let id = item.get("id")?.to_string().trim_matches('"').to_owned();
    let image = item.get("image");
    Some(
        json!({ "id": id, "externalId": id, "seasonNumber": season, "episodeNumber": number,
        "name": nullable_text(item.get("name")), "overview": clean_text(item.get("summary")),
        "airDate": nullable_text(item.get("airdate")), "airTime": nullable_text(item.get("airtime")),
        "runtime": nullable_integer(item.get("runtime")),
        "stillPath": image.and_then(|value| value.get("original")).or_else(|| image.and_then(|value| value.get("medium"))).cloned().unwrap_or(Value::Null) }),
    )
}

fn tmdb_show(item: &Value, media_type: &str) -> Value {
    let id = item
        .get("id")
        .map(|id| id.to_string().trim_matches('"').to_owned())
        .unwrap_or_default();
    let movie = media_type == "movie";
    let title = if movie {
        item.get("title")
    } else {
        item.get("name")
    };
    let original_name = if movie {
        item.get("original_title")
    } else {
        item.get("original_name")
    };
    let first_air_date = if movie {
        item.get("release_date")
    } else {
        item.get("first_air_date")
    };
    let external_url = format!("https://www.themoviedb.org/{media_type}/{id}");
    json!({ "id": id, "externalId": id, "providerId": "tmdb", "mediaType": media_type,
        "name": title.and_then(Value::as_str).unwrap_or(""), "originalName": original_name.and_then(Value::as_str).or_else(|| title.and_then(Value::as_str)).unwrap_or(""),
        "overview": clean_text(item.get("overview")), "firstAirDate": first_air_date.cloned().unwrap_or(Value::Null),
        "year": year(first_air_date), "language": nullable_text(item.get("original_language")),
        "posterPath": nullable_text(item.get("poster_path")), "backdropPath": nullable_text(item.get("backdrop_path")),
        "voteAverage": finite_number(item.get("vote_average")), "externalUrl": external_url })
}

fn tmdb_season(item: &Value) -> Option<Value> {
    let number = item.get("season_number")?.as_i64()?;
    if number < 0 {
        return None;
    }
    Some(
        json!({"seasonNumber": number, "name": nullable_text(item.get("name")), "episodeCount": nullable_integer(item.get("episode_count")), "airDate": nullable_text(item.get("air_date")), "externalId": item.get("id")?.to_string().trim_matches('"')}),
    )
}

fn tmdb_episode(item: &Value, season: i64) -> Option<Value> {
    let number = item.get("episode_number")?.as_i64()?;
    if number <= 0 {
        return None;
    }
    let id = item.get("id")?.to_string().trim_matches('"').to_owned();
    Some(
        json!({"id": id, "externalId": id, "seasonNumber": season, "episodeNumber": number,
        "name": nullable_text(item.get("name")), "overview": clean_text(item.get("overview")),
        "airDate": nullable_text(item.get("air_date")), "runtime": nullable_integer(item.get("runtime")),
        "stillPath": nullable_text(item.get("still_path"))}),
    )
}

pub async fn serve(id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let kind = match id {
        "tvmaze" => ProviderKind::Tvmaze,
        "tmdb" => ProviderKind::Tmdb,
        _ => return Err("unknown built-in plugin".into()),
    };
    let mut plugin =
        MetadataProvider::new(kind).map_err(|code| format!("plugin start failed: {code}"))?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = input.read_until(b'\n', &mut line).await?;
        if count == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let request = match serde_json::from_slice::<PluginRequest>(&line) {
            Ok(request) => request,
            Err(_) => continue,
        };
        let response = plugin.handle(request).await;
        let frame = serde_json::to_vec(&response)?;
        output.write_all(&frame).await?;
        output.write_all(b"\n").await?;
        output.flush().await?;
    }
    Ok(())
}
