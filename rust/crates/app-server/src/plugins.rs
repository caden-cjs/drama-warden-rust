use std::{
    collections::{HashMap, HashSet},
    env, io,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use drama_warden_plugin_protocol::{PluginManifest, PluginRequest, PluginResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, RwLock},
    time::timeout,
};
use url::Url;

const MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct PluginCommandConfig {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub label: String,
    pub kind: String,
    pub version: String,
    pub api_version: u16,
    pub capabilities: Vec<String>,
    pub capability_domains: HashMap<String, Vec<String>>,
    pub config_schema: drama_warden_plugin_protocol::ConfigSchema,
    pub storage_contract: Option<Value>,
    pub subscriptions: Vec<String>,
    pub enabled: bool,
    pub order: usize,
}

#[derive(Debug)]
pub struct PluginFailure {
    pub code: String,
    pub side_effect_may_have_completed: bool,
}

impl PluginFailure {
    fn new(code: &str) -> Self {
        Self {
            code: stable_code(code),
            side_effect_may_have_completed: false,
        }
    }
    fn uncertain(code: &str) -> Self {
        Self {
            code: stable_code(code),
            side_effect_may_have_completed: true,
        }
    }
}

impl axum::response::IntoResponse for PluginFailure {
    fn into_response(self) -> axum::response::Response {
        let status = match self.code.as_str() {
            "PLUGIN_NOT_FOUND" => axum::http::StatusCode::NOT_FOUND,
            "PLUGIN_DISABLED" => axum::http::StatusCode::CONFLICT,
            "PLUGIN_CAPABILITY_UNSUPPORTED" => axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLUGIN_TIMEOUT" | "PLUGIN_IO_ERROR" => axum::http::StatusCode::BAD_GATEWAY,
            _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            axum::Json(json!({ "error": "plugin operation failed", "code": self.code })),
        )
            .into_response()
    }
}

fn stable_code(code: &str) -> String {
    if code.len() <= 64
        && !code.is_empty()
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        code.to_owned()
    } else {
        "PLUGIN_ERROR".to_owned()
    }
}

struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
}

impl PluginProcess {
    async fn spawn(config: &PluginCommandConfig) -> Result<Self, PluginFailure> {
        if config.command.trim().is_empty() {
            return Err(PluginFailure::new("PLUGIN_COMMAND_INVALID"));
        }
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command
            .spawn()
            .map_err(|_| PluginFailure::new("PLUGIN_START_FAILED"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PluginFailure::new("PLUGIN_START_FAILED"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PluginFailure::new("PLUGIN_START_FAILED"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            sequence: 0,
        })
    }

    async fn request(
        &mut self,
        method: &str,
        input: Value,
        correlation_id: Option<String>,
        timeout_ms: u64,
    ) -> Result<Value, PluginFailure> {
        self.sequence = self.sequence.saturating_add(1);
        let request_id = format!("{}", self.sequence);
        let request = PluginRequest {
            request_id: request_id.clone(),
            method: method.to_owned(),
            input,
            correlation_id,
        };
        let mut frame = serde_json::to_vec(&request)
            .map_err(|_| PluginFailure::new("PLUGIN_PROTOCOL_ERROR"))?;
        if frame.len() > MAX_FRAME_BYTES {
            return Err(PluginFailure::new("PLUGIN_REQUEST_TOO_LARGE"));
        }
        frame.push(b'\n');
        self.stdin
            .write_all(&frame)
            .await
            .and_then(|_| Ok(()))
            .map_err(|_| PluginFailure::uncertain("PLUGIN_IO_ERROR"))?;
        self.stdin
            .flush()
            .await
            .map_err(|_| PluginFailure::uncertain("PLUGIN_IO_ERROR"))?;

        let bytes = match timeout(
            Duration::from_millis(timeout_ms),
            read_bounded_line(&mut self.stdout),
        )
        .await
        {
            Err(_) => {
                let _ = self.child.start_kill();
                return Err(PluginFailure::uncertain("PLUGIN_TIMEOUT"));
            }
            Ok(Err(_)) => {
                let _ = self.child.start_kill();
                return Err(PluginFailure::uncertain("PLUGIN_IO_ERROR"));
            }
            Ok(Ok(bytes)) => bytes,
        };
        let response: PluginResponse = serde_json::from_slice(&bytes)
            .map_err(|_| PluginFailure::uncertain("PLUGIN_PROTOCOL_ERROR"))?;
        if response.request_id != request_id {
            let _ = self.child.start_kill();
            return Err(PluginFailure::uncertain("PLUGIN_PROTOCOL_ERROR"));
        }
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => {
                let mut failure = PluginFailure::new(&error.code);
                failure.side_effect_may_have_completed = error.side_effect_may_have_completed;
                Err(failure)
            }
            _ => Err(PluginFailure::uncertain("PLUGIN_PROTOCOL_ERROR")),
        }
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(512);
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if result.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "plugin closed stdout",
                ));
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "plugin response was not newline terminated",
            ));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(available.len(), |index| index + 1);
        if result.len().saturating_add(length) > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "plugin response exceeded frame limit",
            ));
        }
        result.extend_from_slice(&available[..length]);
        reader.consume(length);
        if newline.is_some() {
            result.pop();
            if result.last() == Some(&b'\r') {
                result.pop();
            }
            return Ok(result);
        }
    }
}

struct PluginEntry {
    manifest: PluginManifest,
    process: Mutex<PluginProcess>,
    config: Value,
    timeout_ms: u64,
    enabled: AtomicBool,
    order: AtomicUsize,
}

#[derive(Clone)]
pub struct PluginHost {
    entries: Arc<RwLock<HashMap<String, Arc<PluginEntry>>>>,
    audit_pool: AnyPool,
    mysql: bool,
    settings_queue: Arc<Mutex<()>>,
}

fn plugin_prefix(id: &str) -> String {
    format!(
        "DRAMA_WARDEN_PLUGIN_{}_",
        id.replace('-', "_").to_ascii_uppercase()
    )
}

fn env_field(prefix: &str, key: &str) -> String {
    let mut normalized = String::new();
    let mut previous_lower_or_digit = false;
    for character in key.chars() {
        if character.is_ascii_uppercase() && previous_lower_or_digit {
            normalized.push('_');
        }
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_uppercase());
        } else {
            normalized.push('_');
        }
        previous_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
    }
    format!("{prefix}{normalized}")
}

fn compatibility_env_field(plugin_id: &str, key: &str) -> Option<&'static str> {
    match (plugin_id, key) {
        ("local-storage", "storageRoot") => Some("LOCAL_STORAGE_ROOT"),
        ("local-storage", "shareRoot") | ("local-resource", "shareRoot") => {
            Some("LOCAL_STORAGE_SHARE_ROOT")
        }
        ("quark", "cliPath") => Some("QUARK_CLI_PATH"),
        ("quark", "homeDirectory") => Some("HOME"),
        ("quark", "stateDirectory") => Some("QUARK_STATE_DIR"),
        ("quark", "recycleDirectory") => Some("QUARK_RECYCLE_DIR"),
        ("pan115", "cliPath") => Some("PAN115_CLI_PATH"),
        ("pan115", "homeDirectory") => Some("HOME"),
        ("pan115", "cookie") => Some("PAN115_COOKIE"),
        ("pan115", "minIntervalMs") => Some("PAN115_MIN_INTERVAL_MS"),
        ("pan115", "wafCooldownMs") => Some("PAN115_WAF_COOLDOWN_MS"),
        ("smartstrm", "baseUrl") => Some("SMARTSTRM_BASE_URL"),
        ("smartstrm", "webhookToken") => Some("SMARTSTRM_WEBHOOK_TOKEN"),
        ("smartstrm", "taskName") => Some("SMARTSTRM_TASK_NAME"),
        ("feishu-notification", "webhookUrl") => Some("FEISHU_WEBHOOK_URL"),
        ("feishu-notification", "webhookSecret") => Some("FEISHU_WEBHOOK_SECRET"),
        ("feishu-notification", "eventWebhookUrl") => Some("UPDATE_EVENT_WEBHOOK_URL"),
        ("feishu-notification", "eventWebhookToken") => Some("UPDATE_EVENT_WEBHOOK_TOKEN"),
        ("cloudsaver", "cloudsaverUrl") => Some("CLOUDSAVER_URL"),
        ("cloudsaver", "cloudsaverUser") => Some("CLOUDSAVER_USER"),
        ("cloudsaver", "cloudsaverPassword") => Some("CLOUDSAVER_PASS"),
        ("wps-query", "wpsQueryUrl") => Some("WPS_QUERY_URL"),
        ("opensubtitles", "apiKey") => Some("OPENSUBTITLES_API_KEY"),
        ("opensubtitles", "userAgent") => Some("OPENSUBTITLES_USER_AGENT"),
        _ => None,
    }
}

fn plugin_config(manifest: &PluginManifest) -> Result<Value, PluginFailure> {
    let prefix = plugin_prefix(&manifest.id);
    let variables: HashMap<String, String> = env::vars().collect();
    let allowed: HashSet<String> = manifest
        .config_schema
        .fields
        .iter()
        .map(|field| env_field(&prefix, &field.key))
        .collect();
    if variables
        .keys()
        .any(|key| key.starts_with(&prefix) && !allowed.contains(key))
    {
        return Err(PluginFailure::new("PLUGIN_CONFIG_INVALID"));
    }
    let mut config = serde_json::Map::new();
    for field in &manifest.config_schema.fields {
        let variable = env_field(&prefix, &field.key);
        let value = variables
            .get(&variable)
            .or_else(|| {
                compatibility_env_field(&manifest.id, &field.key)
                    .and_then(|name| variables.get(name))
            })
            .or_else(|| {
                if manifest.id == "tmdb" && field.key == "apiKey" {
                    variables.get("TMDB_API_KEY")
                } else {
                    None
                }
            });
        if value.is_none_or(|value| value.trim().is_empty()) {
            if field.required {
                return Err(PluginFailure::new("PLUGIN_CONFIG_REQUIRED"));
            }
            continue;
        }
        let value = value.unwrap();
        if field.field_type == "url" {
            let parsed =
                Url::parse(value).map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
            if !["http", "https"].contains(&parsed.scheme()) {
                return Err(PluginFailure::new("PLUGIN_CONFIG_INVALID"));
            }
        }
        config.insert(field.key.clone(), Value::String(value.clone()));
    }
    Ok(Value::Object(config))
}

fn public_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl PluginHost {
    pub fn new(audit_pool: AnyPool, mysql: bool) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            audit_pool,
            mysql,
            settings_queue: Arc::new(Mutex::new(())),
        }
    }

    pub async fn load_configured(&self) -> Result<(), PluginFailure> {
        if env::var("DRAMA_WARDEN_PLUGINS").is_ok_and(|value| !value.trim().is_empty()) {
            return Err(PluginFailure::new("PLUGIN_MODULE_LOADING_NOT_MIGRATED"));
        }
        let raw = env::var("DRAMA_WARDEN_PLUGIN_COMMANDS").unwrap_or_default();
        let executable = env::current_exe()
            .map_err(|_| PluginFailure::new("PLUGIN_COMMAND_CONFIG_INVALID"))?
            .to_string_lossy()
            .into_owned();
        let mut commands = vec![PluginCommandConfig {
            command: executable.clone(),
            args: vec!["--plugin".to_owned(), "tvmaze".to_owned()],
            timeout_ms: None,
        }];
        if env::var("TMDB_API_KEY").is_ok_and(|value| !value.trim().is_empty())
            || env::var("DRAMA_WARDEN_PLUGIN_TMDB_API_KEY")
                .is_ok_and(|value| !value.trim().is_empty())
        {
            commands.push(PluginCommandConfig {
                command: executable,
                args: vec!["--plugin".to_owned(), "tmdb".to_owned()],
                timeout_ms: None,
            });
        }
        if raw.trim().is_empty() {
            let node_bridge = |id: &str| PluginCommandConfig {
                command: "node".to_owned(),
                args: vec![
                    "backend/plugins/node-protocol-bridge.mjs".to_owned(),
                    id.to_owned(),
                ],
                timeout_ms: None,
            };
            let configured =
                |name: &str| env::var(name).is_ok_and(|value| !value.trim().is_empty());
            if env::var("LOCAL_STORAGE_ENABLED").is_ok_and(|value| value == "true") {
                commands.push(node_bridge("local-storage"));
                commands.push(node_bridge("local-resource"));
            }
            if configured("QUARK_CLI_PATH") {
                commands.push(node_bridge("quark"));
            }
            if configured("PAN115_CLI_PATH") || configured("PAN115_COOKIE") {
                commands.push(node_bridge("pan115"));
            }
            if configured("CLOUDSAVER_URL")
                && configured("CLOUDSAVER_USER")
                && configured("CLOUDSAVER_PASS")
            {
                commands.push(node_bridge("cloudsaver"));
            }
            if configured("SMARTSTRM_BASE_URL") && configured("SMARTSTRM_WEBHOOK_TOKEN") {
                commands.push(node_bridge("smartstrm"));
            }
            if configured("WPS_QUERY_URL") {
                commands.push(node_bridge("wps-query"));
            }
            if configured("FEISHU_WEBHOOK_URL") || configured("UPDATE_EVENT_WEBHOOK_URL") {
                commands.push(node_bridge("feishu-notification"));
            }
            if configured("OPENSUBTITLES_API_KEY") {
                commands.push(node_bridge("opensubtitles"));
            }
            if env::var("SUBTITLE_ASSRT_ENABLED").is_ok_and(|value| value == "true") {
                commands.push(node_bridge("assrt"));
            }
        }
        if !raw.trim().is_empty() {
            let extra: Vec<PluginCommandConfig> = serde_json::from_str(&raw)
                .map_err(|_| PluginFailure::new("PLUGIN_COMMAND_CONFIG_INVALID"))?;
            commands.extend(extra);
        }
        if commands.len() > 128
            || commands.iter().any(|item| {
                item.args.len() > 128
                    || item.command.len() > 4096
                    || item.args.iter().any(|arg| arg.len() > 8192)
            })
        {
            return Err(PluginFailure::new("PLUGIN_COMMAND_CONFIG_INVALID"));
        }
        for (order, config) in commands.iter().enumerate() {
            self.load_one(config, order).await?;
        }
        self.restore_settings().await?;
        Ok(())
    }

    async fn load_one(
        &self,
        config: &PluginCommandConfig,
        order: usize,
    ) -> Result<(), PluginFailure> {
        let timeout_ms = config
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(100, 120_000);
        let mut process = PluginProcess::spawn(config).await?;
        let manifest_value = process
            .request("manifest", json!({}), None, timeout_ms)
            .await?;
        let manifest: PluginManifest = serde_json::from_value(manifest_value)
            .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
        manifest
            .validate()
            .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
        if !public_id_valid(&manifest.id) {
            return Err(PluginFailure::new("PLUGIN_MANIFEST_INVALID"));
        }
        let config_value = plugin_config(&manifest)?;
        let entry = Arc::new(PluginEntry {
            manifest: manifest.clone(),
            process: Mutex::new(process),
            config: config_value,
            timeout_ms,
            enabled: AtomicBool::new(true),
            order: AtomicUsize::new(order),
        });
        let mut entries = self.entries.write().await;
        if entries.contains_key(&manifest.id) {
            return Err(PluginFailure::new("PLUGIN_DUPLICATE_ID"));
        }
        entries.insert(manifest.id.clone(), entry);
        Ok(())
    }

    pub async fn list(&self) -> Vec<PluginSummary> {
        let entries = self.entries.read().await;
        let mut result: Vec<_> = entries
            .values()
            .map(|entry| PluginSummary {
                id: entry.manifest.id.clone(),
                name: entry.manifest.name.clone(),
                label: entry
                    .manifest
                    .label
                    .clone()
                    .unwrap_or_else(|| entry.manifest.name.clone()),
                kind: entry.manifest.kind.clone(),
                version: entry.manifest.version.clone(),
                api_version: entry.manifest.api_version,
                capabilities: entry
                    .manifest
                    .capabilities
                    .iter()
                    .map(|item| item.name.clone())
                    .collect(),
                capability_domains: entry
                    .manifest
                    .capabilities
                    .iter()
                    .map(|item| (item.name.clone(), vec![item.domain.clone()]))
                    .collect(),
                config_schema: entry.manifest.config_schema.clone(),
                storage_contract: entry.manifest.storage_contract.clone(),
                subscriptions: entry.manifest.subscriptions.clone(),
                enabled: entry.enabled.load(Ordering::SeqCst),
                order: entry.order.load(Ordering::SeqCst),
            })
            .collect();
        result.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        result
    }

    pub async fn invoke(
        &self,
        id: &str,
        capability: &str,
        input: Value,
        source: &str,
        correlation_id: Option<String>,
    ) -> Result<Value, PluginFailure> {
        let entry = self.entry(id).await?;
        if !entry.enabled.load(Ordering::SeqCst) {
            return Err(PluginFailure::new("PLUGIN_DISABLED"));
        }
        if !entry
            .manifest
            .capabilities
            .iter()
            .any(|item| item.name == capability)
        {
            return Err(PluginFailure::new("PLUGIN_CAPABILITY_UNSUPPORTED"));
        }
        let started = Instant::now();
        let mut process = entry.process.lock().await;
        if !entry.enabled.load(Ordering::SeqCst) {
            return Err(PluginFailure::new("PLUGIN_DISABLED"));
        }
        let result = process
            .request(
                "invoke",
                json!({
                    "capability": capability,
                    "input": input,
                    "context": {"source": source, "correlationId": correlation_id}
                }),
                correlation_id.clone(),
                entry.timeout_ms,
            )
            .await;
        let failure_code = result.as_ref().err().map(|error| error.code.clone());
        let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
        let source = if source.trim().is_empty() {
            "application"
        } else {
            source
        };
        let audit = sqlx::query("INSERT INTO plugin_call_log (plugin_id, capability, source, correlation_id, ok, error_code, duration_ms) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&entry.manifest.id).bind(capability).bind(source).bind(correlation_id)
            .bind(i64::from(result.is_ok())).bind(failure_code).bind(duration_ms)
            .execute(&self.audit_pool).await;
        if audit.is_err() && result.is_ok() {
            return Err(PluginFailure::uncertain("PLUGIN_AUDIT_FAILED"));
        }
        result
    }

    pub async fn initialize_all(&self) -> Result<(), PluginFailure> {
        let entries: Vec<_> = self.entries.read().await.values().cloned().collect();
        for entry in entries {
            if entry.enabled.load(Ordering::SeqCst) {
                entry
                    .process
                    .lock()
                    .await
                    .request(
                        "initialize",
                        json!({"config": entry.config}),
                        None,
                        entry.timeout_ms,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn publish_event(&self, event: Value) -> Vec<Value> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if event_type.is_empty() {
            return Vec::new();
        }
        let correlation_id = event
            .get("correlationId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let entries: Vec<_> = self
            .entries
            .read()
            .await
            .values()
            .filter(|entry| {
                entry.enabled.load(Ordering::SeqCst)
                    && entry
                        .manifest
                        .subscriptions
                        .iter()
                        .any(|subscription| subscription == "*" || subscription == &event_type)
            })
            .cloned()
            .collect();
        let mut outcomes = Vec::with_capacity(entries.len());
        for entry in entries {
            let started = Instant::now();
            let result = entry
                .process
                .lock()
                .await
                .request(
                    "onEvent",
                    json!({"event":event.clone(),"context":{"source":"workflow-event","correlationId":correlation_id.clone()}}),
                    correlation_id.clone(),
                    entry.timeout_ms,
                )
                .await;
            let error_code = result.as_ref().err().map(|failure| failure.code.clone());
            let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
            let audit = sqlx::query("INSERT INTO plugin_call_log (plugin_id, capability, source, correlation_id, ok, error_code, duration_ms) VALUES (?, 'onEvent', 'workflow-event', ?, ?, ?, ?)")
                .bind(&entry.manifest.id)
                .bind(correlation_id.as_deref())
                .bind(i64::from(result.is_ok()))
                .bind(error_code.as_deref())
                .bind(duration_ms)
                .execute(&self.audit_pool)
                .await;
            if audit.is_err() {
                tracing::warn!(plugin_id = %entry.manifest.id, "event plugin audit write failed");
            }
            outcomes.push(json!({
                "pluginId":entry.manifest.id,
                "ok":result.is_ok(),
                "errorCode":error_code,
            }));
        }
        outcomes
    }

    pub async fn reset(&self, id: &str) -> Result<(), PluginFailure> {
        let _guard = self.settings_queue.lock().await;
        let entry = self.entry(id).await?;
        entry
            .process
            .lock()
            .await
            .request("reset", json!({}), None, entry.timeout_ms)
            .await?;
        Ok(())
    }

    pub async fn monitor(&self, id: &str) -> Result<Value, PluginFailure> {
        let entry = self.entry(id).await?;
        let response = entry
            .process
            .lock()
            .await
            .request("monitor", json!({}), None, entry.timeout_ms)
            .await;
        let raw = response.as_ref().ok();
        let ok = raw
            .and_then(|value| value.get("ok"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut result = json!({
            "pluginId": id,
            "enabled": entry.enabled.load(Ordering::SeqCst),
            "ok": ok,
        });
        if let Some(configured) = raw
            .and_then(|value| value.get("configured"))
            .and_then(Value::as_bool)
        {
            result["configured"] = json!(configured);
        }
        if let Some(authorized) = raw
            .and_then(|value| value.get("authorized"))
            .and_then(Value::as_bool)
        {
            result["authorized"] = json!(authorized);
        }
        if !ok {
            let code = raw
                .and_then(|value| value.get("errorCode"))
                .and_then(Value::as_str)
                .map(stable_code)
                .or_else(|| response.err().map(|error| error.code.clone()))
                .unwrap_or_else(|| "PLUGIN_ERROR".to_owned());
            result["errorCode"] = json!(code);
        }
        Ok(result)
    }

    async fn entry(&self, id: &str) -> Result<Arc<PluginEntry>, PluginFailure> {
        self.entries
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| PluginFailure::new("PLUGIN_NOT_FOUND"))
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), PluginFailure> {
        let _guard = self.settings_queue.lock().await;
        let entry = self.entry(id).await?;
        let mut settings = self.read_settings().await?;
        if entry.enabled.load(Ordering::SeqCst) == enabled {
            return Ok(());
        }
        if !enabled {
            settings.entry(id.to_owned()).or_insert_with(|| json!({}))["enabled"] = json!(false);
            self.write_settings(&settings).await?;
            entry.enabled.store(false, Ordering::SeqCst);
            entry
                .process
                .lock()
                .await
                .request("reset", json!({}), None, entry.timeout_ms)
                .await
                .map_err(|_| PluginFailure::new("PLUGIN_RESET_FAILED"))?;
        } else {
            entry
                .process
                .lock()
                .await
                .request(
                    "initialize",
                    json!({"config": entry.config}),
                    None,
                    entry.timeout_ms,
                )
                .await?;
            settings.entry(id.to_owned()).or_insert_with(|| json!({}))["enabled"] = json!(true);
            if let Err(error) = self.write_settings(&settings).await {
                let _ = entry
                    .process
                    .lock()
                    .await
                    .request("reset", json!({}), None, entry.timeout_ms)
                    .await;
                return Err(error);
            }
            entry.enabled.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    pub async fn move_plugin(
        &self,
        id: &str,
        direction: &str,
    ) -> Result<Vec<PluginSummary>, PluginFailure> {
        if !["up", "down"].contains(&direction) {
            return Err(PluginFailure::new("PLUGIN_ORDER_INVALID"));
        }
        let _guard = self.settings_queue.lock().await;
        let selected = self.entry(id).await?;
        let mut peers: Vec<_> = self
            .entries
            .read()
            .await
            .values()
            .filter(|entry| entry.manifest.kind == selected.manifest.kind)
            .cloned()
            .collect();
        peers.sort_by(|left, right| {
            left.order
                .load(Ordering::SeqCst)
                .cmp(&right.order.load(Ordering::SeqCst))
                .then_with(|| left.manifest.id.cmp(&right.manifest.id))
        });
        let index = peers
            .iter()
            .position(|entry| entry.manifest.id == id)
            .ok_or_else(|| PluginFailure::new("PLUGIN_NOT_FOUND"))?;
        let target = if direction == "up" {
            index.checked_sub(1)
        } else if index + 1 < peers.len() {
            Some(index + 1)
        } else {
            None
        };
        let Some(target) = target else {
            return Ok(self.list().await);
        };
        peers.swap(index, target);
        let mut settings = self.read_settings().await?;
        for (order, entry) in peers.iter().enumerate() {
            settings
                .entry(entry.manifest.id.clone())
                .or_insert_with(|| json!({}))["order"] = json!(order);
        }
        self.write_settings(&settings).await?;
        for (order, entry) in peers.iter().enumerate() {
            entry.order.store(order, Ordering::SeqCst);
        }
        Ok(self.list().await)
    }

    async fn restore_settings(&self) -> Result<(), PluginFailure> {
        let settings = self.read_settings().await?;
        let entries = self.entries.read().await;
        for (id, value) in settings {
            let Some(entry) = entries.get(&id) else {
                continue;
            };
            if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                entry.enabled.store(enabled, Ordering::SeqCst);
            }
            if let Some(order) = value
                .get("order")
                .and_then(Value::as_u64)
                .filter(|order| *order <= 999)
            {
                entry.order.store(order as usize, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    async fn read_settings(&self) -> Result<serde_json::Map<String, Value>, PluginFailure> {
        let row = sqlx::query("SELECT CAST(value_json AS CHAR) AS value_json FROM application_setting WHERE setting_key='plugins'")
            .fetch_optional(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_SETTINGS_READ_FAILED"))?;
        let Some(row) = row else {
            return Ok(serde_json::Map::new());
        };
        let value = match row.try_get::<String, _>("value_json") {
            Ok(value) => value,
            Err(_) => {
                let bytes: Vec<u8> = row
                    .try_get("value_json")
                    .map_err(|_| PluginFailure::new("PLUGIN_SETTINGS_INVALID"))?;
                String::from_utf8(bytes)
                    .map_err(|_| PluginFailure::new("PLUGIN_SETTINGS_INVALID"))?
            }
        };
        let parsed: Value = serde_json::from_str(&value)
            .map_err(|_| PluginFailure::new("PLUGIN_SETTINGS_INVALID"))?;
        parsed
            .as_object()
            .cloned()
            .ok_or_else(|| PluginFailure::new("PLUGIN_SETTINGS_INVALID"))
    }

    async fn write_settings(
        &self,
        settings: &serde_json::Map<String, Value>,
    ) -> Result<(), PluginFailure> {
        let value = Value::Object(settings.clone()).to_string();
        let query = if self.mysql {
            "INSERT INTO application_setting (setting_key, value_json) VALUES ('plugins', ?) ON DUPLICATE KEY UPDATE value_json=VALUES(value_json), updated_at=CURRENT_TIMESTAMP(3)"
        } else {
            "INSERT INTO application_setting (setting_key, value_json) VALUES ('plugins', ?) ON CONFLICT(setting_key) DO UPDATE SET value_json=excluded.value_json, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')"
        };
        sqlx::query(query)
            .bind(value)
            .execute(&self.audit_pool)
            .await
            .map_err(|_| PluginFailure::new("PLUGIN_SETTINGS_SAVE_FAILED"))?;
        Ok(())
    }
}

#[cfg(test)]
mod node_bridge_manifest_tests {
    use std::{
        io::Write,
        path::PathBuf,
        process::{Command, Stdio},
    };

    use drama_warden_plugin_protocol::{PluginManifest, PluginResponse};

    #[test]
    fn bundled_node_adapter_manifests_satisfy_the_rust_plugin_contract() {
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping bridge manifest contract check: node is unavailable");
            return;
        }
        let bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../backend/plugins/node-protocol-bridge.mjs");
        for id in [
            "cloudsaver",
            "wps-query",
            "local-storage",
            "local-resource",
            "quark",
            "pan115",
            "smartstrm",
            "feishu-notification",
            "opensubtitles",
            "assrt",
        ] {
            let mut child = Command::new("node")
                .arg(&bridge)
                .arg(id)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("could not start Node bridge for {id}: {error}"));
            child
                .stdin
                .take()
                .expect("bridge stdin")
                .write_all(b"{\"requestId\":\"manifest-test\",\"method\":\"manifest\"}\n")
                .expect("write manifest request");
            let output = child.wait_with_output().expect("read manifest response");
            assert!(output.status.success(), "bridge exited for {id}");
            let response: PluginResponse =
                serde_json::from_slice(&output.stdout).expect("valid JSON Lines response");
            assert_eq!(response.request_id, "manifest-test", "request ID for {id}");
            let manifest: PluginManifest =
                serde_json::from_value(response.result.expect("manifest result"))
                    .unwrap_or_else(|error| panic!("invalid manifest for {id}: {error}"));
            assert_eq!(manifest.id, id);
            manifest
                .validate()
                .unwrap_or_else(|error| panic!("contract rejected {id}: {error}"));
        }
    }
}
