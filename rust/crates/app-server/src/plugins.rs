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
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
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
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct PluginCommandConfig {
    #[serde(default)]
    id: Option<String>,
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
    pub configured: bool,
    pub active: bool,
    pub configuration_error: Option<String>,
    pub config_revision: u64,
    pub order: usize,
}

#[derive(Clone)]
struct ResolvedConfiguration {
    values: Value,
    sources: HashMap<String, String>,
    configured: bool,
    error: Option<String>,
    revision: u64,
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
            "PLUGIN_DISABLED"
            | "PLUGIN_CONFIG_REVISION_CONFLICT"
            | "PLUGIN_CONFIG_APPLY_FAILED" => axum::http::StatusCode::CONFLICT,
            "PLUGIN_CONFIG_INVALID"
            | "PLUGIN_CONFIG_REQUIRED"
            | "PLUGIN_CONFIG_SECRET_KEY_MISSING"
            | "PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"
            | "PLUGIN_CONFIG_FIELD_SCOPE" => axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLUGIN_CONFIG_READ_FAILED" | "PLUGIN_CONFIG_STORE_FAILED" => {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
            "PLUGIN_UNAVAILABLE" => axum::http::StatusCode::SERVICE_UNAVAILABLE,
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
    config: RwLock<ResolvedConfiguration>,
    command: PluginCommandConfig,
    timeout_ms: u64,
    enabled: AtomicBool,
    active: AtomicBool,
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
            if let Some(default) = &field.default {
                config.insert(field.key.clone(), default.clone());
            }
            continue;
        }
        let value = value.unwrap();
        let parsed = match field.field_type.as_str() {
            "integer" => Value::Number(
                value
                    .parse::<i64>()
                    .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?
                    .into(),
            ),
            "boolean" => match value.to_ascii_lowercase().as_str() {
                "true" | "1" => json!(true),
                "false" | "0" => json!(false),
                _ => return Err(PluginFailure::new("PLUGIN_CONFIG_INVALID")),
            },
            _ => Value::String(value.clone()),
        };
        validate_config_value(&manifest.id, field, &parsed)?;
        config.insert(field.key.clone(), parsed);
    }
    Ok(Value::Object(config))
}

fn encryption_key() -> Result<LessSafeKey, PluginFailure> {
    let raw = env::var("PLUGIN_CONFIG_ENCRYPTION_KEY")
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_KEY_MISSING"))?;
    let bytes =
        hex::decode(raw).map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_KEY_MISSING"))?;
    let key = UnboundKey::new(&AES_256_GCM, &bytes)
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_KEY_MISSING"))?;
    Ok(LessSafeKey::new(key))
}

fn seal_secret(
    plugin_id: &str,
    field: &str,
    schema: u16,
    value: &str,
) -> Result<String, PluginFailure> {
    let key = encryption_key()?;
    let mut nonce_bytes = [0u8; 12];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_ENCRYPT_FAILED"))?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let aad = format!("{plugin_id}:{field}:{schema}");
    let mut data = value.as_bytes().to_vec();
    key.seal_in_place_append_tag(nonce, Aad::from(aad.as_bytes()), &mut data)
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_ENCRYPT_FAILED"))?;
    Ok(format!(
        "v1:{}:{}",
        hex::encode(nonce_bytes),
        hex::encode(data)
    ))
}

fn open_secret(
    plugin_id: &str,
    field: &str,
    schema: u16,
    envelope: &str,
) -> Result<String, PluginFailure> {
    let key = encryption_key()?;
    let mut parts = envelope.split(':');
    if parts.next() != Some("v1") {
        return Err(PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"));
    }
    let nonce = hex::decode(parts.next().unwrap_or_default())
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"))?;
    let mut data = hex::decode(parts.next().unwrap_or_default())
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"))?;
    if parts.next().is_some() || nonce.len() != 12 {
        return Err(PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"));
    }
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&nonce);
    let aad = format!("{plugin_id}:{field}:{schema}");
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(aad.as_bytes()),
            &mut data,
        )
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"))?;
    String::from_utf8(plain.to_vec())
        .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_SECRET_DECRYPT_FAILED"))
}

fn public_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_config_value(
    plugin_id: &str,
    field: &drama_warden_plugin_protocol::ConfigField,
    value: &Value,
) -> Result<(), PluginFailure> {
    let valid = match field.field_type.as_str() {
        "text" | "password" => value.as_str().is_some_and(|text| !text.trim().is_empty()),
        "path" => value.as_str().is_some_and(|text| {
            !text.trim().is_empty()
                && !text.contains('\0')
                && (!(matches!(plugin_id, "quark" | "pan115") && field.key == "cliPath")
                    || std::path::Path::new(text).is_absolute())
        }),
        "url" => value
            .as_str()
            .and_then(|text| Url::parse(text).ok())
            .is_some_and(|url| ["http", "https"].contains(&url.scheme())),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some_and(|number| {
            field.min.is_none_or(|min| number >= min) && field.max.is_none_or(|max| number <= max)
        }),
        "select" => value
            .as_str()
            .is_some_and(|selected| field.options.iter().any(|option| option.value == selected)),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(PluginFailure::new("PLUGIN_CONFIG_INVALID"))
    }
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
            tracing::warn!(
                "DRAMA_WARDEN_PLUGINS is ignored by the Rust host; install plugin commands through deployment configuration"
            );
        }
        let raw = env::var("DRAMA_WARDEN_PLUGIN_COMMANDS").unwrap_or_default();
        let executable = env::current_exe()
            .map_err(|_| PluginFailure::new("PLUGIN_COMMAND_CONFIG_INVALID"))?
            .to_string_lossy()
            .into_owned();
        let mut commands = vec![PluginCommandConfig {
            id: Some("tvmaze".to_owned()),
            command: executable.clone(),
            args: vec!["--plugin".to_owned(), "tvmaze".to_owned()],
            timeout_ms: None,
        }];
        commands.push(PluginCommandConfig {
            id: Some("tmdb".to_owned()),
            command: executable,
            args: vec!["--plugin".to_owned(), "tmdb".to_owned()],
            timeout_ms: None,
        });
        {
            let node_bridge = |id: &str| PluginCommandConfig {
                id: Some(id.to_owned()),
                command: "node".to_owned(),
                args: vec![
                    "backend/plugins/node-protocol-bridge.mjs".to_owned(),
                    id.to_owned(),
                ],
                // Quark resolves a virtual path by browsing each parent
                // directory through the official CLI. A complete listing of
                // a deep path can take several CLI calls, each near 20s.
                timeout_ms: None,
            };
            for id in [
                "local-storage",
                "local-resource",
                "quark",
                "pan115",
                "cloudsaver",
                "smartstrm",
                "wps-query",
                "feishu-notification",
                "opensubtitles",
                "assrt",
            ] {
                commands.push(node_bridge(id));
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
            if let Err(error) = self.load_one(config, order).await {
                tracing::warn!(plugin_id = ?config.id, code = %error.code, "plugin registration failed; continuing with remaining plugins");
            }
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
        if config.id.as_ref().is_some_and(|id| id != &manifest.id) {
            return Err(PluginFailure::new("PLUGIN_ID_MISMATCH"));
        }
        let mut config_value = plugin_config(&manifest)?;
        let mut sources = HashMap::new();
        let mut revision = 0;
        for field in &manifest.config_schema.fields {
            if config_value.get(&field.key).is_some()
                && field.default.as_ref() != config_value.get(&field.key)
            {
                sources.insert(field.key.clone(), "legacyEnvironment".to_owned());
            }
        }
        let row = sqlx::query("SELECT CAST(config_json AS CHAR) AS config_json, CAST(secrets_json AS CHAR) AS secrets_json, schema_version, revision FROM plugin_configuration WHERE plugin_id=?")
            .bind(&manifest.id).fetch_optional(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_READ_FAILED"))?;
        let mut config_error = None;
        if let Some(row) = row {
            let read_json = |row: &sqlx::any::AnyRow, key: &str| -> Result<Value, PluginFailure> {
                let text = row
                    .try_get::<String, _>(key)
                    .or_else(|_| {
                        row.try_get::<Vec<u8>, _>(key).and_then(|bytes| {
                            String::from_utf8(bytes)
                                .map_err(|_| sqlx::Error::Decode("invalid utf8".into()))
                        })
                    })
                    .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
                serde_json::from_str(&text).map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))
            };
            let stored = read_json(&row, "config_json")?;
            let secrets = read_json(&row, "secrets_json")?;
            revision = row.try_get::<i64, _>("revision").unwrap_or(0).max(0) as u64;
            let stored_schema = row.try_get::<i64, _>("schema_version").unwrap_or(1) as u16;
            if stored_schema != manifest.config_schema.version {
                config_error = Some("PLUGIN_CONFIG_MIGRATION_REQUIRED".to_owned());
            }
            for field in &manifest.config_schema.fields {
                if !field.is_secret() {
                    if let Some(value) = stored.get(&field.key) {
                        config_value[&field.key] = value.clone();
                        sources.insert(field.key.clone(), "database".to_owned());
                    }
                } else if let Some(envelope) = secrets.get(&field.key).and_then(Value::as_str) {
                    match open_secret(&manifest.id, &field.key, stored_schema, envelope) {
                        Ok(value) => {
                            config_value[&field.key] = json!(value);
                            sources.insert(field.key.clone(), "database".to_owned());
                        }
                        Err(error) => {
                            config_value.as_object_mut().unwrap().remove(&field.key);
                            config_error = Some(error.code);
                        }
                    }
                }
            }
        }
        let configured = config_error.is_none()
            && manifest.config_schema.fields.iter().all(|field| {
                !field.required
                    || config_value
                        .get(&field.key)
                        .is_some_and(|value| !value.is_null())
            });
        let has_legacy_values = !config_value
            .as_object()
            .is_none_or(|values| values.is_empty());
        let local_storage_disabled = ["local-storage", "local-resource"]
            .contains(&manifest.id.as_str())
            && env::var("LOCAL_STORAGE_ENABLED")
                .is_ok_and(|value| value.eq_ignore_ascii_case("false"));
        let default_enabled = !local_storage_disabled
            && (manifest.id == "tvmaze" || (configured && has_legacy_values));
        let entry = Arc::new(PluginEntry {
            manifest: manifest.clone(),
            process: Mutex::new(process),
            config: RwLock::new(ResolvedConfiguration {
                values: config_value,
                sources,
                configured,
                error: config_error
                    .or_else(|| (!configured).then(|| "PLUGIN_CONFIG_REQUIRED".to_owned())),
                revision,
            }),
            command: config.clone(),
            timeout_ms,
            enabled: AtomicBool::new(default_enabled),
            active: AtomicBool::new(false),
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
                configured: entry
                    .config
                    .try_read()
                    .map(|config| config.configured)
                    .unwrap_or(false),
                active: entry.active.load(Ordering::SeqCst),
                configuration_error: entry
                    .config
                    .try_read()
                    .ok()
                    .and_then(|config| config.error.clone()),
                config_revision: entry
                    .config
                    .try_read()
                    .map(|config| config.revision)
                    .unwrap_or(0),
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

    pub async fn configuration(&self, id: &str) -> Result<Value, PluginFailure> {
        let entry = self.entry(id).await?;
        let config = entry.config.read().await;
        let fields = entry
            .manifest
            .config_schema
            .fields
            .iter()
            .map(|field| {
                let value = config.values.get(&field.key);
                let source = config
                    .sources
                    .get(&field.key)
                    .cloned()
                    .or_else(|| {
                        if value.is_none() {
                            Some("unset".to_owned())
                        } else if field.scope == "deployment" {
                            Some("deployment".to_owned())
                        } else if field.default.as_ref() == value {
                            Some("manifestDefault".to_owned())
                        } else {
                            Some("legacyEnvironment".to_owned())
                        }
                    })
                    .unwrap_or_else(|| "unset".to_owned());
                let mut item = json!({
                    "key": field.key, "type": field.field_type, "label": field.label,
                    "description": field.description, "required": field.required,
                    "secret": field.is_secret(), "scope": field.scope,
                    "configured": value.is_some_and(|v| !v.is_null()),
                    "source": source,
                    "editable": field.scope == "runtime"
                });
                if !field.is_secret() {
                    if let Some(value) = value {
                        item["value"] = value.clone();
                    }
                }
                item
            })
            .collect::<Vec<_>>();
        Ok(
            json!({ "pluginId": id, "schemaVersion": entry.manifest.config_schema.version,
            "configured": config.configured, "enabled": entry.enabled.load(Ordering::SeqCst),
            "active": entry.active.load(Ordering::SeqCst), "revision": config.revision, "fields": fields }),
        )
    }

    pub async fn validate_configuration(
        &self,
        id: &str,
        patch: &Value,
    ) -> Result<Value, PluginFailure> {
        let entry = self.entry(id).await?;
        let object = patch
            .as_object()
            .ok_or_else(|| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
        let current = entry.config.read().await;
        for (key, value) in object {
            let field = entry
                .manifest
                .config_schema
                .fields
                .iter()
                .find(|field| &field.key == key)
                .ok_or_else(|| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
            if field.scope != "runtime" {
                return Err(PluginFailure::new("PLUGIN_CONFIG_FIELD_SCOPE"));
            }
            if value.is_null() {
                if field.required {
                    return Err(PluginFailure::new("PLUGIN_CONFIG_REQUIRED"));
                }
                continue;
            }
            validate_config_value(id, field, value)?;
        }
        let mut values = current.values.clone();
        for (key, value) in object {
            if value.is_null() {
                values.as_object_mut().unwrap().remove(key);
            } else {
                values[key] = value.clone();
            }
        }
        for field in &entry.manifest.config_schema.fields {
            if field.required && values.get(&field.key).is_none_or(|value| value.is_null()) {
                return Err(PluginFailure::new("PLUGIN_CONFIG_REQUIRED"));
            }
        }
        Ok(json!({"valid":true,"pluginId":id,"issues":[]}))
    }

    pub async fn save_configuration(
        &self,
        id: &str,
        patch: &Value,
        expected_revision: u64,
    ) -> Result<Value, PluginFailure> {
        self.validate_configuration(id, patch).await?;
        let _guard = self.settings_queue.lock().await;
        let entry = self.entry(id).await?;
        let mut current = entry.config.write().await;
        if current.revision != expected_revision {
            return Err(PluginFailure::new("PLUGIN_CONFIG_REVISION_CONFLICT"));
        }
        let mut values = current.values.clone();
        let base_values = plugin_config(&entry.manifest)?;
        let existing_row = sqlx::query("SELECT CAST(config_json AS CHAR) AS config_json, CAST(secrets_json AS CHAR) AS secrets_json FROM plugin_configuration WHERE plugin_id=?")
            .bind(id).fetch_optional(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        let read_json = |row: &sqlx::any::AnyRow, key: &str| -> Result<Value, PluginFailure> {
            let text = row
                .try_get::<String, _>(key)
                .or_else(|_| {
                    row.try_get::<Vec<u8>, _>(key).and_then(|bytes| {
                        String::from_utf8(bytes)
                            .map_err(|_| sqlx::Error::Decode("invalid utf8".into()))
                    })
                })
                .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
            serde_json::from_str(&text).map_err(|_| PluginFailure::new("PLUGIN_CONFIG_INVALID"))
        };
        let mut overrides = if let Some(row) = &existing_row {
            read_json(row, "config_json")?
        } else {
            json!({})
        };
        let mut secrets = existing_row
            .as_ref()
            .map(|row| read_json(row, "secrets_json"))
            .transpose()?
            .unwrap_or_else(|| json!({}))
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut safe = serde_json::Map::new();
        for (key, value) in patch.as_object().unwrap() {
            let field = entry
                .manifest
                .config_schema
                .fields
                .iter()
                .find(|field| &field.key == key)
                .unwrap();
            if field.is_secret() {
                if value.is_null() {
                    secrets.remove(key);
                    if let Some(fallback) = base_values.get(key) {
                        values[key] = fallback.clone();
                    } else {
                        values.as_object_mut().unwrap().remove(key);
                    }
                } else {
                    let text = value
                        .as_str()
                        .filter(|text| !text.trim().is_empty())
                        .ok_or_else(|| PluginFailure::new("PLUGIN_CONFIG_INVALID"))?;
                    secrets.insert(
                        key.clone(),
                        json!(seal_secret(
                            id,
                            key,
                            entry.manifest.config_schema.version,
                            text
                        )?),
                    );
                    values[key] = json!(text);
                }
            } else if value.is_null() {
                if let Some(fallback) = base_values.get(key) {
                    values[key] = fallback.clone();
                } else {
                    values.as_object_mut().unwrap().remove(key);
                }
                overrides.as_object_mut().unwrap().remove(key);
            } else {
                values[key] = value.clone();
                overrides[key] = value.clone();
            }
            safe.insert(key.clone(), json!({"key":key,"configured":values.get(key).is_some_and(|v| !v.is_null()),"source":if value.is_null() { "legacyEnvironment" } else { "database" },"secret":field.is_secret()}));
        }
        let new_revision = current.revision + 1;
        let configured = entry.manifest.config_schema.fields.iter().all(|field| {
            !field.required || values.get(&field.key).is_some_and(|value| !value.is_null())
        });
        let mut candidate = None;
        if entry.enabled.load(Ordering::SeqCst) && configured {
            let mut process = PluginProcess::spawn(&entry.command).await?;
            let manifest: PluginManifest = serde_json::from_value(
                process
                    .request("manifest", json!({}), None, entry.timeout_ms)
                    .await?,
            )
            .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
            if manifest.id != id
                || manifest.api_version != entry.manifest.api_version
                || manifest.config_schema.version != entry.manifest.config_schema.version
            {
                return Err(PluginFailure::new("PLUGIN_MANIFEST_INVALID"));
            }
            manifest
                .validate()
                .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
            process
                .request(
                    "initialize",
                    json!({"config":values}),
                    None,
                    entry.timeout_ms,
                )
                .await
                .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_APPLY_FAILED"))?;
            candidate = Some(process);
        }
        let config_json = overrides.to_string();
        let secrets_json = Value::Object(secrets).to_string();
        let field_keys = patch
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let secret_keys = field_keys
            .iter()
            .filter(|key| {
                entry
                    .manifest
                    .config_schema
                    .fields
                    .iter()
                    .any(|field| &field.key == *key && field.is_secret())
            })
            .cloned()
            .collect::<Vec<_>>();
        let exists = sqlx::query("SELECT revision FROM plugin_configuration WHERE plugin_id=?")
            .bind(id)
            .fetch_optional(&self.audit_pool)
            .await
            .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        if exists.is_some() {
            sqlx::query("UPDATE plugin_configuration SET schema_version=?, config_json=?, secrets_json=?, revision=?, updated_at=CURRENT_TIMESTAMP WHERE plugin_id=?")
                .bind(entry.manifest.config_schema.version as i64).bind(config_json).bind(secrets_json).bind(new_revision as i64).bind(id).execute(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        } else {
            sqlx::query("INSERT INTO plugin_configuration (plugin_id,schema_version,config_json,secrets_json,revision) VALUES (?,?,?,?,?)")
                .bind(id).bind(entry.manifest.config_schema.version as i64).bind(config_json).bind(secrets_json).bind(new_revision as i64).execute(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        }
        let audit = sqlx::query("INSERT INTO plugin_config_audit (plugin_id,field_keys_json,secret_field_keys_json,outcome_code,revision) VALUES (?,?,?,NULL,?)")
            .bind(id).bind(json!(field_keys).to_string()).bind(json!(secret_keys).to_string()).bind(new_revision as i64).execute(&self.audit_pool).await;
        if audit.is_err() {
            tracing::warn!(plugin_id = id, "plugin configuration audit write failed");
        }
        current.values = values;
        current.configured = configured;
        current.revision = new_revision;
        for (key, value) in patch.as_object().unwrap() {
            if value.is_null() {
                current.sources.remove(key);
            } else {
                current.sources.insert(key.clone(), "database".to_owned());
            }
        }
        if let Some(candidate) = candidate {
            *entry.process.lock().await = candidate;
            entry.active.store(true, Ordering::SeqCst);
        } else if !current.configured {
            entry.active.store(false, Ordering::SeqCst);
        }
        Ok(
            json!({"pluginId":id,"applied":entry.active.load(Ordering::SeqCst),"revision":new_revision,"fields":safe}),
        )
    }

    pub async fn reset_configuration(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<Value, PluginFailure> {
        let _guard = self.settings_queue.lock().await;
        let entry = self.entry(id).await?;
        let mut current = entry.config.write().await;
        if current.revision != expected_revision {
            return Err(PluginFailure::new("PLUGIN_CONFIG_REVISION_CONFLICT"));
        }
        let next_revision = current.revision + 1;
        let values = plugin_config(&entry.manifest)?;
        let configured = entry.manifest.config_schema.fields.iter().all(|field| {
            !field.required || values.get(&field.key).is_some_and(|value| !value.is_null())
        });
        let mut candidate = None;
        if entry.enabled.load(Ordering::SeqCst) && configured {
            let mut process = PluginProcess::spawn(&entry.command).await?;
            let manifest: PluginManifest = serde_json::from_value(
                process
                    .request("manifest", json!({}), None, entry.timeout_ms)
                    .await?,
            )
            .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
            if manifest.id != id
                || manifest.api_version != entry.manifest.api_version
                || manifest.config_schema.version != entry.manifest.config_schema.version
            {
                return Err(PluginFailure::new("PLUGIN_MANIFEST_INVALID"));
            }
            manifest
                .validate()
                .map_err(|_| PluginFailure::new("PLUGIN_MANIFEST_INVALID"))?;
            process
                .request(
                    "initialize",
                    json!({"config":values}),
                    None,
                    entry.timeout_ms,
                )
                .await
                .map_err(|_| PluginFailure::new("PLUGIN_CONFIG_APPLY_FAILED"))?;
            candidate = Some(process);
        }
        let stored = sqlx::query("UPDATE plugin_configuration SET config_json='{}', secrets_json='{}', revision=?, updated_at=CURRENT_TIMESTAMP WHERE plugin_id=?")
            .bind(next_revision as i64).bind(id).execute(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        if stored.rows_affected() == 0 {
            sqlx::query("INSERT INTO plugin_configuration (plugin_id,schema_version,config_json,secrets_json,revision) VALUES (?,?, '{}','{}',?)")
                .bind(id).bind(entry.manifest.config_schema.version as i64).bind(next_revision as i64).execute(&self.audit_pool).await.map_err(|_| PluginFailure::new("PLUGIN_CONFIG_STORE_FAILED"))?;
        }
        current.values = values;
        current.configured = configured;
        current.error = (!configured).then(|| "PLUGIN_CONFIG_REQUIRED".to_owned());
        current.sources.clear();
        current.revision = next_revision;
        let revision = current.revision;
        if let Some(candidate) = candidate {
            *entry.process.lock().await = candidate;
            entry.active.store(true, Ordering::SeqCst);
        } else {
            entry.active.store(false, Ordering::SeqCst);
        }
        let audit = sqlx::query("INSERT INTO plugin_config_audit (plugin_id,field_keys_json,secret_field_keys_json,outcome_code,revision) VALUES (?,?,?,NULL,?)")
            .bind(id).bind("[]").bind("[]").bind(revision as i64).execute(&self.audit_pool).await;
        if audit.is_err() {
            tracing::warn!(plugin_id = id, "plugin configuration audit write failed");
        }
        Ok(json!({"pluginId":id,"revision":revision,"configured":configured,"fields":[]}))
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
        if !entry.active.load(Ordering::SeqCst) {
            return Err(PluginFailure::new("PLUGIN_UNAVAILABLE"));
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
            let config = entry.config.read().await.clone();
            if entry.enabled.load(Ordering::SeqCst) && config.configured {
                let result = entry
                    .process
                    .lock()
                    .await
                    .request(
                        "initialize",
                        json!({"config": config.values}),
                        None,
                        entry.timeout_ms,
                    )
                    .await;
                match result {
                    Ok(_) => entry.active.store(true, Ordering::SeqCst),
                    Err(error) => {
                        tracing::warn!(plugin_id = %entry.manifest.id, code = %error.code, "plugin initialization failed");
                        entry.config.write().await.error = Some(error.code);
                    }
                }
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
                    && entry.active.load(Ordering::SeqCst)
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
            entry.active.store(false, Ordering::SeqCst);
            entry
                .process
                .lock()
                .await
                .request("reset", json!({}), None, entry.timeout_ms)
                .await
                .map_err(|_| PluginFailure::new("PLUGIN_RESET_FAILED"))?;
        } else {
            let config = entry.config.read().await.clone();
            if !config.configured {
                return Err(PluginFailure::new("PLUGIN_CONFIG_REQUIRED"));
            }
            entry
                .process
                .lock()
                .await
                .request(
                    "initialize",
                    json!({"config": config.values}),
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
            entry.active.store(true, Ordering::SeqCst);
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
