use serde::Deserialize;
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    server: ServerConfig,
    database: DatabaseConfig,
    storage: StorageConfig,
    indexer: IndexerConfig,
    subtitles: SubtitleConfig,
    automation: AutomationConfig,
    plugins: PluginConfig,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ServerConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    time_zone: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct DatabaseConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    driver: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    sqlite_path: Option<String>,
    mysql: MysqlConfig,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct MysqlConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    host: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    port: Option<u16>,
    #[serde(default, deserialize_with = "deserialize_present")]
    database: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    user: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    connection_limit: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    connect_attempts: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_present")]
    connect_delay_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_present")]
    connect_timeout_ms: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StorageConfig {
    local: LocalStorageConfig,
    quark: QuarkConfig,
    pan115: Pan115Config,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct LocalStorageConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    root: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    share_root: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct QuarkConfig {
    cli_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    state_directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    recycle_directory: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Pan115Config {
    cli_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    min_interval_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_present")]
    waf_cooldown_ms: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct IndexerConfig {
    smartstrm: SmartstrmConfig,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct SmartstrmConfig {
    base_url: Option<String>,
    task_name: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct SubtitleConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    worker_enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    assrt_enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    open_subtitles_user_agent: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct AutomationConfig {
    #[serde(default, deserialize_with = "deserialize_present")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present")]
    scan_interval_minutes: Option<u16>,
    #[serde(default, deserialize_with = "deserialize_present")]
    auto_process: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PluginConfig {
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    commands: Vec<PluginProcessConfig>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct PluginProcessConfig {
    command: String,
    args: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    timeout_ms: Option<u64>,
}

impl Default for PluginProcessConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            timeout_ms: None,
        }
    }
}

fn validate_string(
    value: Option<&String>,
    key: &str,
    max: usize,
    required: bool,
) -> Result<(), String> {
    let Some(value) = value else { return Ok(()) };
    if value.len() > max || value.contains('\0') || (required && value.trim().is_empty()) {
        return Err(format!("invalid Drama Warden configuration field: {key}"));
    }
    Ok(())
}

fn validate_range<T: PartialOrd + Copy>(
    value: Option<T>,
    min: T,
    max: T,
    key: &str,
) -> Result<(), String> {
    if value.is_some_and(|value| value < min || value > max) {
        return Err(format!("invalid Drama Warden configuration field: {key}"));
    }
    Ok(())
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        for (value, key, max, required) in [
            (self.server.time_zone.as_ref(), "server.timeZone", 128, true),
            (
                self.database.sqlite_path.as_ref(),
                "database.sqlitePath",
                1024,
                true,
            ),
            (
                self.database.mysql.host.as_ref(),
                "database.mysql.host",
                255,
                true,
            ),
            (
                self.database.mysql.database.as_ref(),
                "database.mysql.database",
                128,
                true,
            ),
            (
                self.database.mysql.user.as_ref(),
                "database.mysql.user",
                128,
                true,
            ),
            (
                self.storage.local.root.as_ref(),
                "storage.local.root",
                4096,
                true,
            ),
            (
                self.storage.local.share_root.as_ref(),
                "storage.local.shareRoot",
                4096,
                true,
            ),
            (
                self.storage.quark.cli_path.as_ref(),
                "storage.quark.cliPath",
                4096,
                false,
            ),
            (
                self.storage.quark.state_directory.as_ref(),
                "storage.quark.stateDirectory",
                4096,
                true,
            ),
            (
                self.storage.quark.recycle_directory.as_ref(),
                "storage.quark.recycleDirectory",
                4096,
                true,
            ),
            (
                self.storage.pan115.cli_path.as_ref(),
                "storage.pan115.cliPath",
                4096,
                false,
            ),
            (
                self.indexer.smartstrm.base_url.as_ref(),
                "indexer.smartstrm.baseUrl",
                4096,
                false,
            ),
            (
                self.indexer.smartstrm.task_name.as_ref(),
                "indexer.smartstrm.taskName",
                1024,
                false,
            ),
            (
                self.subtitles.open_subtitles_user_agent.as_ref(),
                "subtitles.openSubtitlesUserAgent",
                1024,
                true,
            ),
        ] {
            validate_string(value, key, max, required)?;
        }
        if self
            .database
            .driver
            .as_deref()
            .is_some_and(|value| !["sqlite", "mysql"].contains(&value))
        {
            return Err("invalid Drama Warden configuration field: database.driver".to_owned());
        }
        validate_range(self.database.mysql.port, 1, 65_535, "database.mysql.port")?;
        validate_range(
            self.database.mysql.connection_limit,
            1,
            100,
            "database.mysql.connectionLimit",
        )?;
        validate_range(
            self.database.mysql.connect_attempts,
            1,
            120,
            "database.mysql.connectAttempts",
        )?;
        validate_range(
            self.database.mysql.connect_delay_ms,
            0,
            10_000,
            "database.mysql.connectDelayMs",
        )?;
        validate_range(
            self.database.mysql.connect_timeout_ms,
            100,
            60_000,
            "database.mysql.connectTimeoutMs",
        )?;
        validate_range(
            self.storage.pan115.min_interval_ms,
            0,
            60_000,
            "storage.pan115.minIntervalMs",
        )?;
        validate_range(
            self.storage.pan115.waf_cooldown_ms,
            0,
            3_600_000,
            "storage.pan115.wafCooldownMs",
        )?;
        validate_range(
            self.automation.scan_interval_minutes,
            5,
            1440,
            "automation.scanIntervalMinutes",
        )?;
        if self.plugins.modules.len() > 100 {
            return Err("invalid Drama Warden configuration field: plugins.modules".to_owned());
        }
        let mut modules = HashSet::new();
        for module in &self.plugins.modules {
            if module.trim().is_empty()
                || module.len() > 500
                || module.contains(',')
                || module.contains('\0')
                || !modules.insert(module)
            {
                return Err("invalid Drama Warden configuration field: plugins.modules".to_owned());
            }
        }
        if self.plugins.commands.len() > 128
            || self.plugins.commands.iter().any(|plugin| {
                plugin.command.trim().is_empty()
                    || plugin.command.len() > 4096
                    || plugin.command.contains('\0')
                    || plugin.args.len() > 128
                    || plugin
                        .args
                        .iter()
                        .any(|arg| arg.len() > 8192 || arg.contains('\0'))
            })
        {
            return Err("invalid Drama Warden configuration field: plugins.commands".to_owned());
        }
        for plugin in &self.plugins.commands {
            validate_range(
                plugin.timeout_ms,
                100,
                120_000,
                "plugins.commands.timeoutMs",
            )?;
        }
        Ok(())
    }

    fn apply(&self) -> Result<(), String> {
        for (key, value) in [
            ("TZ", self.server.time_zone.as_deref().map(str::to_owned)),
            ("DB_DRIVER", self.database.driver.clone()),
            ("SQLITE_PATH", self.database.sqlite_path.clone()),
            (
                "SUBTITLE_WORKER_ENABLED",
                self.subtitles.worker_enabled.map(|v| v.to_string()),
            ),
            (
                "SUBTITLE_ASSRT_ENABLED",
                self.subtitles.assrt_enabled.map(|v| v.to_string()),
            ),
            (
                "OPENSUBTITLES_USER_AGENT",
                self.subtitles.open_subtitles_user_agent.clone(),
            ),
            (
                "FOLLOWUP_AUTOMATION_ENABLED",
                self.automation.enabled.map(|v| v.to_string()),
            ),
            (
                "FOLLOWUP_SCAN_INTERVAL_MINUTES",
                self.automation.scan_interval_minutes.map(|v| v.to_string()),
            ),
            (
                "FOLLOWUP_AUTO_PROCESS",
                self.automation.auto_process.map(|v| v.to_string()),
            ),
            (
                "DRAMA_WARDEN_PLUGINS",
                (!self.plugins.modules.is_empty()).then(|| self.plugins.modules.join(",")),
            ),
            (
                "SMARTSTRM_BASE_URL",
                self.indexer.smartstrm.base_url.clone(),
            ),
            (
                "SMARTSTRM_TASK_NAME",
                self.indexer.smartstrm.task_name.clone(),
            ),
            ("MYSQL_HOST", self.database.mysql.host.clone()),
            (
                "MYSQL_PORT",
                self.database.mysql.port.map(|v| v.to_string()),
            ),
            ("MYSQL_DATABASE", self.database.mysql.database.clone()),
            ("MYSQL_USER", self.database.mysql.user.clone()),
            (
                "MYSQL_CONNECTION_LIMIT",
                self.database.mysql.connection_limit.map(|v| v.to_string()),
            ),
            (
                "MYSQL_CONNECT_ATTEMPTS",
                self.database.mysql.connect_attempts.map(|v| v.to_string()),
            ),
            (
                "MYSQL_CONNECT_DELAY_MS",
                self.database.mysql.connect_delay_ms.map(|v| v.to_string()),
            ),
            (
                "MYSQL_CONNECT_TIMEOUT_MS",
                self.database
                    .mysql
                    .connect_timeout_ms
                    .map(|v| v.to_string()),
            ),
            (
                "LOCAL_STORAGE_ENABLED",
                self.storage.local.enabled.map(|v| v.to_string()),
            ),
            ("LOCAL_STORAGE_ROOT", self.storage.local.root.clone()),
            (
                "LOCAL_STORAGE_SHARE_ROOT",
                self.storage.local.share_root.clone(),
            ),
            ("QUARK_CLI_PATH", self.storage.quark.cli_path.clone()),
            (
                "QUARK_STATE_DIR",
                self.storage.quark.state_directory.clone(),
            ),
            (
                "QUARK_RECYCLE_DIR",
                self.storage.quark.recycle_directory.clone(),
            ),
            ("PAN115_CLI_PATH", self.storage.pan115.cli_path.clone()),
            (
                "PAN115_MIN_INTERVAL_MS",
                self.storage.pan115.min_interval_ms.map(|v| v.to_string()),
            ),
            (
                "PAN115_WAF_COOLDOWN_MS",
                self.storage.pan115.waf_cooldown_ms.map(|v| v.to_string()),
            ),
        ] {
            if let Some(value) = value {
                set_if_unset(key, &value);
            }
        }
        if !self.plugins.commands.is_empty() {
            let commands = serde_json::to_string(&self.plugins.commands)
                .map_err(|_| "could not serialize Drama Warden plugin configuration".to_owned())?;
            set_if_unset("DRAMA_WARDEN_PLUGIN_COMMANDS", &commands);
        }
        Ok(())
    }
}

fn set_if_unset(key: &str, value: &str) {
    let has_value =
        env::var_os(key).is_some_and(|current| !current.to_string_lossy().trim().is_empty());
    if !has_value {
        // Configuration is loaded synchronously before the Tokio runtime and any worker threads exist.
        unsafe { env::set_var(key, value) };
    }
}

fn select_config_path() -> Result<(PathBuf, bool), String> {
    let configured = env::var("DRAMA_WARDEN_CONFIG").unwrap_or_default();
    if !configured.trim().is_empty() {
        return Ok((PathBuf::from(configured), true));
    }
    let normal = PathBuf::from("config/config.yaml");
    if normal.exists() {
        return Ok((normal, false));
    }
    Ok((PathBuf::from("config/config.example.yaml"), false))
}

fn read_config(path: &Path, explicit: bool) -> Result<Option<String>, String> {
    if !path.exists() {
        return if explicit {
            Err("configured Drama Warden YAML file does not exist".to_owned())
        } else {
            Ok(None)
        };
    }
    let metadata = fs::metadata(path)
        .map_err(|_| "could not read the Drama Warden YAML configuration".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err("invalid Drama Warden YAML configuration file size".to_owned());
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|_| "could not read the Drama Warden YAML configuration".to_owned())
}

pub fn load() -> Result<(), String> {
    let (path, explicit) = select_config_path()?;
    let Some(source) = read_config(&path, explicit)? else {
        return Ok(());
    };
    let config: Config = serde_saphyr::from_str(&source)
        .map_err(|_| "could not parse the Drama Warden YAML configuration".to_owned())?;
    config.validate()?;
    config.apply()?;
    Ok(())
}
